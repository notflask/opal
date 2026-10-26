//! The frozen-screen overlay. Per output: the capture itself as background
//! (no copy), dim bars, a border, handles, the annotation layer and a few
//! small panels, all as subsurfaces the compositor composes. Moving the
//! selection only moves rectangles; nothing is redrawn pixel by pixel.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, generic::Generic},
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
    shm::{Shm, ShmHandler},
    subcompositor::SubcompositorState,
};
use tiny_skia::{Paint, Pixmap, PixmapMut, Transform};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::{
    annot::{self, Ctx, History, Item, Kind, Style, Tool, WIDTHS},
    capture::Shot,
    config::{Cmd, Config},
    geom::{Pt, Rect},
    surface::{Anno, Env, IRect, Panel, Quad, Solids},
    text::Fonts,
    theme::{Rgb, Theme},
    ui::{self, Action, Canvas, Toolbar},
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
/// How close (logical px) the pointer must be to a border to grab it.
const GRAB: f32 = 8.0;
const HANDLE_OUTER: i32 = 8;
const HANDLE_INNER: i32 = 6;

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

/// Indexes into `Solids`.
const SOLID_DIM: usize = 0;
const SOLID_PRIMARY: usize = 1;
const SOLID_SURFACE: usize = 2;

struct Surf {
    layer: LayerSurface,
    viewport: WpViewport,
    name: String,
    /// Index into `Overlay::shots`.
    shot: usize,
    /// Logical rectangle in compositor space.
    geo: Rect,
    /// Physical pixels per logical pixel.
    k: f32,
    dim: [Quad; 4],
    anno: Anno,
    /// Where the annotation layer was last drawn (global logical origin).
    anno_origin: Option<(i32, i32)>,
    /// What changed in the annotations since this output last drew them.
    anno_dirty: Option<Rect>,
    border: [Panel; 4],
    handle_edge: Vec<Quad>,
    handle_fill: Vec<Quad>,
    toolbar: Panel,
    label: Panel,
    hint: Panel,
    configured: bool,
    /// The background was attached.
    drawn: bool,
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
    env: Env,
    outputs: OutputState,
    seats: SeatState,
    shm: Shm,
    solids: Solids,
    surfs: Vec<Surf>,
    shots: Vec<Shot>,
    cfg: Config,
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
    /// Where `current` was last drawn, to redraw exactly what changed.
    cur_bounds: Option<Rect>,
    typing: bool,
    hover: Option<Action>,
}

pub fn run(conn: &Connection, shots: Vec<Shot>, cfg: Config, theme: Theme, fonts: Fonts, opts: Options) -> Result<Finished> {
    let (globals, mut queue) = registry_queue_init(conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("the compositor lacks wl_compositor")?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .context("the compositor lacks wl_subcompositor")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("the compositor lacks wlr-layer-shell")?;
    let viewporter: WpViewporter =
        globals.bind(&qh, 1..=1, ()).context("the compositor lacks wp_viewporter")?;
    let shm = Shm::bind(&globals, &qh).context("the compositor lacks wl_shm")?;
    let env = Env { compositor, subcompositor, viewporter };

    let dim_a = (cfg.ui.dim * 255.0).round() as u8;
    let solids = Solids::new(
        &shm,
        &[Rgb(0, 0, 0).argb(dim_a), theme.primary.argb(255), theme.surface.argb(255)],
    )?;

    let mut event_loop: EventLoop<Overlay> = EventLoop::try_new()?;
    let (tool, color, width) = (cfg.draw.tool, cfg.draw.color, cfg.draw.width);
    let mut app = Overlay {
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
        cfg,
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
        tool,
        color,
        width,
        history: History::default(),
        current: None,
        cur_bounds: None,
        typing: false,
        hover: None,
    };
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;

    let wl_outputs: Vec<_> = app.outputs.outputs().collect();
    for idx in 0..app.shots.len() {
        let shot = &app.shots[idx];
        let out = wl_outputs
            .iter()
            .find(|o| app.outputs.info(o).and_then(|i| i.name).as_deref() == Some(shot.name.as_str()))
            .with_context(|| format!("output {} vanished", shot.name))?;
        let surface = app.env.compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("opal"), Some(out));
        layer.set_anchor(Anchor::all());
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(0, 0);
        let viewport = app.env.viewporter.get_viewport(layer.wl_surface(), &qh, ());
        viewport.set_destination(shot.lw, shot.lh);
        layer.commit();

        let parent = layer.wl_surface().clone();
        let quad = |c: usize| Quad::new(&app.env, &parent, app.solids.get(c), &qh);
        let quads = |n: usize, c: usize| (0..n).map(|_| quad(c)).collect::<Vec<_>>();
        // creation order is stacking order
        let dim: [Quad; 4] = std::array::from_fn(|_| quad(SOLID_DIM));
        let anno = Anno::new(&app.env, &parent, &app.shm, &qh)?;
        let mut border = Vec::new();
        for _ in 0..4 {
            border.push(Panel::new(&app.env, &parent, &app.shm, &qh)?);
        }
        let border: [Panel; 4] = border.try_into().ok().context("border panels")?;
        let handle_edge = quads(8, SOLID_SURFACE);
        let handle_fill = quads(8, SOLID_PRIMARY);
        let toolbar = Panel::new(&app.env, &parent, &app.shm, &qh)?;
        let label = Panel::new(&app.env, &parent, &app.shm, &qh)?;
        let hint = Panel::new(&app.env, &parent, &app.shm, &qh)?;
        app.surfs.push(Surf {
            layer,
            viewport,
            name: shot.name.clone(),
            shot: idx,
            geo: shot.logical(),
            k: shot.scale(),
            dim,
            anno,
            anno_origin: None,
            anno_dirty: None,
            border,
            handle_edge,
            handle_fill,
            toolbar,
            label,
            hint,
            configured: false,
            drawn: false,
            frame_pending: false,
            dirty: true,
        });
    }
    app.bounds = app.surfs.iter().map(|s| s.geo).reduce(|a, b| a.union(&b)).unwrap_or_default();

    // a second `opal` (or a signal) closes us cleanly
    let signals = crate::sig::signal_fd(&[libc::SIGUSR1, libc::SIGTERM, libc::SIGINT])?;
    event_loop
        .handle()
        .insert_source(Generic::new(signals, Interest::READ, Mode::Level), |_, fd, app: &mut Overlay| {
            crate::sig::drain(&**fd);
            app.outcome = Some(Outcome::Cancel);
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
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
        fonts: std::mem::replace(&mut app.fonts, Fonts::new("builtin")),
    })
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

fn trace(msg: impl FnOnce() -> String) {
    if std::env::var_os("OPAL_TRACE").is_some() {
        eprintln!("{}", msg());
    }
}

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

impl Overlay {
    // ── state helpers ────────────────────────────────────────────────

    fn style(&self) -> Style {
        Style { color: self.cfg.draw.palette[self.color], width: self.width }
    }

    /// The output holding a point (half-open, so borders belong to one output only).
    fn owner(&self, p: Pt) -> Option<usize> {
        let p = self.bounds.clamp_pt(p);
        self.surfs.iter().position(|s| p.x >= s.geo.x && p.x < s.geo.right() && p.y >= s.geo.y && p.y < s.geo.bottom())
    }

    /// The output the toolbar must stay on: the one under the selection's center.
    fn toolbar_area(&self, sel: Rect) -> Rect {
        let c = Pt::new(sel.x + sel.w / 2.0, sel.y + sel.h / 2.0);
        self.owner(c).map_or(self.bounds, |i| self.surfs[i].geo)
    }

    fn toolbar(&self) -> Option<Toolbar> {
        let sel = self.sel?;
        if matches!(self.drag, Drag::New { .. } | Drag::Move { .. } | Drag::Resize { .. }) {
            return None;
        }
        (sel.w >= 4.0 && sel.h >= 4.0).then(|| {
            ui::layout(sel, self.toolbar_area(sel), self.cfg.draw.palette.len(), &self.cfg.ui.toolbar)
        })
    }

    /// Redraw every output (each one only touches what changed).
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

    fn set_sel(&mut self, new: Option<Rect>, qh: &QueueHandle<Self>) {
        if self.sel == new {
            return;
        }
        self.sel = new;
        self.schedule(qh);
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

    /// Remember that an area of the annotation layer must be redrawn.
    fn mark_anno(&mut self, r: Rect) {
        for s in &mut self.surfs {
            s.anno_dirty = Some(s.anno_dirty.map_or(r, |d| d.union(&r)));
        }
    }

    /// `current` changed: redraw where it was and where it is.
    fn touch_current(&mut self) {
        let now = self.current.as_ref().map(Item::rough_bounds);
        let old = std::mem::replace(&mut self.cur_bounds, now);
        for r in [old, now].into_iter().flatten() {
            self.mark_anno(r);
        }
    }

    fn commit_text(&mut self) {
        self.typing = false;
        if let Some(it) = self.current.take() {
            if matches!(&it.kind, Kind::Text { text, .. } if !text.trim().is_empty()) {
                self.history.push(it);
            }
        }
        self.touch_current();
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
                self.touch_current();
                return;
            }
            Tool::Step => {
                let n = self.history.next_step();
                let item = Item { kind: Kind::Step { at: p, n }, style };
                self.mark_anno(item.rough_bounds());
                self.history.push(item);
                return;
            }
        };
        self.current = Some(Item { kind, style });
        self.drag = Drag::Draw;
        self.touch_current();
    }

    fn extend_draw(&mut self, p: Pt) {
        let Some(it) = &mut self.current else { return };
        match &mut it.kind {
            Kind::Pen(pts) | Kind::Highlighter(pts) => {
                if pts.last().is_none_or(|l| (l.x - p.x).hypot(l.y - p.y) >= 0.75) {
                    pts.push(p);
                }
            }
            Kind::Line(_, b) | Kind::Arrow(_, b) | Kind::Rect(_, b) | Kind::Ellipse(_, b) | Kind::Pixelate(_, b) => *b = p,
            _ => {}
        }
        self.touch_current();
    }

    fn finish_draw(&mut self) {
        let Some(it) = self.current.take() else { return };
        let keep = match &it.kind {
            Kind::Pen(_) | Kind::Highlighter(_) => true,
            Kind::Line(a, b) | Kind::Arrow(a, b) | Kind::Rect(a, b) | Kind::Ellipse(a, b) | Kind::Pixelate(a, b) => {
                (a.x - b.x).hypot(a.y - b.y) >= 3.0
            }
            _ => false,
        };
        if keep {
            self.history.push(it);
        }
        self.touch_current();
    }

    fn do_action(&mut self, a: Action, qh: &QueueHandle<Self>) {
        match a {
            Action::Tool(t) => self.select_tool(t),
            Action::Color(i) => {
                self.color = i;
                if let Some(it) = &mut self.current {
                    it.style.color = self.cfg.draw.palette[i];
                    self.touch_current();
                }
            }
            Action::Width(i) => self.width = WIDTHS[i],
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Cancel => self.outcome = Some(Outcome::Cancel),
            Action::Confirm => self.finish(),
        }
        self.schedule(qh);
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
            self.touch_current();
        } else if let Some(r) = self.history.undo() {
            self.mark_anno(r);
        }
    }

    fn redo(&mut self) {
        self.commit_text();
        if let Some(r) = self.history.redo() {
            self.mark_anno(r);
        }
    }

    fn finish(&mut self) {
        self.commit_text();
        if let Some(sel) = self.sel {
            self.outcome = Some(Outcome::Done { rect: sel.rounded(), items: std::mem::take(&mut self.history.items) });
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
            self.schedule(qh);
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
                        self.schedule(qh);
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
                    self.schedule(qh);
                }
                return;
            }
            Drag::Draw => {
                let c = self.bounds.clamp_pt(self.cursor);
                self.extend_draw(c);
                self.schedule(qh);
                return;
            }
            Drag::New { anchor } => Rect::from_points(anchor, p),
            Drag::Move { grab, orig } => orig.translate(p.x - grab.x, p.y - grab.y).fit_into(&self.bounds),
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
                self.schedule(qh);
            }
            Drag::New { .. } | Drag::Move { .. } | Drag::Resize { .. } => {
                if let Some(sel) = self.sel {
                    if sel.w < 4.0 || sel.h < 4.0 {
                        self.set_sel(None, qh);
                    } else if matches!(was, Drag::New { .. }) && self.opts.finish_on_release {
                        self.outcome = Some(Outcome::Done { rect: sel.rounded(), items: Vec::new() });
                    } else {
                        self.schedule(qh); // toolbar appears
                    }
                }
            }
            Drag::None => {}
        }
    }

    // ── drawing ──────────────────────────────────────────────────────

    /// Bring every subsurface of output `i` up to date, then commit it.
    fn draw(&mut self, qh: &QueueHandle<Self>, i: usize) {
        let t0 = std::time::Instant::now();
        let tb = self.toolbar();
        let sel = self.sel;
        let typing = self.typing;
        let border = self.cfg.ui.border as i32;
        let (geo, k) = (self.surfs[i].geo, self.surfs[i].k);
        let (ow, oh) = (geo.w as i32, geo.h as i32);
        let area = IRect::new(0, 0, ow, oh);
        let local = |r: Rect| IRect::round(Rect::new(r.x - geo.x, r.y - geo.y, r.w, r.h));
        let px = |v: i32| (v as f32 * k).round() as i32;

        // what the toolbar shows
        let can_undo = self.history.can_undo() || typing;
        let can_redo = self.history.can_redo();
        let (tool, color, width, hover) = (self.tool, self.color, self.width, self.hover);
        let tb_owner = tb.as_ref().and_then(|t| self.owner(Pt::new(t.rect.x + t.rect.w / 2.0, t.rect.y + t.rect.h / 2.0)));
        let label_owner = sel.and_then(|s| self.owner(Pt::new(s.x, s.y)));
        let hint_here = self.opts.hint_output.as_deref().is_none_or(|n| n == self.surfs[i].name);

        let surf = &mut self.surfs[i];
        let first = !surf.drawn;

        // background: the capture itself, attached once
        if first {
            let shot = &self.shots[surf.shot];
            let wl = surf.layer.wl_surface();
            wl.attach(Some(&shot.buffer), 0, 0);
            wl.damage_buffer(0, 0, shot.w as i32, shot.h as i32);
            if shot.has_alpha() {
                if let Ok(r) = Region::new(&self.env.compositor) {
                    r.add(0, 0, ow, oh);
                    wl.set_opaque_region(Some(r.wl_region()));
                }
            }
            surf.viewport.set_destination(ow, oh);
        }

        let sel_l = sel.map(local);

        // dim: everything outside the selection
        let dims = match sel_l.and_then(|s| s.clip(area)) {
            None => [Some(area), None, None, None],
            Some(s) => [
                Some(IRect::new(0, 0, ow, s.y)),
                Some(IRect::new(0, s.bottom(), ow, oh - s.bottom())),
                Some(IRect::new(0, s.y, s.x, s.h)),
                Some(IRect::new(s.right(), s.y, ow - s.right(), s.h)),
            ],
        };
        for (q, r) in surf.dim.iter_mut().zip(dims) {
            q.set(r);
        }

        // border (outside the selection) and handles
        match sel_l {
            Some(s) => {
                // Hyprland draws a 1px-thick subsurface 2px thick, so a 1px
                // bar is a 2px buffer whose second pixel is transparent.
                let t = if border == 1 { 2 } else { border };
                let bars = [
                    (IRect::new(s.x - border, s.y - border, s.w + 2 * border, t), true),
                    (IRect::new(s.x - border, s.bottom(), s.w + 2 * border, t), true),
                    (IRect::new(s.x - border, s.y, t, s.h), false),
                    (IRect::new(s.right(), s.y, t, s.h), false),
                ];
                let primary = self.theme.primary.argb(255);
                let colored = px(border);
                for (q, (full, horizontal)) in surf.border.iter_mut().zip(bars) {
                    let r = full.clip(area);
                    let size = r.map_or((0, 0), |r| (px(r.w), px(r.h)));
                    // how much of the bar's start the clip cut off, in px
                    let skip = r.map_or(0, |r| px(if horizontal { r.y - full.y } else { r.x - full.x }));
                    q.set(r, size, hash_of(&(size, primary, skip, colored)), |buf| {
                        bar_pixels(buf, size, horizontal, skip, colored, primary)
                    });
                }
                let big = s.w > 56 && s.h > 56;
                let (cx, cy) = (s.x + s.w / 2, s.y + s.h / 2);
                let mut pts = vec![(s.x, s.y), (s.right(), s.y), (s.x, s.bottom()), (s.right(), s.bottom())];
                if big {
                    pts.extend([(cx, s.y), (cx, s.bottom()), (s.x, cy), (s.right(), cy)]);
                }
                for n in 0..8 {
                    let at = pts.get(n).copied();
                    let r = |size: i32| at.and_then(|(x, y)| IRect::new(x - size / 2, y - size / 2, size, size).clip(area));
                    surf.handle_edge[n].set(r(HANDLE_OUTER));
                    surf.handle_fill[n].set(r(HANDLE_INNER));
                }
            }
            None => {
                for q in &mut surf.border {
                    q.set(None, (0, 0), 0, |_| {});
                }
                for q in surf.handle_edge.iter_mut().chain(&mut surf.handle_fill) {
                    q.set(None);
                }
            }
        }

        // annotations: only the selection part on this output
        let layer = sel.and_then(|s| local(s.rounded()).clip(area));
        let has_content = !self.history.items.is_empty() || self.current.is_some();
        let mut anno_ok = true;
        match layer {
            Some(lr) if has_content => {
                let origin = (geo.x as i32 + lr.x, geo.y as i32 + lr.y);
                let size = (px(lr.w), px(lr.h));
                let moved = surf.anno_origin != Some(origin);
                let full = IRect::new(0, 0, size.0, size.1);
                let dirty = if moved || !surf.anno.is_shown() {
                    Some(full)
                } else {
                    surf.anno_dirty.map(|d| {
                        let x0 = ((d.x - origin.0 as f32) * k).floor() as i32 - 1;
                        let y0 = ((d.y - origin.1 as f32) * k).floor() as i32 - 1;
                        let x1 = ((d.right() - origin.0 as f32) * k).ceil() as i32 + 1;
                        let y1 = ((d.bottom() - origin.1 as f32) * k).ceil() as i32 + 1;
                        IRect::new(x0, y0, x1 - x0, y1 - y0)
                    })
                };
                let history = &self.history.items;
                let current = self.current.as_ref();
                let fonts = &mut self.fonts;
                let shots = &self.shots;
                let (aa, block) = (self.cfg.draw.antialias, self.cfg.draw.pixelate_block);
                let mut render = |pm: &mut Pixmap, region: IRect| {
                    let ctx = Ctx {
                        tf: Transform::from_row(k, 0.0, 0.0, k, -(origin.0 as f32) * k - region.x as f32, -(origin.1 as f32) * k - region.y as f32),
                        k,
                        shots,
                        aa,
                        block,
                    };
                    // logical, global area this region covers
                    let area = Rect::new(
                        origin.0 as f32 + region.x as f32 / k,
                        origin.1 as f32 + region.y as f32 / k,
                        region.w as f32 / k,
                        region.h as f32 / k,
                    );
                    let mut view = pm.as_mut();
                    for it in history.iter().chain(current) {
                        if it.rough_bounds().intersect(&area).is_some() {
                            annot::draw(&mut view, &ctx, it, fonts);
                        }
                    }
                    if typing {
                        if let Some(it) = current {
                            caret(&mut view, &ctx, it, fonts);
                        }
                    }
                };
                anno_ok = surf.anno.update(lr, size, dirty, &mut render);
                if anno_ok {
                    surf.anno_origin = Some(origin);
                    surf.anno_dirty = None;
                }
            }
            _ => {
                surf.anno.clear();
                surf.anno_origin = None;
                surf.anno_dirty = None;
            }
        }

        // toolbar
        let tb_here = tb.as_ref().filter(|_| tb_owner == Some(i));
        match tb_here {
            Some(t) => {
                let r = local(t.rect);
                let size = (px(r.w), px(r.h));
                let key = hash_of(&(tool, color, width.to_bits(), can_undo, can_redo, hover, size, k.to_bits(), border));
                let st = ui::State { tool, color, width, palette: &self.cfg.draw.palette, can_undo, can_redo, hover };
                let theme = &self.theme;
                surf.toolbar.set(Some(r), size, key, |buf| {
                    ui::draw_toolbar(&mut Canvas::new(buf, size.0, size.1), t, &st, theme, border as f32, k);
                });
            }
            None => surf.toolbar.set(None, (0, 0), 0, |_| {}),
        }

        // size label above the selection's top-left corner
        let label_here = self.cfg.ui.size_label && label_owner == Some(i);
        match sel.filter(|s| label_here && s.w >= 1.0 && s.h >= 1.0) {
            Some(s) => {
                let text = format!(
                    "{} x {}",
                    (s.w * self.opts.canvas_scale).round() as i32,
                    (s.h * self.opts.canvas_scale).round() as i32
                );
                let (lw, lh) = ui::label_size(&text);
                let (lw, lh) = (lw as i32, lh as i32);
                let sl = local(s);
                let mut y = sl.y - lh - border;
                if y < 0 {
                    y = sl.y + border; // no room above: tuck inside
                }
                let x = sl.x.clamp(0, (ow - lw).max(0));
                let size = (px(lw), px(lh));
                let key = hash_of(&(text.as_str(), size));
                let theme = &self.theme;
                surf.label.set(Some(IRect::new(x, y, lw, lh)), size, key, |buf| {
                    ui::draw_label(&mut Canvas::new(buf, size.0, size.1), &text, theme, border as f32, k);
                });
            }
            None => surf.label.set(None, (0, 0), 0, |_| {}),
        }

        // hint before the first drag
        if sel.is_none() && self.cfg.ui.hint && hint_here {
            let text = "drag to select \u{b7} esc to cancel";
            let (hw, hh) = ui::label_size(text);
            let (hw, hh) = (hw as i32 + 16, hh as i32 + 8);
            let size = (px(hw), px(hh));
            let key = hash_of(&(size, k.to_bits()));
            let theme = &self.theme;
            surf.hint.set(Some(IRect::new((ow - hw) / 2, oh - 110 - hh, hw, hh)), size, key, |buf| {
                let mut c = Canvas::new(buf, size.0, size.1);
                ui::frame(&mut c, theme, border as f32, k);
                let s = (k.round() as i32).max(1);
                c.text(px(8 + 5), px(4 + 2), text, s, theme.text.argb(255));
            });
        } else {
            surf.hint.set(None, (0, 0), 0, |_| {});
        }

        let wl = surf.layer.wl_surface();
        wl.frame(qh, FrameCallbackData(wl.clone()));
        surf.layer.commit();
        surf.frame_pending = true;
        // no buffer was free for the annotations: try again next frame
        surf.dirty = !anno_ok;
        surf.drawn = true;
        trace(|| format!("draw {} took {:?}", surf.name, t0.elapsed()));
    }
}

