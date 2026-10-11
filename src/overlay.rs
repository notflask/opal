//! The frozen-screen overlay: one layer-shell surface per output showing the
//! capture, dimmed, with a bright selection, annotations, a glass toolbar,
//! and all input handling.

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    reexports::{
        calloop::{EventLoop, LoopHandle},
        calloop_wayland_source::WaylandSource,
    },
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, PixmapMut, Stroke, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::{
    annot::{self, COLORS, Ctx, History, Item, Kind, Style, Tool, WIDTHS},
    capture::Shot,
    geom::{Pt, Rect},
    glass,
    text::Fonts,
    theme::Theme,
    toolbar::{self, Action, Toolbar},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// How close (logical px) the pointer must be to a border to grab it.
const GRAB: f32 = 8.0;

pub enum Outcome {
    Cancel,
    Done { rect: Rect, items: Vec<Item> },
}

pub struct Finished {
    pub outcome: Outcome,
    pub shots: Vec<Shot>,
    pub fonts: Fonts,
}

pub struct Options {
    /// Selection that exists from the start (`output`, `all`).
    pub preset: Option<Rect>,
    /// Finish as soon as a drag ends (`region --no-edit`).
    pub finish_on_release: bool,
    /// Where the hint is shown (focused output name).
    pub hint_output: Option<String>,
    pub canvas_scale: f32,
}

struct Surf {
    layer: LayerSurface,
    _viewport: WpViewport,
    name: String,
    /// Index into `Overlay::shots`.
    shot: usize,
    /// Logical rectangle in compositor space.
    geo: Rect,
    /// Physical pixels per logical pixel.
    k: f32,
    dim: Pixmap,
    scratch: Option<Pixmap>,
    pool: SlotPool,
    configured: bool,
    frame_pending: bool,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Handle {
    dx: i8,
    dy: i8,
}

enum Drag {
    None,
    New { anchor: Pt },
    Move { grab: Pt, orig: Rect },
    Resize { handle: Handle, orig: Rect },
    Draw,
}

struct Overlay {
    conn: Connection,
    qh: QueueHandle<Overlay>,
    loop_handle: LoopHandle<'static, Overlay>,
    registry: RegistryState,
    compositor: CompositorState,
    outputs: OutputState,
    seats: SeatState,
    shm: Shm,
    surfs: Vec<Surf>,
    shots: Vec<Shot>,
    format: wl_shm::Format,
    theme: Theme,
    fonts: Fonts,
    opts: Options,
    bounds: Rect,

    pointer: Option<ThemedPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    mods: Modifiers,
    space: bool,
    cursor: Pt,
    icon: CursorIcon,

    sel: Option<Rect>,
    drag: Drag,
    outcome: Option<Outcome>,

    tool: Tool,
    color: usize,
    width: f32,
    history: History,
    /// Shape being drawn, or text being typed.
    current: Option<Item>,
    typing: bool,
    hover: Option<Action>,
}

pub fn run(shots: Vec<Shot>, theme: Theme, fonts: Fonts, opts: Options) -> Result<Finished> {
    let conn = Connection::connect_to_env().context("no Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("no wl_compositor")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("compositor lacks layer-shell")?;
    let viewporter: WpViewporter =
        globals.bind(&qh, 1..=1, ()).context("compositor lacks wp_viewporter")?;
    let shm = Shm::bind(&globals, &qh).context("no wl_shm")?;

    let mut event_loop: EventLoop<Overlay> = EventLoop::try_new()?;
    let mut app = Overlay {
        conn: conn.clone(),
        qh: qh.clone(),
        loop_handle: event_loop.handle(),
        registry: RegistryState::new(&globals),
        compositor,
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        shm,
        surfs: Vec::new(),
        shots,
        format: wl_shm::Format::Xrgb8888,
        theme,
        fonts,
        sel: opts.preset.map(|r| r.rounded()),
        opts,
        bounds: Rect::default(),
        pointer: None,
        keyboard: None,
        mods: Modifiers::default(),
        space: false,
        cursor: Pt::default(),
        icon: CursorIcon::Crosshair,
        drag: Drag::None,
        outcome: None,
        tool: Tool::Arrow,
        color: 0,
        width: WIDTHS[1],
        history: History::default(),
        current: None,
        typing: false,
        hover: None,
    };
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;
    if app.shm.formats().contains(&wl_shm::Format::Xbgr8888) {
        // same byte order as tiny-skia: no per-frame channel swap
        app.format = wl_shm::Format::Xbgr8888;
    }

    let wl_outputs: Vec<_> = app.outputs.outputs().collect();
    for idx in 0..app.shots.len() {
        let shot = &app.shots[idx];
        let out = wl_outputs
            .iter()
            .find(|o| {
                app.outputs.info(o).and_then(|i| i.name).as_deref() == Some(shot.name.as_str())
            })
            .with_context(|| format!("output {} vanished", shot.name))?;
        let surface = app.compositor.create_surface(&qh);
        let layer =
            layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("opal"), Some(out));
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        let viewport = viewporter.get_viewport(layer.wl_surface(), &qh, ());
        viewport.set_destination(shot.lw, shot.lh);
        layer.commit();

        let (pw, ph) = (shot.pixmap.width(), shot.pixmap.height());
        let mut dim = shot.pixmap.clone();
        dim_pixmap(&mut dim, 0.42);
        let scratch =
            (app.format != wl_shm::Format::Xbgr8888).then(|| Pixmap::new(pw, ph)).flatten();
        let surf = Surf {
            layer,
            _viewport: viewport,
            name: shot.name.clone(),
            shot: idx,
            geo: Rect::new(shot.x as f32, shot.y as f32, shot.lw as f32, shot.lh as f32),
            k: shot.scale(),
            dim,
            scratch,
            pool: SlotPool::new((pw * ph * 4) as usize * 2, &app.shm)?,
            configured: false,
            frame_pending: false,
            dirty: true,
        };
        app.surfs.push(surf);
    }
    app.bounds = app.surfs.iter().map(|s| s.geo).reduce(|a, b| a.union(&b)).unwrap_or_default();

    // the event loop gives us timers for key repeat
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    while app.outcome.is_none() {
        event_loop.dispatch(None, &mut app)?;
    }
    // destroy the overlay before anything else touches the screen/clipboard
    app.surfs.clear();
    let _ = conn.roundtrip();
    Ok(Finished {
        outcome: app.outcome.take().unwrap(),
        shots: std::mem::take(&mut app.shots),
        fonts: std::mem::replace(&mut app.fonts, Fonts::empty()),
    })
}

fn dim_pixmap(pm: &mut Pixmap, amount: f32) {
    let keep = ((1.0 - amount) * 256.0) as u32;
    for px in pm.data_mut().chunks_exact_mut(4) {
        for c in &mut px[..3] {
            *c = ((*c as u32 * keep) >> 8) as u8;
        }
    }
}

fn handle_at(sel: Rect, p: Pt) -> Option<Handle> {
    if !sel.inflate(GRAB).contains(p) {
        return None;
    }
    let near = |a: f32, b: f32| (a - b).abs() <= GRAB;
    let axis = |v: f32, lo: f32, hi: f32| {
        if near(v, lo) && (!near(v, hi) || (v - lo).abs() <= (v - hi).abs()) {
            -1
        } else if near(v, hi) {
            1
        } else {
            0
        }
    };
    let (dx, dy) = (axis(p.x, sel.x, sel.right()), axis(p.y, sel.y, sel.bottom()));
    (dx != 0 || dy != 0).then_some(Handle { dx, dy })
}

fn handle_icon(h: Handle) -> CursorIcon {
    match (h.dx, h.dy) {
        (-1, -1) => CursorIcon::NwResize,
        (1, -1) => CursorIcon::NeResize,
        (-1, 1) => CursorIcon::SwResize,
        (1, 1) => CursorIcon::SeResize,
        (0, _) => CursorIcon::NsResize,
        _ => CursorIcon::EwResize,
    }
}

/// Letter keys by evdev code, so shortcuts work on any keyboard layout.
fn tool_for_key(code: u32) -> Option<Tool> {
    Some(match code {
        30 => Tool::Arrow,       // A
        38 => Tool::Line,        // L
        19 => Tool::Rect,        // R
        18 => Tool::Ellipse,     // E
        25 => Tool::Pen,         // P
        35 => Tool::Highlighter, // H
        20 => Tool::Text,        // T
        48 => Tool::Pixelate,    // B (blur)
        49 => Tool::Step,        // N (number)
        _ => return None,
    })
}

const KEY_Z: u32 = 44;
const KEY_Y: u32 = 21;
const KEY_C: u32 = 46;

fn trace(msg: impl FnOnce() -> String) {
    if std::env::var_os("OPAL_TRACE").is_some() {
        eprintln!("{}", msg());
    }
}

impl Overlay {
    // ── state helpers ────────────────────────────────────────────────

    fn style(&self) -> Style {
        Style { color: COLORS[self.color], width: self.width }
    }

    /// The output the toolbar must stay on: the one under the selection's center.
    fn toolbar_area(&self, sel: Rect) -> Rect {
        let c = Pt::new(sel.x + sel.w / 2.0, sel.y + sel.h / 2.0);
        self.surfs.iter().map(|s| s.geo).find(|g| g.contains(c)).unwrap_or(self.bounds)
    }

    fn toolbar(&self) -> Option<Toolbar> {
        let sel = self.sel?;
        if matches!(self.drag, Drag::New { .. } | Drag::Move { .. } | Drag::Resize { .. }) {
            return None;
        }
        (sel.w >= 4.0 && sel.h >= 4.0).then(|| toolbar::layout(sel, self.toolbar_area(sel)))
    }

    fn ui_region(&self, sel: Rect) -> Rect {
        let tb = toolbar::layout(sel, self.toolbar_area(sel));
        sel.inflate(70.0).union(&tb.rect.inflate(40.0))
    }

    fn invalidate(&mut self, region: Option<Rect>, qh: &QueueHandle<Self>) {
        let mut todo = Vec::new();
        for (i, s) in self.surfs.iter_mut().enumerate() {
            if region.is_none_or(|r| r.intersect(&s.geo).is_some()) {
                s.dirty = true;
                if s.configured && !s.frame_pending {
                    todo.push(i);
                }
            }
        }
        for i in todo {
            self.draw(qh, i);
        }
    }

    /// Redraw the selection and toolbar area.
    fn refresh(&mut self, qh: &QueueHandle<Self>) {
        let region = self.sel.map(|s| self.ui_region(s));
        self.invalidate(region, qh);
    }

    fn set_sel(&mut self, new: Option<Rect>, qh: &QueueHandle<Self>) {
        let old = self.sel;
        if old == new {
            return;
        }
        self.sel = new;
        let region = match (old, new) {
            (Some(a), Some(b)) => Some(self.ui_region(a).union(&self.ui_region(b))),
            _ => None, // hint appears/disappears: redraw everything
        };
        self.invalidate(region, qh);
    }

    fn snap(&self, p: Pt) -> Pt {
        let p = self.bounds.clamp_pt(p);
        Pt::new(p.x.round(), p.y.round())
    }

    fn update_cursor(&mut self, conn: &Connection) {
        let tb = self.toolbar();
        let icon = match (&self.drag, self.sel) {
            (Drag::Resize { handle, .. }, _) => handle_icon(*handle),
            (Drag::Move { .. }, _) => CursorIcon::Move,
            (Drag::New { .. } | Drag::Draw, _) => CursorIcon::Crosshair,
            (Drag::None, Some(sel)) => {
                if tb.as_ref().is_some_and(|t| t.rect.contains(self.cursor)) {
                    CursorIcon::Default
                } else if let Some(h) = handle_at(sel, self.cursor) {
                    handle_icon(h)
                } else if sel.contains(self.cursor) {
                    if self.space {
                        CursorIcon::Move
                    } else if self.tool == Tool::Text {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Crosshair
                    }
                } else {
                    CursorIcon::Crosshair
                }
            }
            (Drag::None, None) => CursorIcon::Crosshair,
        };
        if icon != self.icon {
            self.icon = icon;
            if let Some(p) = &self.pointer {
                let _ = p.set_cursor(conn, icon);
            }
        }
    }

    // ── annotations ──────────────────────────────────────────────────

    fn commit_text(&mut self) {
        self.typing = false;
        if let Some(it) = self.current.take() {
            if matches!(&it.kind, Kind::Text { text, .. } if !text.trim().is_empty()) {
                self.history.push(it);
            }
        }
    }

    fn start_draw(&mut self, p: Pt) {
        let style = self.style();
        let kind = match self.tool {
            Tool::Pen => Kind::Pen(vec![p]),
            Tool::Highlighter => Kind::Highlighter(vec![p]),
            Tool::Line => Kind::Line(p, p),
            Tool::Arrow => Kind::Arrow(p, p),
            Tool::Rect => Kind::Rect(p, p),
            Tool::Ellipse => Kind::Ellipse(p, p),
            Tool::Pixelate => Kind::Pixelate(p, p),
            Tool::Text => {
                self.current = Some(Item { kind: Kind::Text { at: p, text: String::new() }, style });
                self.typing = true;
                return;
            }
            Tool::Step => {
                let n = self.history.next_step();
                self.history.push(Item { kind: Kind::Step { at: p, n }, style });
                return;
            }
        };
        self.current = Some(Item { kind, style });
        self.drag = Drag::Draw;
    }

    fn extend_draw(&mut self, p: Pt) {
        let Some(it) = &mut self.current else { return };
        match &mut it.kind {
            Kind::Pen(pts) | Kind::Highlighter(pts) => {
                if pts.last().is_none_or(|l| (l.x - p.x).hypot(l.y - p.y) >= 0.75) {
                    pts.push(p);
                }
            }
            Kind::Line(_, b)
            | Kind::Arrow(_, b)
            | Kind::Rect(_, b)
            | Kind::Ellipse(_, b)
            | Kind::Pixelate(_, b) => *b = p,
            _ => {}
        }
    }

    fn finish_draw(&mut self) {
        let Some(it) = self.current.take() else { return };
        let keep = match &it.kind {
            Kind::Pen(_) | Kind::Highlighter(_) => true,
            Kind::Line(a, b) | Kind::Arrow(a, b) | Kind::Rect(a, b) | Kind::Ellipse(a, b)
            | Kind::Pixelate(a, b) => (a.x - b.x).hypot(a.y - b.y) >= 3.0,
            _ => false,
        };
        if keep {
            self.history.push(it);
        }
    }

    fn do_action(&mut self, a: Action, qh: &QueueHandle<Self>) {
        match a {
            Action::Tool(t) => self.select_tool(t),
            Action::Color(i) => {
                self.color = i;
                if let Some(it) = &mut self.current {
                    it.style.color = COLORS[i];
                }
            }
            Action::Width(i) => self.width = WIDTHS[i],
            Action::Undo => self.undo(),
            Action::Redo => {
                self.commit_text();
                self.history.redo();
            }
            Action::Cancel => self.outcome = Some(Outcome::Cancel),
            Action::Confirm => self.finish(),
        }
        self.refresh(qh);
    }

    fn select_tool(&mut self, t: Tool) {
        self.commit_text();
        self.tool = t;
    }

    fn undo(&mut self) {
        if self.typing {
            // typing counts as the newest step: drop the unfinished text
            self.typing = false;
            self.current = None;
        } else {
            self.history.undo();
        }
    }

    fn finish(&mut self) {
        self.commit_text();
        if let Some(sel) = self.sel {
            self.outcome =
                Some(Outcome::Done { rect: sel.rounded(), items: std::mem::take(&mut self.history.items) });
        }
    }

    // ── pointer ──────────────────────────────────────────────────────

    fn press(&mut self, qh: &QueueHandle<Self>) {
        trace(|| format!("press at {:?} sel={:?} tool={:?} typing={}", self.cursor, self.sel, self.tool, self.typing));
        // toolbar first
        if let Some(tb) = self.toolbar() {
            if tb.rect.contains(self.cursor) {
                if let Some(a) = tb.hit(self.cursor) {
                    self.do_action(a, qh);
                }
                return;
            }
        }
        if self.typing {
            self.commit_text();
            self.refresh(qh);
        }
        let p = self.snap(self.cursor);
        match self.sel {
            Some(sel) => match handle_at(sel, p) {
                Some(handle) => self.drag = Drag::Resize { handle, orig: sel },
                None if sel.contains(p) => {
                    if self.space {
                        self.drag = Drag::Move { grab: p, orig: sel };
                    } else {
                        let c = self.cursor;
                        self.start_draw(c);
                        self.refresh(qh);
                    }
                }
                None => self.drag = Drag::New { anchor: p },
            },
            None => self.drag = Drag::New { anchor: p },
        }
        if let Drag::New { anchor } = self.drag {
            self.set_sel(Some(Rect::new(anchor.x, anchor.y, 0.0, 0.0)), qh);
        }
    }

    fn motion(&mut self, qh: &QueueHandle<Self>) {
        let p = self.snap(self.cursor);
        let new = match self.drag {
            Drag::None => {
                let hover = self.toolbar().and_then(|t| t.hit(self.cursor));
                if hover != self.hover {
                    self.hover = hover;
                    self.refresh(qh);
                }
                return;
            }
            Drag::Draw => {
                let c = self.bounds.clamp_pt(self.cursor);
                self.extend_draw(c);
                self.refresh(qh);
                return;
            }
            Drag::New { anchor } => Rect::from_points(anchor, p),
            Drag::Move { grab, orig } => {
                orig.translate(p.x - grab.x, p.y - grab.y).fit_into(&self.bounds)
            }
            Drag::Resize { handle, orig } => {
                let (mut x0, mut y0, mut x1, mut y1) = (orig.x, orig.y, orig.right(), orig.bottom());
                match handle.dx {
                    -1 => x0 = p.x,
                    1 => x1 = p.x,
                    _ => {}
                }
                match handle.dy {
                    -1 => y0 = p.y,
                    1 => y1 = p.y,
                    _ => {}
                }
                Rect::from_points(Pt::new(x0, y0), Pt::new(x1, y1))
            }
        };
        self.set_sel(Some(new), qh);
    }

    fn release(&mut self, qh: &QueueHandle<Self>) {
        let was = std::mem::replace(&mut self.drag, Drag::None);
        match was {
            Drag::Draw => {
                self.finish_draw();
                self.refresh(qh);
            }
            Drag::New { .. } | Drag::Move { .. } | Drag::Resize { .. } => {
                if let Some(sel) = self.sel {
                    if sel.w < 4.0 || sel.h < 4.0 {
                        self.set_sel(None, qh);
                    } else if matches!(was, Drag::New { .. }) && self.opts.finish_on_release {
                        self.outcome = Some(Outcome::Done { rect: sel.rounded(), items: Vec::new() });
                    } else {
                        self.refresh(qh); // toolbar appears
                    }
                }
            }
            Drag::None => {}
        }
    }

    // ── drawing ──────────────────────────────────────────────────────

    fn draw(&mut self, qh: &QueueHandle<Self>, i: usize) {
        let show_hint = self.sel.is_none()
            && self.opts.hint_output.as_deref().is_none_or(|n| n == self.surfs[i].name);
        let tb = self.toolbar();
        let tb_state = toolbar::State {
            tool: self.tool,
            color: self.color,
            width: self.width,
            can_undo: self.history.can_undo() || self.typing,
            can_redo: self.history.can_redo(),
            hover: self.hover,
        };
        let scene = Scene {
            sel: self.sel,
            show_hint,
            canvas_scale: self.opts.canvas_scale,
            items: &self.history.items,
            current: self.current.as_ref(),
            typing: self.typing,
            toolbar: tb.as_ref().map(|t| (t, &tb_state)),
            shots: &self.shots,
        };
        let format = self.format;
        let s = &mut self.surfs[i];
        let (w, h) = (self.shots[s.shot].pixmap.width(), self.shots[s.shot].pixmap.height());
        let Ok((buffer, data)) = s.pool.create_buffer(w as i32, h as i32, (w * 4) as i32, format)
        else {
            return;
        };
        let target = Target { geo: s.geo, k: s.k, frozen: &self.shots[s.shot].pixmap, dim: &s.dim };
        match s.scratch.as_mut() {
            None => {
                let mut pm = PixmapMut::from_bytes(data, w, h).unwrap();
                render(&mut pm, &target, &scene, &mut self.fonts, &self.theme);
            }
            Some(scratch) => {
                let mut pm = scratch.as_mut();
                render(&mut pm, &target, &scene, &mut self.fonts, &self.theme);
                for (d, p) in data.chunks_exact_mut(4).zip(scratch.data().chunks_exact(4)) {
                    d.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            }
        }
        let surface = s.layer.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        surface.frame(qh, FrameCallbackData(surface.clone()));
        let _ = buffer.attach_to(surface);
        s.layer.commit();
        s.frame_pending = true;
        s.dirty = false;
    }
}

struct Scene<'a> {
    sel: Option<Rect>,
    show_hint: bool,
    canvas_scale: f32,
    items: &'a [Item],
    current: Option<&'a Item>,
    typing: bool,
    toolbar: Option<(&'a Toolbar, &'a toolbar::State)>,
    shots: &'a [Shot],
}

struct Target<'a> {
    /// Logical rectangle of this output.
    geo: Rect,
    /// Physical pixels per logical pixel.
    k: f32,
    frozen: &'a Pixmap,
    dim: &'a Pixmap,
}

/// Copy a physical-pixel rectangle between two same-sized pixel buffers.
fn copy_px(dst: &mut [u8], src: &[u8], w: i32, h: i32, x0: i32, y0: i32, x1: i32, y1: i32) {
    let (x0, y0, x1, y1) = (x0.max(0), y0.max(0), x1.min(w), y1.min(h));
    if x1 <= x0 {
        return;
    }
    for y in y0..y1 {
        let a = ((y * w + x0) * 4) as usize;
        let b = ((y * w + x1) * 4) as usize;
        dst[a..b].copy_from_slice(&src[a..b]);
    }
}

fn render(pm: &mut PixmapMut, t: &Target, sc: &Scene, fonts: &mut Fonts, th: &Theme) {
    let (geo, k) = (t.geo, t.k);
    pm.data_mut().copy_from_slice(t.dim.data());
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    // logical (global) -> physical (this buffer)
    let to_px = |r: Rect| Rect::new((r.x - geo.x) * k, (r.y - geo.y) * k, r.w * k, r.h * k);

    let Some(sel) = sc.sel else {
        if sc.show_hint {
            hint(pm, fonts, th, k);
        }
        return;
    };

    // bright selection straight from the capture
    let sp = to_px(sel);
    let (sx0, sy0) = (sp.x.round() as i32, sp.y.round() as i32);
    let (sx1, sy1) = (sp.right().round() as i32, sp.bottom().round() as i32);
    copy_px(pm.data_mut(), t.frozen.data(), pw, ph, sx0, sy0, sx1, sy1);

    // annotations, then put the dimmed pixels back outside the selection
    if !sc.items.is_empty() || sc.current.is_some() {
        let ctx = Ctx {
            tf: Transform::from_row(k, 0.0, 0.0, k, -geo.x * k, -geo.y * k),
            k,
            shots: sc.shots,
        };
        annot::draw_all(pm, &ctx, sc.items, fonts);
        if let Some(cur) = sc.current {
            annot::draw(pm, &ctx, cur, fonts);
            if sc.typing {
                caret(pm, &ctx, cur, fonts, th);
            }
        }
        let d = t.dim.data();
        let data = pm.data_mut();
        copy_px(data, d, pw, ph, 0, 0, pw, sy0); // above
        copy_px(data, d, pw, ph, 0, sy1, pw, ph); // below
        copy_px(data, d, pw, ph, 0, sy0, sx0, sy1); // left
        copy_px(data, d, pw, ph, sx1, sy0, pw, sy1); // right
    }

    draw_frame(pm, sp, k, sc.toolbar.is_some());
    if sel.w >= 1.0 && sel.h >= 1.0 {
        let dims = format!(
            "{} × {}",
            (sel.w * sc.canvas_scale).round() as i32,
            (sel.h * sc.canvas_scale).round() as i32
        );
        size_label(pm, fonts, th, &dims, sp, k);
    }
    if let Some((tb, st)) = sc.toolbar {
        toolbar::draw(pm, tb, st, th, k, &to_px);
    }
}

fn hint(pm: &mut PixmapMut, fonts: &mut Fonts, th: &Theme, k: f32) {
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    let text = "Drag to select an area   ·   Esc to cancel";
    let size = 14.0 * k;
    let (tw, text_h) = fonts.measure(text, size, 500);
    let (padx, pady) = (20.0 * k, 11.0 * k);
    let r = Rect::new((pw - tw) / 2.0 - padx, ph - 110.0 * k, tw + 2.0 * padx, text_h + 2.0 * pady);
    glass::panel(pm, r, r.h / 2.0, k, th);
    fonts.draw(pm, text, r.x + padx, r.y + pady, size, 500, th.text);
}

/// Blinking-free text cursor after the last typed character.
fn caret(pm: &mut PixmapMut, ctx: &Ctx, it: &Item, fonts: &mut Fonts, th: &Theme) {
    let Kind::Text { at, text } = &it.kind else { return };
    let size = annot::text_size(&it.style) * ctx.k;
    let lines = text.split('\n').count();
    let last = text.rsplit('\n').next().unwrap_or("");
    let (w, line_h) = if last.is_empty() { (0.0, (size * 1.25).round()) } else { fonts.measure(last, size, 600) };
    let x = at.x * ctx.k + ctx.tf.tx + w + 1.0 * ctx.k;
    let y = at.y * ctx.k + ctx.tf.ty + (lines - 1) as f32 * (size * 1.25).round();
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y + line_h * 0.08, 1.6 * ctx.k, line_h * 0.84) {
        let mut p = Paint::default();
        p.set_color(th.text);
        pm.fill_rect(r, &p, Transform::identity(), None);
    }
}

