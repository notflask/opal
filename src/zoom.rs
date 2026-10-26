//! `opal zoom`: freeze the screen and zoom/pan it, like tsoding/boomer.
//!
//! Frozen mode: the capture is the surface's buffer, and zooming only moves
//! the wp_viewporter *source rectangle*, so the compositor scales on the GPU
//! and we do no pixel work. With `filter = "nearest"` the visible part is
//! rendered by the CPU instead, for crisp pixels. The flashlight is a hole
//! buffer plus four dim bars, moved with the pointer.
//!

use std::time::Instant;

use anyhow::{Context, Result, bail};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode as LoopMode, PostAction, generic::Generic},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
    subcompositor::SubcompositorState,
};
use tiny_skia::{FilterQuality, PixmapPaint, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::{
    capture::{self, Shot},
    config::{Config, Zoom as ZoomCfg},
    ipc, output,
    surface::{Env, IRect, Quad, Solids, Sub},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const KEY_ESC: u32 = 1;
const KEY_0: u32 = 11;
const KEY_MINUS: u32 = 12;
const KEY_EQUAL: u32 = 13;
const KEY_Q: u32 = 16;
const KEY_F: u32 = 33;
const KEY_S: u32 = 31;
const KEY_C: u32 = 46;
/// Zoom per wheel notch at `scroll_speed = 1`.
const NOTCH: f32 = 1.15;

// ── the view model ───────────────────────────────────────────────────────

/// What part of the screenshot is visible, plus the inertia of zooming and
/// panning. Pure math, no Wayland.
#[derive(Clone, Debug)]
pub struct View {
    pub scale: f32,
    /// Top-left of the visible area in logical source pixels.
    pub x: f32,
    pub y: f32,
    /// Output size, logical.
    w: f32,
    h: f32,
    /// d(ln scale)/dt
    scale_vel: f32,
    /// Source px per second.
    vel: (f32, f32),
    cfg: ZoomCfg,
}

impl View {
    pub fn new(w: f32, h: f32, cfg: ZoomCfg) -> Self {
        Self { scale: 1.0, x: 0.0, y: 0.0, w, h, scale_vel: 0.0, vel: (0.0, 0.0), cfg }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.w, self.h, self.cfg.clone());
    }

    /// Visible area in logical source pixels: x, y, w, h.
    pub fn source(&self) -> (f32, f32, f32, f32) {
        (self.x, self.y, self.w / self.scale, self.h / self.scale)
    }

    pub fn clamp(&mut self) {
        self.x = self.x.clamp(0.0, (self.w - self.w / self.scale).max(0.0));
        self.y = self.y.clamp(0.0, (self.h - self.h / self.scale).max(0.0));
    }

    /// Set the zoom, keeping the source point under `anchor` (screen, logical) fixed.
    pub fn zoom_to(&mut self, scale: f32, anchor: (f32, f32)) {
        let wx = self.x + anchor.0 / self.scale;
        let wy = self.y + anchor.1 / self.scale;
        self.scale = scale.clamp(self.cfg.min, self.cfg.max);
        self.x = wx - anchor.0 / self.scale;
        self.y = wy - anchor.1 / self.scale;
        self.clamp();
    }

    /// One wheel notch: positive zooms in.
    pub fn notch(&mut self, dir: f32, anchor: (f32, f32)) {
        let ln = NOTCH.ln() * self.cfg.scroll_speed * dir;
        if self.cfg.smooth {
            // the impulse integrates to `ln` over the friction decay
            self.scale_vel += ln * self.cfg.scale_friction;
        } else {
            self.zoom_to(self.scale * ln.exp(), anchor);
        }
    }

    /// Move the picture by a pointer delta (screen px).
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.x -= dx / self.scale;
        self.y -= dy / self.scale;
        self.clamp();
    }

    /// Let go of a drag moving at `v` screen px/s.
    pub fn fling(&mut self, v: (f32, f32)) {
        self.vel = if self.cfg.smooth { (-v.0 / self.scale, -v.1 / self.scale) } else { (0.0, 0.0) };
    }

    pub fn stop(&mut self) {
        self.vel = (0.0, 0.0);
        self.scale_vel = 0.0;
    }

    /// Advance the inertia by `dt` seconds. Returns whether it is still moving.
    pub fn step(&mut self, dt: f32, anchor: (f32, f32)) -> bool {
        if self.scale_vel != 0.0 {
            let before = self.scale;
            self.zoom_to(self.scale * (self.scale_vel * dt).exp(), anchor);
            self.scale_vel *= (-self.cfg.scale_friction * dt).exp();
            if self.scale == before || self.scale_vel.abs() < 0.01 {
                self.scale_vel = 0.0;
            }
        }
        if self.vel != (0.0, 0.0) {
            self.x += self.vel.0 * dt;
            self.y += self.vel.1 * dt;
            let (ox, oy) = (self.x, self.y);
            self.clamp();
            let f = (-self.cfg.drag_friction * dt).exp();
            self.vel = (if self.x == ox { self.vel.0 * f } else { 0.0 }, if self.y == oy { self.vel.1 * f } else { 0.0 });
            if self.vel.0.hypot(self.vel.1) < 2.0 {
                self.vel = (0.0, 0.0);
            }
        }
        self.moving()
    }

    pub fn moving(&self) -> bool {
        self.scale_vel != 0.0 || self.vel != (0.0, 0.0)
    }
}