/// Fill a bar buffer: the first `colored` pixels across its thickness (counted
/// from the uncut start, `skip` already clipped away) get `argb`, the rest stay
/// transparent.
fn bar_pixels(buf: &mut [u8], size: (i32, i32), horizontal: bool, skip: i32, colored: i32, argb: u32) {
    let c = argb.to_le_bytes();
    for (i, p) in buf.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as i32 % size.0, i as i32 / size.0);
        let across = if horizontal { y } else { x } + skip;
        p.copy_from_slice(&if across < colored { c } else { [0; 4] });
    }
}

/// Text cursor after the last typed character.
fn caret(pm: &mut PixmapMut, ctx: &Ctx, it: &Item, fonts: &mut Fonts) {
    let Kind::Text { at, text } = &it.kind else { return };
    let size = annot::text_size(&it.style) * ctx.k;
    let lines = text.split('\n').count();
    let last = text.rsplit('\n').next().unwrap_or("");
    let lh = Fonts::line_height(size);
    let w = if last.is_empty() { 0.0 } else { fonts.line_width(last, size) };
    let x = at.x * ctx.tf.sx + ctx.tf.tx + w + 1.0 * ctx.k;
    let y = at.y * ctx.tf.sy + ctx.tf.ty + (lines - 1) as f32 * lh;
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y + lh * 0.08, (1.6 * ctx.k).max(1.0).round(), lh * 0.84) {
        let mut p = Paint::default();
        p.set_color_rgba8(it.style.color[0], it.style.color[1], it.style.color[2], 255);
        p.anti_alias = false;
        pm.fill_rect(r, &p, Transform::identity(), None);
    }
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
    /// An output went away while we are open: close, the capture is stale.
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outcome = Some(Outcome::Cancel);
    }
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.outcome = Some(Outcome::Cancel);
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        if let Some(i) = self.surfs.iter().position(|s| s.layer.wl_surface() == layer.wl_surface()) {
            if !self.surfs[i].configured {
                self.surfs[i].configured = true;
                self.draw(qh, i);
                trace(|| format!("first frame on {}", self.surfs[i].name));
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
            self.keyboard = self.seats.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), repeat).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            // dedicated surface for the cursor image
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
        let phys = e.raw_code;
        let (ctrl, shift, alt) = (self.mods.ctrl, self.mods.shift, self.mods.alt);
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
                    self.touch_current();
                }
                Keysym::Return | Keysym::KP_Enter => {
                    if let Some(Item { kind: Kind::Text { text, .. }, .. }) = &mut self.current {
                        text.push('\n');
                    }
                    self.touch_current();
                }
                _ => match e.utf8.as_deref() {
                    Some(s) if !s.chars().any(char::is_control) => {
                        if let Some(Item { kind: Kind::Text { text, .. }, .. }) = &mut self.current {
                            text.push_str(s);
                        }
                        self.touch_current();
                    }
                    _ => changed = false,
                },
            }
            if changed {
                self.schedule(qh);
            }
            return;
        }

        if e.keysym == Keysym::space {
            self.space = true;
            self.update_cursor(conn);
            return;
        }
        match self.cfg.keys.lookup(phys, ctrl, shift, alt) {
            Some(Cmd::Cancel) => self.outcome = Some(Outcome::Cancel),
            Some(Cmd::Confirm) => self.finish(),
            Some(Cmd::Undo) => {
                self.undo();
                self.schedule(qh);
            }
            Some(Cmd::Redo) => {
                self.redo();
                self.schedule(qh);
            }
            Some(Cmd::Tool(t)) if self.sel.is_some() => {
                self.select_tool(t);
                self.schedule(qh);
            }
            Some(Cmd::Tool(_)) => {}
            None => {
                // 1-9: palette colors
                let n = self.cfg.draw.palette.len().min(9) as u32;
                if !ctrl && !alt && self.sel.is_some() && (2..2 + n).contains(&phys) {
                    self.do_action(Action::Color(phys as usize - 2), qh);
                }
            }
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
                                self.touch_current();
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
                            self.touch_current();
                        }
                        self.schedule(qh);
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
