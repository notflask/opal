//! Grab outputs with wlr-screencopy. The compositor writes into a shm buffer
//! we keep: the overlay attaches that very buffer as the background, so the
//! screenshot is never copied.

use anyhow::{Context, Result, bail};
use smithay_client_toolkit::{
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shm::{Shm, ShmHandler, raw::RawPool},
};
use tiny_skia::Pixmap;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, Flags, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// One captured output. `x`/`y`/`lw`/`lh` are logical (compositor space),
/// the pixels are physical.
pub struct Shot {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub lw: i32,
    pub lh: i32,
    pub w: u32,
    pub h: u32,
    stride: u32,
    pub format: wl_shm::Format,
    /// Keeps the memory and the wl_shm_pool alive.
    pool: RawPool,
    ptr: *const u8,
    len: usize,
    pub buffer: wl_buffer::WlBuffer,
}

impl Drop for Shot {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl Shot {
    /// Physical pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.w as f32 / self.lw as f32
    }

    pub fn logical(&self) -> crate::geom::Rect {
        crate::geom::Rect::new(self.x as f32, self.y as f32, self.lw as f32, self.lh as f32)
    }

    /// The compositor is done writing before a `Shot` exists, and the mapping
    /// lives as long as `_pool`, which is never resized.
    fn data(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Does the format have an alpha channel we must not trust?
    pub fn has_alpha(&self) -> bool {
        matches!(self.format, wl_shm::Format::Argb8888 | wl_shm::Format::Abgr8888)
    }

    /// Raw pixels, `stride` bytes per row, in `format`.
    pub fn bytes(&self) -> &[u8] {
        self.data()
    }

    pub fn stride(&self) -> usize {
        self.stride as usize
    }

    pub fn rgb(&self, px: u32, py: u32) -> [u8; 3] {
        let (px, py) = (px.min(self.w - 1), py.min(self.h - 1));
        let i = py as usize * self.stride as usize + px as usize * 4;
        pixel_rgb(self.format, &self.data()[i..i + 4])
    }

    /// Opaque premultiplied RGBA copy of a physical-pixel rectangle.
    pub fn read_rgba(&self, x: u32, y: u32, w: u32, h: u32) -> Option<Pixmap> {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        if x >= x1 || y >= y1 {
            return None;
        }
        let mut pm = Pixmap::new(x1 - x, y1 - y)?;
        let ow = (x1 - x) as usize;
        let dst = pm.data_mut();
        let src = self.data();
        for (row, drow) in dst.chunks_exact_mut(ow * 4).enumerate() {
            let s = &src[(y as usize + row) * self.stride as usize + x as usize * 4..][..ow * 4];
            for (sp, dp) in s.chunks_exact(4).zip(drow.chunks_exact_mut(4)) {
                let [r, g, b] = pixel_rgb(self.format, sp);
                dp.copy_from_slice(&[r, g, b, 255]);
            }
        }
        Some(pm)
    }
}

fn pixel_rgb(fmt: wl_shm::Format, p: &[u8]) -> [u8; 3] {
    match fmt {
        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => [p[2], p[1], p[0]],
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => [p[0], p[1], p[2]],
        wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => {
            let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            [(v >> 22) as u8, (v >> 12) as u8, (v >> 2) as u8]
        }
        wl_shm::Format::Xbgr2101010 | wl_shm::Format::Abgr2101010 => {
            let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            [(v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8]
        }
        _ => [0, 0, 0],
    }
}

fn supported(fmt: wl_shm::Format) -> bool {
    use wl_shm::Format as F;
    matches!(
        fmt,
        F::Argb8888 | F::Xrgb8888 | F::Abgr8888 | F::Xbgr8888 | F::Xrgb2101010 | F::Argb2101010 | F::Xbgr2101010 | F::Abgr2101010
    )
}

/// Formats the overlay can attach unchanged.
fn attachable(fmt: wl_shm::Format) -> bool {
    use wl_shm::Format as F;
    matches!(fmt, F::Argb8888 | F::Xrgb8888 | F::Abgr8888 | F::Xbgr8888)
}

struct Frame {
    output: usize,
    offers: Vec<(wl_shm::Format, u32, u32, u32)>,
    chosen: Option<(wl_shm::Format, u32, u32, u32)>,
    pool: Option<RawPool>,
    buffer: Option<wl_buffer::WlBuffer>,
    y_invert: bool,
    result: Option<bool>,
}

struct Capture {
    registry: RegistryState,
    outputs: OutputState,
    shm: Shm,
    frames: Vec<Frame>,
}

/// Capture every output, or only the one called `only`.
pub fn capture(conn: &Connection, only: Option<&str>) -> Result<Vec<Shot>> {
    let (globals, mut queue) = registry_queue_init(conn)?;
    let qh = queue.handle();

    let manager: ZwlrScreencopyManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .context("the compositor lacks wlr-screencopy")?;
    let mut app = Capture {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).context("the compositor lacks wl_shm")?,
        frames: Vec::new(),
    };
    // first roundtrip binds the outputs and wl_shm, second delivers their info
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;

    let mut outputs: Vec<wl_output::WlOutput> = app.outputs.outputs().collect();
    if let Some(name) = only {
        outputs.retain(|o| app.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name));
        if outputs.is_empty() {
            bail!("no output called {name:?}");
        }
    }
    if outputs.is_empty() {
        bail!("no outputs");
    }