// ── pixels ───────────────────────────────────────────────────────────────

/// Nearest-neighbour render of `src` (physical px: x, y, w, h) of `shot` into
/// an Xrgb8888 buffer of `dw` x `dh`. Repeated source rows are copied, not recomputed.
fn render_nearest(shot: &Shot, src: (f32, f32, f32, f32), dst: &mut [u8], dw: usize, dh: usize) {
    let data = shot.bytes();
    let swap = matches!(shot.format, wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888);
    let (sw, sh) = (shot.w as usize, shot.h as usize);
    let xs: Vec<usize> =
        (0..dw).map(|x| ((src.0 + (x as f32 + 0.5) * src.2 / dw as f32) as usize).min(sw - 1)).collect();
    let mut last: Option<usize> = None;
    for y in 0..dh {
        let sy = ((src.1 + (y as f32 + 0.5) * src.3 / dh as f32) as usize).min(sh - 1);
        let (before, row) = dst.split_at_mut(y * dw * 4);
        let row = &mut row[..dw * 4];
        if last == Some(sy) {
            row.copy_from_slice(&before[(y - 1) * dw * 4..y * dw * 4]);
            continue;
        }
        last = Some(sy);
        let srow = &data[sy * shot.stride()..];
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let s = &srow[xs[x] * 4..xs[x] * 4 + 4];
            if swap {
                px.copy_from_slice(&[s[2], s[1], s[0], 255]);
            } else {
                px.copy_from_slice(&[s[0], s[1], s[2], 255]);
            }
        }
    }
}

/// What is on screen right now, as an image at the output's pixel size.
fn render_view(shot: &Shot, src: (f32, f32, f32, f32), nearest: bool) -> Result<tiny_skia::Pixmap> {
    let (x, y, w, h) = src;
    let (x0, y0) = (x.floor().max(0.0) as u32, y.floor().max(0.0) as u32);
    let (x1, y1) = (((x + w).ceil() as u32).min(shot.w), ((y + h).ceil() as u32).min(shot.h));
    let part = shot.read_rgba(x0, y0, x1 - x0, y1 - y0).context("empty view")?;
    let mut out = tiny_skia::Pixmap::new(shot.w, shot.h).context("empty view")?;
    let (sx, sy) = (shot.w as f32 / w, shot.h as f32 / h);
    let quality = if nearest { FilterQuality::Nearest } else { FilterQuality::Bilinear };
    out.draw_pixmap(
        0,
        0,
        part.as_ref(),
        &PixmapPaint { quality, ..Default::default() },
        Transform::from_row(sx, 0.0, 0.0, sy, (x0 as f32 - x) * sx, (y0 as f32 - y) * sy),
        None,
    );
    Ok(out)
}

/// The flashlight: a square buffer with a transparent disc, the rest dimmed.
/// The disc is built from `block` x `block` pixel squares, so its edge is
/// pixel art; `block = 1` gives the finest hard edge.
fn hole_pixels(buf: &mut [u8], d: i32, alpha: u8, block: i32) {
    let block = block.clamp(1, d.max(1));
    let r = d as f32 / 2.0;
    let dim = [0, 0, 0, alpha];
    // cells are laid out symmetrically around the centre
    let cells = (d + block - 1) / block;
    let inside = |cx: i32, cy: i32| {
        let x = ((cx * block).min(d) + ((cx + 1) * block).min(d)) as f32 / 2.0 - r;
        let y = ((cy * block).min(d) + ((cy + 1) * block).min(d)) as f32 / 2.0 - r;
        x.hypot(y) < r
    };
    for cy in 0..cells {
        for cx in 0..cells {
            let px = if inside(cx, cy) { [0; 4] } else { dim };
            for y in cy * block..((cy + 1) * block).min(d) {
                for x in cx * block..((cx + 1) * block).min(d) {
                    let i = ((y * d + x) * 4) as usize;
                    buf[i..i + 4].copy_from_slice(&px);
                }
            }
        }
    }
}