fn draw_frame(pm: &mut PixmapMut, r: Rect, k: f32, editing: bool) {
    let mut paint = Paint::default();
    paint.anti_alias = true;
    // dark under-line so the border reads on bright screenshots too
    if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h) {
        let path = PathBuilder::from_rect(rect);
        paint.set_color(Color::from_rgba8(0, 0, 0, 70));
        pm.stroke_path(&path, &paint, &Stroke { width: 3.0 * k, ..Default::default() }, Transform::identity(), None);
        paint.set_color(Color::from_rgba8(255, 255, 255, if editing { 150 } else { 235 }));
        pm.stroke_path(&path, &paint, &Stroke { width: 1.5 * k, ..Default::default() }, Transform::identity(), None);
    }
    // handles: corners and edge midpoints
    let big = r.w > 56.0 * k && r.h > 56.0 * k;
    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
    let mut pts = vec![(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())];
    if big {
        pts.extend([(cx, r.y), (cx, r.bottom()), (r.x, cy), (r.right(), cy)]);
    }
    for (x, y) in pts {
        if let Some(c) = PathBuilder::from_circle(x, y, 5.0 * k) {
            paint.set_color(Color::from_rgba8(0, 0, 0, 80));
            pm.fill_path(&c, &paint, FillRule::Winding, Transform::from_translate(0.0, k), None);
            paint.set_color(Color::from_rgba8(255, 255, 255, 255));
            pm.fill_path(&c, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
}

fn size_label(pm: &mut PixmapMut, fonts: &mut Fonts, th: &Theme, text: &str, sel: Rect, k: f32) {
    let size = 12.5 * k;
    let (tw, text_h) = fonts.measure(text, size, 600);
    let (padx, pady) = (11.0 * k, 5.0 * k);
    let (w, h) = (tw + 2.0 * padx, text_h + 2.0 * pady);
    let mut y = sel.y - h - 10.0 * k;
    if y < 6.0 * k {
        y = sel.y + 10.0 * k; // no room above: tuck inside
    }
    let r = Rect::new(sel.x.max(6.0 * k), y, w, h);
    glass::panel(pm, r, h / 2.0, k, th);
    fonts.draw(pm, text, r.x + padx, r.y + pady, size, 600, th.text);
}

// ── Wayland plumbing ─────────────────────────────────────────────────────

impl CompositorHandler for Overlay {
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

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.outcome = Some(Outcome::Cancel);
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(i) = self.surfs.iter().position(|s| s.layer.wl_surface() == layer.wl_surface()) {
            if !self.surfs[i].configured {
                self.surfs[i].configured = true;
                self.draw(qh, i);
                if std::env::var_os("OPAL_TRACE").is_some() {
                    eprintln!("first frame on {}", self.surfs[i].name);
                }
            }
        }
    }
}

impl SeatHandler for Overlay {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let repeat = Box::new(|st: &mut Overlay, _: &wl_keyboard::WlKeyboard, e: KeyEvent| {
                let (conn, qh) = (st.conn.clone(), st.qh.clone());
                st.key(&conn, &qh, e);
            });
            self.keyboard = self
                .seats
                .get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat)
                .ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            // dedicated surface for the cursor image
            let cursor_surface = self.compositor.create_surface(qh);
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

impl KeyboardHandler for Overlay {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.space = false;
    }

    fn press_key(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        self.key(conn, qh, e);
    }
    fn repeat_key(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        self.key(conn, qh, e);
    }
    fn release_key(&mut self, conn: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        if e.keysym == Keysym::space {
            self.space = false;
            self.update_cursor(conn);
        }
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = m;
    }
}