    let mut attempt = 0;
    loop {
        app.frames.clear();
        for (i, out) in outputs.iter().enumerate() {
            app.frames.push(Frame {
                output: i,
                offers: Vec::new(),
                chosen: None,
                pool: None,
                buffer: None,
                y_invert: false,
                result: None,
            });
            manager.capture_output(0, out, &qh, i);
        }
        while app.frames.iter().any(|f| f.result.is_none()) {
            queue.blocking_dispatch(&mut app)?;
        }
        if app.frames.iter().all(|f| f.result == Some(true)) {
            break;
        }
        attempt += 1;
        if attempt >= 2 {
            bail!("screencopy failed");
        }
        for f in &mut app.frames {
            if let Some(b) = f.buffer.take() {
                b.destroy();
            }
        }
    }

    let shm_formats = app.shm.formats().to_vec();
    let mut shots = Vec::new();
    for f in &mut app.frames {
        let out = &outputs[f.output];
        let info = app.outputs.info(out).context("output info missing")?;
        let (fmt, w, h, stride) = f.chosen.context("no buffer format")?;
        let pool = f.pool.take().context("no pool")?;
        let buffer = f.buffer.take().context("no buffer")?;
        let (lw, lh) = info.logical_size.unwrap_or((w as i32, h as i32));
        let (x, y) = info.logical_position.or(Some(info.location)).unwrap_or((0, 0));
        let mut shot = Shot {
            name: info.name.clone().unwrap_or_else(|| format!("output-{}", info.id)),
            x,
            y,
            lw,
            lh,
            w,
            h,
            stride,
            format: fmt,
            ptr: std::ptr::null(),
            len: 0,
            buffer,
            pool,
        };
        let m = shot.pool.mmap();
        shot.ptr = m.as_ptr();
        shot.len = m.len();
        let direct = !f.y_invert && attachable(fmt) && shm_formats.contains(&fmt);
        if !direct {
            // 10-bit or upside-down capture: one conversion into Xrgb8888
            shot = convert(shot, &app.shm, &qh, f.y_invert)?;
        }
        shots.push(shot);
    }
    shots.sort_by_key(|s| (s.x, s.y));
    Ok(shots)
}

/// Rewrite a capture into a top-down Xrgb8888 pool of its own.
fn convert(old: Shot, shm: &Shm, qh: &QueueHandle<Capture>, y_invert: bool) -> Result<Shot> {
    let (w, h) = (old.w, old.h);
    let stride = w * 4;
    let mut pool = RawPool::new((stride * h) as usize, shm).context("shm pool")?;
    let src = old.data();
    let dst = pool.mmap();
    for row in 0..h as usize {
        let srow = if y_invert { h as usize - 1 - row } else { row };
        let s = &src[srow * old.stride as usize..][..w as usize * 4];
        let d = &mut dst[row * w as usize * 4..][..w as usize * 4];
        for (sp, dp) in s.chunks_exact(4).zip(d.chunks_exact_mut(4)) {
            let [r, g, b] = pixel_rgb(old.format, sp);
            dp.copy_from_slice(&[b, g, r, 255]);
        }
    }
    let (ptr, len) = (pool.mmap().as_ptr(), pool.mmap().len());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Xrgb8888, (), qh);
    Ok(Shot {
        name: old.name.clone(),
        x: old.x,
        y: old.y,
        lw: old.lw,
        lh: old.lh,
        w,
        h,
        stride,
        format: wl_shm::Format::Xrgb8888,
        pool,
        ptr,
        len,
        buffer,
    })
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Capture {
    fn event(_: &mut Self, _: &ZwlrScreencopyManagerV1, _: <ZwlrScreencopyManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Capture {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for Capture {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        idx: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event as E;
        let slot = *idx;
        match event {
            E::Buffer { format, width, height, stride } => {
                if let WEnum::Value(f) = format {
                    if supported(f) {
                        state.frames[slot].offers.push((f, width, height, stride));
                    }
                }
                // protocol v1/v2 has no BufferDone: copy right away
                if frame.version() < 3 {
                    start_copy(state, frame, slot, qh);
                }
            }
            E::BufferDone => start_copy(state, frame, slot, qh),
            E::Flags { flags } => {
                if let WEnum::Value(fl) = flags {
                    state.frames[slot].y_invert = fl.contains(Flags::YInvert);
                }
            }
            E::Ready { .. } => {
                state.frames[slot].result = Some(true);
                frame.destroy();
            }
            E::Failed => {
                state.frames[slot].result = Some(false);
                frame.destroy();
            }
            _ => {}
        }
    }
}

fn start_copy(state: &mut Capture, frame: &ZwlrScreencopyFrameV1, slot: usize, qh: &QueueHandle<Capture>) {
    if state.frames[slot].buffer.is_some() {
        return;
    }
    // the first format we can attach unchanged, else whatever came first
    let offers = &state.frames[slot].offers;
    let pick = [wl_shm::Format::Xrgb8888, wl_shm::Format::Argb8888, wl_shm::Format::Xbgr8888, wl_shm::Format::Abgr8888]
        .iter()
        .find_map(|f| offers.iter().find(|o| o.0 == *f).copied().filter(|o| state.shm.formats().contains(&o.0)))
        .or_else(|| offers.first().copied());
    let Some((fmt, w, h, stride)) = pick else {
        state.frames[slot].result = Some(false);
        return;
    };
    let Ok(mut pool) = RawPool::new((stride * h) as usize, &state.shm) else {
        state.frames[slot].result = Some(false);
        return;
    };
    let buf = pool.create_buffer(0, w as i32, h as i32, stride as i32, fmt, (), qh);
    frame.copy(&buf);
    let f = &mut state.frames[slot];
    f.chosen = Some((fmt, w, h, stride));
    f.pool = Some(pool);
    f.buffer = Some(buf);
}

impl OutputHandler for Capture {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Capture {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Capture {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(Capture);
delegate_dispatch2!(Capture);