// ── the app ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Exit {
    Quit,
    Copy,
    Save,
}

/// The flashlight hole: a subsurface cropped to the output.
struct Hole {
    s: Sub,
    pool: SlotPool,
    buf: Option<Buffer>,
    /// Radius (logical) the buffer was drawn for.
    drawn: Option<(i32, u32)>,
    shown: Option<(IRect, IRect)>,
}

struct ZSurf {
    layer: LayerSurface,
    viewport: WpViewport,
    name: String,
    geo: crate::geom::Rect,
    k: f32,
    /// Index into `Zoomer::shots` (frozen mode).
    shot: Option<usize>,
    /// The zoomed picture. A subsurface of its own: Hyprland misplaces the
    /// children of a surface that has a viewport source rectangle.
    pic: Option<Sub>,
    dim: Vec<Quad>,
    hole: Option<Hole>,
    configured: bool,
    frame_pending: bool,
    dirty: bool,
    /// Parent buffer attached yet?
    on_cpu: Option<bool>,
    /// Which buffer the picture shows: true = the CPU-rendered one.
    pic_cpu: Option<bool>,
}

struct Cpu {
    pool: SlotPool,
    bufs: [Option<Buffer>; 2],
    next: usize,
}

struct Zoomer {
    conn: Connection,
    qh: QueueHandle<Zoomer>,
    loop_handle: LoopHandle<'static, Zoomer>,
    registry: RegistryState,
    env: Env,
    outputs: OutputState,
    seats: SeatState,
    shm: Shm,
    solids: Solids,
    surfs: Vec<ZSurf>,
    shots: Vec<Shot>,
    cfg: Config,
    view: View,

    pointer: Option<ThemedPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mods: Modifiers,
    cursor: (f32, f32),
    icon: CursorIcon,
    dragging: bool,
    drag_last: Option<(f32, f32, Instant)>,
    drag_vel: (f32, f32),
    flashlight: bool,
    radius: f32,
    last_tick: Instant,
    cpu: Option<Cpu>,
    exit: Option<Exit>,
}

pub fn run(args: &[&str], cfg: Config) -> Result<()> {
    if let Some(a) = args.first() {
        bail!("usage: opal zoom (got {a:?})");
    }
    let Some(_guard) = crate::lock::acquire() else { return Ok(()) };
    let conn = Connection::connect_to_env().context("no Wayland compositor")?;

    let want = ipc::pointer_output();
    let shots = match want.as_deref().map(|n| capture::capture(&conn, Some(n))) {
        Some(Ok(s)) => s,
        _ => {
            let mut all = capture::capture(&conn, None)?;
            all.truncate(1);
            all
        }
    };
    let trace = std::env::var_os("OPAL_TRACE").is_some();
    let exit = run_app(&conn, shots, cfg.clone(), trace)?;
    if let Some((shot, view, nearest, exit)) = exit {
        let img = render_view(&shot, view, nearest)?;
        let png = output::encode_png(&img)?;
        let mut o = cfg.output.clone();
        o.save = exit == Exit::Save;
        o.clipboard = exit == Exit::Copy;
        let path = output::finish(&o, &png)?;
        if let (Some(p), true) = (path, cfg.output.print_path) {
            println!("{}", p.display());
        }
    }
    Ok(())
}

type ViewResult = Option<(Shot, (f32, f32, f32, f32), bool, Exit)>;