impl Overlay {
    fn key(&mut self, conn: &Connection, qh: &QueueHandle<Self>, e: KeyEvent) {
        // evdev code of the physical key, so shortcuts work on any layout
        let phys = e.raw_code; // wl_keyboard.key: already the evdev code
        let ctrl = self.mods.ctrl;
        trace(|| format!("key phys={phys} sym={:?} utf8={:?} ctrl={ctrl} typing={}", e.keysym, e.utf8, self.typing));

        // text entry swallows plain keys
        if self.typing && !ctrl {
            let mut changed = true;
            match e.keysym {
                Keysym::Escape => self.commit_text(),
                Keysym::BackSpace => {
                    if let Some(Item { kind: Kind::Text { text, .. }, .. }) = &mut self.current {
                        text.pop();
                    }
                }
                Keysym::Return | Keysym::KP_Enter => {
                    if let Some(Item { kind: Kind::Text { text, .. }, .. }) = &mut self.current {
                        text.push('\n');
                    }
                }
                _ => match e.utf8.as_deref() {
                    Some(s) if !s.chars().any(char::is_control) => {
                        if let Some(Item { kind: Kind::Text { text, .. }, .. }) = &mut self.current {
                            text.push_str(s);
                        }
                    }
                    _ => changed = false,
                },
            }
            if changed {
                self.refresh(qh);
            }
            return;
        }

        match e.keysym {
            Keysym::Escape => self.outcome = Some(Outcome::Cancel),
            Keysym::Return | Keysym::KP_Enter => self.finish(),
            Keysym::space => {
                self.space = true;
                self.update_cursor(conn);
            }
            _ if ctrl && phys == KEY_C => self.finish(),
            _ if ctrl && phys == KEY_Z && !self.mods.shift => {
                self.undo();
                self.refresh(qh);
            }
            _ if ctrl && (phys == KEY_Y || (phys == KEY_Z && self.mods.shift)) => {
                self.commit_text();
                self.history.redo();
                self.refresh(qh);
            }
            _ if !ctrl && !self.mods.alt && self.sel.is_some() => {
                if let Some(t) = tool_for_key(phys) {
                    self.select_tool(t);
                    self.refresh(qh);
                } else if (2..=1 + COLORS.len() as u32).contains(&phys) {
                    self.do_action(Action::Color(phys as usize - 2), qh);
                }
            }
            _ => {}
        }
    }
}

