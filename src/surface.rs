//! Building blocks of the overlay: subsurfaces the compositor composes for
//! us. A `Quad` is a solid-color rectangle (a 1x1 buffer stretched by
//! wp_viewporter), a `Panel` a small buffer redrawn only when its content
//! changes, an `Anno` the annotation layer with damage tracking. Moving or
//! resizing any of them costs a few protocol messages and no pixel work.

use smithay_client_toolkit::{
    compositor::{CompositorState, Region, SurfaceData},
    shm::{
        Shm,
        slot::{Buffer, SlotPool},
    },
    subcompositor::{SubcompositorState, SubsurfaceData},
};
use wayland_client::{
    Dispatch, QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_shm, wl_subsurface::WlSubsurface, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

use crate::geom::Rect;

/// Integer rectangle in surface-local logical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Round a float rectangle to whole pixels.
    pub fn round(r: Rect) -> Self {
        let r = r.rounded();
        Self::new(r.x as i32, r.y as i32, r.w as i32, r.h as i32)
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn clip(self, b: IRect) -> Option<IRect> {
        let (x0, y0) = (self.x.max(b.x), self.y.max(b.y));
        let (x1, y1) = (self.right().min(b.right()), self.bottom().min(b.bottom()));
        (x1 > x0 && y1 > y0).then(|| IRect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// The globals every subsurface needs.
pub struct Env {
    pub compositor: CompositorState,
    pub subcompositor: SubcompositorState,
    pub viewporter: WpViewporter,
}

/// A synchronized subsurface that never takes pointer input (so pointer
/// events always arrive on the parent) and has a viewport.
pub struct Sub {
    pub surface: WlSurface,
    pub sub: WlSubsurface,
    pub viewport: WpViewport,
}

impl Sub {
    pub fn new<S>(env: &Env, parent: &WlSurface, qh: &QueueHandle<S>) -> Sub
    where
        S: Dispatch<WlSurface, SurfaceData<()>>
            + Dispatch<WlSubsurface, SubsurfaceData>
            + Dispatch<WpViewport, ()>
            + 'static,
    {
        let (sub, surface) = env.subcompositor.create_subsurface(parent.clone(), qh);
        sub.set_sync();
        if let Ok(empty) = Region::new(&env.compositor) {
            surface.set_input_region(Some(empty.wl_region()));
        }
        let viewport = env.viewporter.get_viewport(&surface, qh, ());
        Sub { surface, sub, viewport }
    }

    pub fn place(&self, old: Option<IRect>, r: IRect) {
        if old.map(|o| (o.x, o.y)) != Some((r.x, r.y)) {
            self.sub.set_position(r.x, r.y);
        }
        if old.map(|o| (o.w, o.h)) != Some((r.w, r.h)) {
            self.viewport.set_destination(r.w, r.h);
        }
    }

    pub fn hide(&self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
    }
}

impl Drop for Sub {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.sub.destroy();
        self.surface.destroy();
    }
}

/// Solid rectangle. Its 1x1 buffer is shared with every quad of that color.
pub struct Quad {
    s: Sub,
    buf: WlBuffer,
    rect: Option<IRect>,
}

impl Quad {
    pub fn new<S>(env: &Env, parent: &WlSurface, buf: WlBuffer, qh: &QueueHandle<S>) -> Quad
    where
        S: Dispatch<WlSurface, SurfaceData<()>>
            + Dispatch<WlSubsurface, SubsurfaceData>
            + Dispatch<WpViewport, ()>
            + 'static,
    {
        Quad { s: Sub::new(env, parent, qh), buf, rect: None }
    }

    /// Show at `r`, or hide for `None`. Nothing is sent when nothing changed.
    pub fn set(&mut self, r: Option<IRect>) {
        let r = r.filter(|r| !r.is_empty());
        if r == self.rect {
            return;
        }
        match r {
            None => self.s.hide(),
            Some(r) => {
                // attach again on every change: Hyprland ignores a new
                // viewport size on an already mapped subsurface otherwise
                self.s.surface.attach(Some(&self.buf), 0, 0);
                self.s.surface.damage_buffer(0, 0, 1, 1);
                self.s.place(self.rect, r);
                self.s.surface.commit();
            }
        }
        self.rect = r;
    }
}

/// A small ARGB buffer shown at a position, redrawn only when `key` or its
/// pixel size changes. Moving it is free.
pub struct Panel {
    s: Sub,
    pool: SlotPool,
    buf: Option<Buffer>,
    rect: Option<IRect>,
    key: u64,
    px: (i32, i32),
}

impl Panel {
    pub fn new<S>(env: &Env, parent: &WlSurface, shm: &Shm, qh: &QueueHandle<S>) -> anyhow::Result<Panel>
    where
        S: Dispatch<WlSurface, SurfaceData<()>>
            + Dispatch<WlSubsurface, SubsurfaceData>
            + Dispatch<WpViewport, ()>
            + 'static,
    {
        Ok(Panel {
            s: Sub::new(env, parent, qh),
            pool: SlotPool::new(4096, shm)?,
            buf: None,
            rect: None,
            key: 0,
            px: (0, 0),
        })
    }

    /// `rect` is where it sits (logical), `px` the buffer size in physical
    /// pixels. `render` fills a fresh buffer when the content is new.
    pub fn set(&mut self, rect: Option<IRect>, px: (i32, i32), key: u64, render: impl FnOnce(&mut [u8])) {
        let rect = rect.filter(|r| !r.is_empty() && px.0 > 0 && px.1 > 0);
        let Some(r) = rect else {
            if self.rect.take().is_some() {
                self.s.hide();
                self.buf = None;
            }
            return;
        };
        let fresh = self.buf.is_none() || self.key != key || self.px != px;
        if !fresh && self.rect == Some(r) {
            return;
        }
        if fresh {
            let Ok((buf, canvas)) = self.pool.create_buffer(px.0, px.1, px.0 * 4, wl_shm::Format::Argb8888) else {
                return;
            };
            render(canvas);
            self.s.surface.attach(Some(buf.wl_buffer()), 0, 0);
            self.s.surface.damage_buffer(0, 0, px.0, px.1);
            self.buf = Some(buf);
            self.key = key;
            self.px = px;
        }
        self.s.place(self.rect, r);
        self.s.surface.commit();
        self.rect = Some(r);
    }
}

/// The annotation layer of one output: covers the selection, holds two
/// buffers and redraws only what changed.
pub struct Anno {
    s: Sub,
    pool: SlotPool,
    bufs: [Option<Buffer>; 2],
    /// Physical-pixel areas each buffer is behind the newest picture.
    stale: [Option<IRect>; 2],
    shown: usize,
    rect: Option<IRect>,
    px: (i32, i32),
}

impl Anno {
    pub fn new<S>(env: &Env, parent: &WlSurface, shm: &Shm, qh: &QueueHandle<S>) -> anyhow::Result<Anno>
    where
        S: Dispatch<WlSurface, SurfaceData<()>>
            + Dispatch<WlSubsurface, SubsurfaceData>
            + Dispatch<WpViewport, ()>
            + 'static,
    {
        Ok(Anno {
            s: Sub::new(env, parent, qh),
            pool: SlotPool::new(4096, shm)?,
            bufs: [None, None],
            stale: [None, None],
            shown: 0,
            rect: None,
            px: (0, 0),
        })
    }

    pub fn is_shown(&self) -> bool {
        self.rect.is_some()
    }

    /// Hide the layer and give its memory back.
    pub fn clear(&mut self) {
        if self.rect.take().is_some() {
            self.s.hide();
        }
        self.bufs = [None, None];
        self.stale = [None, None];
    }

    /// Show the layer at `rect` (logical, surface-local) with `px` pixels.
    /// `dirty` is the physical-pixel area (relative to the layer) that changed,
    /// `None` meaning nothing. `render(canvas_rgba, region)` must draw the
    /// layer's content for `region` into a transparent premultiplied RGBA
    /// pixmap of exactly that size. Returns false when no buffer was free and
    /// the caller should retry.
    pub fn update(
        &mut self,
        rect: IRect,
        px: (i32, i32),
        dirty: Option<IRect>,
        render: &mut dyn FnMut(&mut tiny_skia::Pixmap, IRect),
    ) -> bool {
        let full = IRect::new(0, 0, px.0, px.1);
        let mut dirty = dirty.and_then(|d| d.clip(full));
        if self.px != px || self.bufs.iter().all(Option::is_none) {
            // new size: start over
            self.bufs = [None, None];
            self.px = px;
            dirty = Some(full);
            self.stale = [Some(full), Some(full)];
        }
        let moved = self.rect != Some(rect);
        if dirty.is_none() {
            if moved {
                self.s.place(self.rect, rect);
                self.s.surface.commit();
                self.rect = Some(rect);
            }
            return true;
        }
        let d = dirty.unwrap();

        // draw into the buffer that is not on screen, if the compositor released it
        let other = 1 - self.shown;
        let target = [other, self.shown].into_iter().find(|&i| {
            match &self.bufs[i] {
                Some(b) => b.canvas(&mut self.pool).is_some(),
                None => true,
            }
        });
        let Some(t) = target else { return false };
        if self.bufs[t].is_none() {
            let Ok((b, _)) = self.pool.create_buffer(px.0, px.1, px.0 * 4, wl_shm::Format::Argb8888) else {
                return false;
            };
            self.bufs[t] = Some(b);
            self.stale[t] = Some(full);
        }
        let region = union(Some(d), self.stale[t]).and_then(|r| r.clip(full)).unwrap_or(d);
        let Some(mut tmp) = tiny_skia::Pixmap::new(region.w as u32, region.h as u32) else { return true };
        render(&mut tmp, region);
        let buf = self.bufs[t].as_ref().unwrap();
        let Some(canvas) = buf.canvas(&mut self.pool) else { return false };
        // RGBA -> BGRA rows into the buffer
        for (row, src) in tmp.data().chunks_exact(region.w as usize * 4).enumerate() {
            let at = ((region.y as usize + row) * px.0 as usize + region.x as usize) * 4;
            for (d, s) in canvas[at..at + src.len()].chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        let buf = self.bufs[t].as_ref().unwrap();
        let _ = buf.attach_to(&self.s.surface);
        self.s.surface.damage_buffer(d.x, d.y, d.w, d.h);
        self.s.place(self.rect, rect);
        self.s.surface.commit();
        self.rect = Some(rect);
        self.stale[t] = None;
        self.stale[1 - t] = union(self.stale[1 - t], Some(d));
        self.shown = t;
        true
    }
}

fn union(a: Option<IRect>, b: Option<IRect>) -> Option<IRect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
            Some(IRect::new(x0, y0, x1 - x0, y1 - y0))
        }
        (a, None) => a,
        (None, b) => b,
    }
}

/// The 1x1 colors shared by all quads.
pub struct Solids {
    _pool: SlotPool,
    bufs: Vec<Buffer>,
}

impl Solids {
    /// One buffer per ARGB color, in the order given.
    pub fn new(shm: &Shm, colors: &[u32]) -> anyhow::Result<Solids> {
        let mut pool = SlotPool::new(4096, shm)?;
        let mut bufs = Vec::new();
        for &c in colors {
            let (b, canvas) = pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888)?;
            canvas[..4].copy_from_slice(&c.to_le_bytes());
            bufs.push(b);
        }
        Ok(Solids { _pool: pool, bufs })
    }

    pub fn get(&self, i: usize) -> WlBuffer {
        self.bufs[i].wl_buffer().clone()
    }
}