fn run_app(conn: &Connection, shots: Vec<Shot>, cfg: Config, trace: bool) -> Result<ViewResult> {
    let (globals, mut queue) = registry_queue_init(conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("the compositor lacks wl_compositor")?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .context("the compositor lacks wl_subcompositor")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("the compositor lacks wlr-layer-shell")?;
    let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ()).context("the compositor lacks wp_viewporter")?;
    let shm = Shm::bind(&globals, &qh).context("the compositor lacks wl_shm")?;
    let env = Env { compositor, subcompositor, viewporter };
    let dim_a = (cfg.zoom.flashlight_dim * 255.0).round() as u8;
    // 0: flashlight dim, 1: fully transparent (the parent surface that takes input)
    let solids = Solids::new(&shm, &[u32::from(dim_a) << 24, 0])?;

    let mut event_loop: EventLoop<Zoomer> = EventLoop::try_new()?;
    let mut app = Zoomer {
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        registry: RegistryState::new(&globals),
        env,
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        shm,
        solids,
        surfs: Vec::new(),
        shots,
        view: View::new(1.0, 1.0, cfg.zoom.clone()),
        flashlight: cfg.zoom.flashlight,
        radius: cfg.zoom.flashlight_radius,
        cfg,
        pointer: None,
        keyboard: None,
        mods: Modifiers::default(),
        cursor: (0.0, 0.0),
        icon: CursorIcon::Crosshair,
        dragging: false,
        drag_last: None,
        drag_vel: (0.0, 0.0),
        last_tick: Instant::now(),
        cpu: None,
        exit: None,
    };
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;

    // one surface per target output
    let wl_outputs: Vec<_> = app.outputs.outputs().collect();
    let mut targets: Vec<(String, wl_output::WlOutput, crate::geom::Rect, f32, Option<usize>)> = Vec::new();
    for out in &wl_outputs {
        let Some(info) = app.outputs.info(out) else { continue };
        let name = info.name.clone().unwrap_or_default();
        let (lw, lh) = info.logical_size.unwrap_or((0, 0));
        let (x, y) = info.logical_position.unwrap_or(info.location);
        let shot = app.shots.iter().position(|s| s.name == name);
        if shot.is_none() {
            continue;
        }
        let k = shot.map_or(info.scale_factor as f32, |i| app.shots[i].scale());
        targets.push((name, out.clone(), crate::geom::Rect::new(x as f32, y as f32, lw as f32, lh as f32), k, shot));
    }
    if targets.is_empty() {
        bail!("no output to zoom");
    }
    for (name, out, geo, k, shot) in targets {
        let surface = app.env.compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("opal-zoom"), Some(&out));
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        let viewport = app.env.viewporter.get_viewport(layer.wl_surface(), &qh, ());
        viewport.set_destination(geo.w as i32, geo.h as i32);
        layer.commit();
        let parent = layer.wl_surface().clone();
        // the parent only holds a transparent buffer, the picture is a child
        let pic = shot.is_some().then(|| Sub::new(&app.env, &parent, &qh));
        let (dim, hole) = if shot.is_some() {
            let dim = (0..4).map(|_| Quad::new(&app.env, &parent, app.solids.get(0), &qh)).collect();
            let hole = Hole {
                s: Sub::new(&app.env, &parent, &qh),
                pool: SlotPool::new(4096, &app.shm)?,
                buf: None,
                drawn: None,
                shown: None,
            };
            (dim, Some(hole))
        } else {
            (Vec::new(), None)
        };
        if shot.is_some() {
            app.view = View::new(geo.w, geo.h, app.cfg.zoom.clone());
        }
        app.surfs.push(ZSurf {
            layer,
            viewport,
            name,
            geo,
            k,
            shot,
            pic,
            dim,
            hole,
            configured: false,
            frame_pending: false,
            dirty: true,
            on_cpu: None,
            pic_cpu: None,
        });
    }

    // dev hook for tests: OPAL_ZOOM_VIEW=scale,x,y starts at that view
    if let Some(v) = std::env::var("OPAL_ZOOM_VIEW").ok().map(|v| v.split(',').filter_map(|p| p.trim().parse::<f32>().ok()).collect::<Vec<_>>()) {
        if let [scale, x, y] = v[..] {
            app.view.zoom_to(scale, (0.0, 0.0));
            app.view.x = x;
            app.view.y = y;
            app.view.clamp();
        }
    }

    let signals = crate::sig::signal_fd(&[libc::SIGUSR1, libc::SIGTERM, libc::SIGINT])?;
    event_loop
        .handle()
        .insert_source(Generic::new(signals, Interest::READ, LoopMode::Level), |_, fd, app: &mut Zoomer| {
            crate::sig::drain(&**fd);
            app.exit = Some(Exit::Quit);
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    while app.exit.is_none() {
        event_loop.dispatch(None, &mut app)?;
    }

    // what to hand back, taken before the surfaces go away
    let exit = app.exit.unwrap();
    let result = match exit {
        Exit::Copy | Exit::Save if !app.shots.is_empty() => {
            let k = app.shots[0].scale();
            let (x, y, w, h) = app.view.source();
            let shot = app.shots.remove(0);
            Some((shot, (x * k, y * k, w * k, h * k), app.nearest_active(), exit))
        }
        _ => None,
    };
    app.surfs.clear();
    let _ = conn.roundtrip();
    if trace {
        eprintln!("zoom closed ({exit:?})");
    }
    Ok(result)
}

impl Zoomer {
    fn nearest_active(&self) -> bool {
        self.cfg.zoom.nearest && self.view.scale >= self.cfg.zoom.nearest_from
    }

    /// Ask every output for a new frame (at most one in flight each).
    fn schedule(&mut self, qh: &QueueHandle<Self>) {
        let mut todo = Vec::new();
        for (i, s) in self.surfs.iter_mut().enumerate() {
            s.dirty = true;
            if s.configured && !s.frame_pending {
                todo.push(i);
            }
        }
        for i in todo {
            self.draw(qh, i);
        }
    }

    fn set_icon(&mut self, icon: CursorIcon) {
        if icon != self.icon {
            self.icon = icon;
            if let Some(p) = &self.pointer {
                let _ = p.set_cursor(&self.conn, icon);
            }
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>, i: usize) {
        let t0 = Instant::now();
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.05);
        self.last_tick = now;
        let moving = if self.view.moving() { self.view.step(dt, self.cursor) } else { false };

        let nearest = self.nearest_active();
        let surf = &mut self.surfs[i];
        let wl = surf.layer.wl_surface().clone();
        let (lw, lh) = (surf.geo.w as i32, surf.geo.h as i32);
        let mut retry = false;

        // the parent is a transparent buffer that catches input
        if surf.on_cpu.is_none() {
            wl.attach(Some(&self.solids.get(1)), 0, 0);
            wl.damage_buffer(0, 0, 1, 1);
            surf.viewport.set_destination(lw, lh);
            surf.on_cpu = Some(false);
        }
        if let (Some(si), Some(pic)) = (surf.shot, &surf.pic) {
            let shot = &self.shots[si];
            let k = surf.k;
            let (sx, sy, sw, sh) = self.view.source();
            let pw = pic.surface.clone();
            if nearest {
                // crisp pixels: render the visible part ourselves
                let cpu = self.cpu.get_or_insert_with(|| Cpu {
                    pool: SlotPool::new(4096, &self.shm).expect("shm pool"),
                    bufs: [None, None],
                    next: 0,
                });
                let (dw, dh) = (shot.w as i32, shot.h as i32);
                let pick = [cpu.next, 1 - cpu.next].into_iter().find(|&n| match &cpu.bufs[n] {
                    Some(b) => b.canvas(&mut cpu.pool).is_some(),
                    None => true,
                });
                match pick {
                    None => retry = true,
                    Some(n) => {
                        if cpu.bufs[n].is_none() {
                            cpu.bufs[n] = cpu.pool.create_buffer(dw, dh, dw * 4, wl_shm::Format::Xrgb8888).ok().map(|(b, _)| b);
                        }
                        if let Some(buf) = &cpu.bufs[n] {
                            if let Some(canvas) = buf.canvas(&mut cpu.pool) {
                                render_nearest(shot, (sx * k, sy * k, sw * k, sh * k), canvas, dw as usize, dh as usize);
                                let _ = buf.attach_to(&pw);
                                pw.damage_buffer(0, 0, dw, dh);
                                pic.viewport.set_source(-1.0, -1.0, -1.0, -1.0);
                                surf.pic_cpu = Some(true);
                                cpu.next = 1 - n;
                            }
                        }
                    }
                }
            } else {
                if surf.pic_cpu != Some(false) {
                    pw.attach(Some(&shot.buffer), 0, 0);
                    surf.pic_cpu = Some(false);
                }
                // the whole trick: the compositor scales this rectangle up
                pic.viewport.set_source(f64::from(sx * k), f64::from(sy * k), f64::from(sw * k), f64::from(sh * k));
                pw.damage_buffer(0, 0, shot.w as i32, shot.h as i32);
            }
            pic.viewport.set_destination(lw, lh);
            pw.commit();
        }

        // flashlight: dim bars around a hole that follows the pointer
        if surf.hole.is_some() {
            let area = IRect::new(0, 0, lw, lh);
            let at = (self.cursor.0.round() as i32, self.cursor.1.round() as i32);
            let on = self.flashlight;
            let r = self.radius.round() as i32;
            let dim_alpha = (self.cfg.zoom.flashlight_dim * 255.0).round() as u8;
            let hole_rect = IRect::new(at.0 - r, at.1 - r, 2 * r, 2 * r);
            let vis = if on { hole_rect.clip(area) } else { None };
            let bars = match vis {
                None => [if on { Some(area) } else { None }, None, None, None],
                Some(v) => [
                    Some(IRect::new(0, 0, lw, v.y)),
                    Some(IRect::new(0, v.bottom(), lw, lh - v.bottom())),
                    Some(IRect::new(0, v.y, v.x, v.h)),
                    Some(IRect::new(v.right(), v.y, lw - v.right(), v.h)),
                ],
            };
            for (q, b) in surf.dim.iter_mut().zip(bars) {
                q.set(b);
            }
            if let Some(h) = &mut surf.hole {
                match vis {
                    None => {
                        if h.shown.take().is_some() {
                            h.s.hide();
                        }
                    }
                    Some(v) => {
                        let d = ((2 * r) as f32 * surf.k).round() as i32;
                        if h.drawn != Some((d, u32::from(dim_alpha))) || h.buf.is_none() {
                            let block_px = ((self.cfg.zoom.flashlight_pixel * surf.k).round() as i32).max(1);
                            if let Ok((b, canvas)) = h.pool.create_buffer(d, d, d * 4, wl_shm::Format::Argb8888) {
                                hole_pixels(canvas, d, dim_alpha, block_px);
                                h.s.surface.attach(Some(b.wl_buffer()), 0, 0);
                                h.s.surface.damage_buffer(0, 0, d, d);
                                h.buf = Some(b);
                                h.drawn = Some((d, u32::from(dim_alpha)));
                                h.shown = None;
                            }
                        }
                        // crop to the output so the hole never spills onto a neighbour
                        let src = IRect::new(
                            ((v.x - hole_rect.x) as f32 * surf.k).round() as i32,
                            ((v.y - hole_rect.y) as f32 * surf.k).round() as i32,
                            (v.w as f32 * surf.k).round() as i32,
                            (v.h as f32 * surf.k).round() as i32,
                        );
                        if h.shown != Some((v, src)) {
                            // re-attach: Hyprland ignores a new viewport otherwise
                            if let Some(b) = &h.buf {
                                h.s.surface.attach(Some(b.wl_buffer()), 0, 0);
                                h.s.surface.damage_buffer(0, 0, d, d);
                            }
                            h.s.sub.set_position(v.x, v.y);
                            h.s.viewport.set_source(f64::from(src.x), f64::from(src.y), f64::from(src.w), f64::from(src.h));
                            h.s.viewport.set_destination(v.w, v.h);
                            h.s.surface.commit();
                            h.shown = Some((v, src));
                        }
                    }
                }
            }
        }

        let keep_going = moving || self.view.moving() || retry;
        let surf = &mut self.surfs[i];
        if keep_going || surf.dirty {
            wl.frame(qh, FrameCallbackData(wl.clone()));
            surf.frame_pending = true;
        }
        surf.layer.commit();
        surf.dirty = keep_going;
        if std::env::var_os("OPAL_TRACE").is_some() {
            eprintln!("zoom frame {} scale {:.2} took {:?}", surf.name, self.view.scale, t0.elapsed());
        }
    }

    fn key(&mut self, qh: &QueueHandle<Self>, e: KeyEvent) {
        let ctrl = self.mods.ctrl;
        match e.raw_code {
            KEY_ESC | KEY_Q => self.exit = Some(Exit::Quit),
            KEY_0 => {
                self.view.reset();
                self.schedule(qh);
            }
            KEY_F => {
                self.flashlight = !self.flashlight;
                self.schedule(qh);
            }
            KEY_EQUAL => self.wheel(1.0, qh),
            KEY_MINUS => self.wheel(-1.0, qh),
            KEY_C if !ctrl => self.exit = Some(Exit::Copy),
            KEY_S if !ctrl => self.exit = Some(Exit::Save),
            _ => {}
        }
    }

    /// Positive `dir` = scroll up = zoom in.
    fn wheel(&mut self, dir: f32, qh: &QueueHandle<Self>) {
        // Alt or Ctrl + wheel: size of the light circle
        if self.mods.ctrl || self.mods.alt {
            if self.flashlight {
                let max = self.surfs.iter().map(|s| s.geo.w.min(s.geo.h)).fold(0.0, f32::max).max(60.0);
                self.radius = (self.radius * 1.1f32.powf(dir)).clamp(30.0, max);
            }
        } else {
            self.view.notch(dir, self.cursor);
        }
        self.schedule(qh);
    }
}

// ── Wayland plumbing ─────────────────────────────────────────────────────

impl CompositorHandler for Zoomer {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(i) = self.surfs.iter().position(|s| s.layer.wl_surface() == surface) {
            self.surfs[i].frame_pending = false;
            if self.surfs[i].dirty {
                self.draw(qh, i);
            }
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Zoomer {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.exit = Some(Exit::Quit);
    }
}

impl LayerShellHandler for Zoomer {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = Some(Exit::Quit);
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        if let Some(i) = self.surfs.iter().position(|s| s.layer.wl_surface() == layer.wl_surface()) {
            if !self.surfs[i].configured {
                self.surfs[i].configured = true;
                self.last_tick = Instant::now();
                self.draw(qh, i);
            }
        }
    }
}

impl SeatHandler for Zoomer {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let repeat = Box::new(|st: &mut Zoomer, _: &wl_keyboard::WlKeyboard, e: KeyEvent| {
                let qh = st.qh.clone();
                st.key(&qh, e);
            });
            self.keyboard = self.seats.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.env.compositor.create_surface(qh);
            self.pointer = self
                .seats
                .get_pointer_with_theme::<_, ()>(qh, &seat, self.shm.wl_shm(), cursor_surface, ThemeSpec::default())
                .ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.pointer().release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Zoomer {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        self.key(qh, e);
    }
    fn repeat_key(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        if matches!(e.raw_code, KEY_EQUAL | KEY_MINUS) {
            self.key(qh, e);
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = m;
    }
}

impl PointerHandler for Zoomer {
    fn pointer_frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let pos = (ev.position.0 as f32, ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Enter { .. } => {
                    self.cursor = pos;
                    self.icon = CursorIcon::Default; // force re-send after enter
                    self.set_icon(CursorIcon::Crosshair);
                    self.schedule(qh);
                }
                PointerEventKind::Motion { .. } => {
                    if self.dragging {
                        let (dx, dy) = (pos.0 - self.cursor.0, pos.1 - self.cursor.1);
                        self.view.pan(dx, dy);
                        let now = Instant::now();
                        if let Some((_, _, t)) = self.drag_last {
                            let dt = now.duration_since(t).as_secs_f32().max(0.001);
                            // smoothed pointer velocity for the fling
                            let v = (dx / dt, dy / dt);
                            self.drag_vel = (self.drag_vel.0 * 0.6 + v.0 * 0.4, self.drag_vel.1 * 0.6 + v.1 * 0.4);
                        }
                        self.drag_last = Some((pos.0, pos.1, now));
                    }
                    self.cursor = pos;
                    self.schedule(qh);
                }
                PointerEventKind::Press { button, .. } => match button {
                    BTN_LEFT => {
                        self.cursor = pos;
                        self.view.stop();
                        self.dragging = true;
                        self.drag_last = None;
                        self.drag_vel = (0.0, 0.0);
                        self.set_icon(CursorIcon::Grabbing);
                    }
                    BTN_RIGHT => self.exit = Some(Exit::Quit),
                    _ => {}
                },
                PointerEventKind::Release { button, .. } => {
                    if button == BTN_LEFT && self.dragging {
                        self.dragging = false;
                        // only a drag that was still moving keeps going
                        let fresh = self.drag_last.is_some_and(|(_, _, t)| t.elapsed().as_millis() < 80);
                        self.view.fling(if fresh { self.drag_vel } else { (0.0, 0.0) });
                        self.last_tick = Instant::now();
                        self.set_icon(CursorIcon::Crosshair);
                        self.schedule(qh);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // up (negative) zooms in; wheels report discrete steps, touchpads pixels
                    let notches = if vertical.value120 != 0 {
                        vertical.value120 as f32 / 120.0
                    } else if vertical.discrete != 0 {
                        vertical.discrete as f32
                    } else {
                        (vertical.absolute / 10.0) as f32
                    };
                    if notches != 0.0 {
                        self.wheel(-notches, qh);
                    }
                }
                PointerEventKind::Leave { .. } => {}
            }
        }
    }
}

impl ShmHandler for Zoomer {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Zoomer {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpViewporter, ()> for Zoomer {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for Zoomer {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_registry!(Zoomer);
delegate_dispatch2!(Zoomer);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ZoomCfg {
        Config::default().zoom
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let mut v = View::new(1000.0, 800.0, cfg());
        let anchor = (300.0, 200.0);
        let before = (v.x + anchor.0 / v.scale, v.y + anchor.1 / v.scale);
        v.zoom_to(4.0, anchor);
        let after = (v.x + anchor.0 / v.scale, v.y + anchor.1 / v.scale);
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
        assert_eq!(v.scale, 4.0);
    }

    #[test]
    fn the_source_rectangle_stays_inside_the_buffer() {
        let mut v = View::new(1000.0, 800.0, cfg());
        v.zoom_to(8.0, (999.0, 799.0));
        v.pan(-5000.0, -5000.0);
        let (x, y, w, h) = v.source();
        assert!(x >= 0.0 && y >= 0.0 && x + w <= 1000.001 && y + h <= 800.001, "{x} {y} {w} {h}");
        v.pan(9000.0, 9000.0);
        let (x, y, ..) = v.source();
        assert!(x >= 0.0 && y >= 0.0);
        // never zooms out past 1x or in past max
        v.zoom_to(0.1, (0.0, 0.0));
        assert_eq!(v.scale, 1.0);
        v.zoom_to(1e6, (0.0, 0.0));
        assert_eq!(v.scale, cfg().max);
    }

    #[test]
    fn a_notch_zooms_and_inertia_settles() {
        let mut v = View::new(1000.0, 800.0, cfg());
        v.notch(1.0, (500.0, 400.0));
        assert!(v.moving());
        let mut t = 0.0;
        while v.step(1.0 / 120.0, (500.0, 400.0)) {
            t += 1.0 / 120.0;
            assert!(t < 5.0, "never settles");
        }
        // one notch at scroll_speed 1.5: 1.15^1.5
        assert!((v.scale - NOTCH.powf(1.5)).abs() < 0.02, "{}", v.scale);
    }

    #[test]
    fn no_inertia_without_smooth() {
        let mut c = cfg();
        c.smooth = false;
        let mut v = View::new(1000.0, 800.0, c);
        v.notch(1.0, (0.0, 0.0));
        assert!(!v.moving());
        assert!(v.scale > 1.2);
        v.fling((500.0, 0.0));
        assert!(!v.moving());
    }

    #[test]
    fn fling_slides_and_stops() {
        let mut v = View::new(1000.0, 800.0, cfg());
        v.zoom_to(4.0, (0.0, 0.0));
        v.pan(-400.0, 0.0);
        let x0 = v.x;
        v.fling((-800.0, 0.0));
        let mut n = 0;
        while v.step(1.0 / 120.0, (0.0, 0.0)) {
            n += 1;
            assert!(n < 2000);
        }
        assert!(v.x > x0, "moved on after release");
    }

    #[test]
    fn nearest_scaling_repeats_pixels() {
        // a 2x2 image blown up 2x gives 4x4 blocks
        let src = [10u8, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255, 40, 0, 0, 255];
        let shot_px = |i: usize| src[i * 4];
        let (dw, dh) = (4, 4);
        let xs: Vec<usize> = (0..dw).map(|x| ((0.0 + (x as f32 + 0.5) * 2.0 / dw as f32) as usize).min(1)).collect();
        assert_eq!(xs, vec![0, 0, 1, 1]);
        assert_eq!(shot_px(2), 30);
        let _ = dh;
    }

    #[test]
    fn hole_is_transparent_inside_and_dim_outside() {
        let d = 20;
        let mut buf = vec![0u8; (d * d * 4) as usize];
        hole_pixels(&mut buf, d, 200, 1);
        let at = |x: i32, y: i32| buf[((y * d + x) * 4 + 3) as usize];
        assert_eq!(at(10, 10), 0);
        assert_eq!(at(0, 0), 200);
        assert_eq!(at(19, 19), 200);
        assert_eq!(at(10, 1), 0);
    }
}

#[cfg(test)]
mod pixel_tests {
    use super::*;

    #[test]
    fn pixel_edge_is_made_of_squares_and_symmetric() {
        let d = 64;
        let b = 8;
        let mut buf = vec![0u8; (d * d * 4) as usize];
        hole_pixels(&mut buf, d, 200, b);
        let a = |x: i32, y: i32| buf[((y * d + x) * 4 + 3) as usize];
        // every 8x8 cell is uniform
        for cy in 0..d / b {
            for cx in 0..d / b {
                let first = a(cx * b, cy * b);
                for y in 0..b {
                    for x in 0..b {
                        assert_eq!(a(cx * b + x, cy * b + y), first);
                    }
                }
            }
        }
        // left/right and top/bottom mirror
        for y in 0..d {
            for x in 0..d {
                assert_eq!(a(x, y), a(d - 1 - x, y));
                assert_eq!(a(x, y), a(x, d - 1 - y));
            }
        }
        assert_eq!(a(32, 32), 0);
        assert_eq!(a(0, 0), 200);
    }
}