impl PointerHandler for Overlay {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let Some(i) = self.surfs.iter().position(|s| s.layer.wl_surface() == &ev.surface) else {
                continue;
            };
            let geo = self.surfs[i].geo;
            let pos = Pt::new(geo.x + ev.position.0 as f32, geo.y + ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Enter { .. } => {
                    self.cursor = pos;
                    self.icon = CursorIcon::Default; // force re-send after enter
                    self.update_cursor(conn);
                }
                PointerEventKind::Leave { .. } => {}
                PointerEventKind::Motion { .. } => {
                    self.cursor = pos;
                    self.motion(qh);
                    self.update_cursor(conn);
                }
                PointerEventKind::Press { button, .. } => {
                    self.cursor = pos;
                    match button {
                        BTN_LEFT => self.press(qh),
                        BTN_RIGHT => match self.sel {
                            Some(_) => {
                                self.current = None;
                                self.typing = false;
                                self.drag = Drag::None;
                                self.set_sel(None, qh)
                            }
                            None => self.outcome = Some(Outcome::Cancel),
                        },
                        _ => {}
                    }
                    self.update_cursor(conn);
                }
                PointerEventKind::Release { button, .. } => {
                    self.cursor = pos;
                    if button == BTN_LEFT {
                        self.release(qh);
                        self.update_cursor(conn);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    // wheel: stroke width (up = thicker)
                    let step = if vertical.absolute < 0.0 { 1.0 } else if vertical.absolute > 0.0 { -1.0 } else { 0.0 };
                    if step != 0.0 && self.sel.is_some() {
                        self.width = (self.width + step).clamp(1.0, 24.0);
                        if let Some(it) = &mut self.current {
                            it.style.width = self.width;
                        }
                        self.refresh(qh);
                    }
                }
            }
        }
    }
}

impl ShmHandler for Overlay {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

impl Dispatch<WpViewporter, ()> for Overlay {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for Overlay {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_registry!(Overlay);
delegate_dispatch2!(Overlay);
