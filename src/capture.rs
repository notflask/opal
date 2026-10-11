//! Grab every output with wlr-screencopy into CPU pixmaps.

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
/// the pixmap is in physical pixels.
pub struct Shot {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub lw: i32,
    pub lh: i32,
    pub pixmap: Pixmap,
}

impl Shot {
    /// Physical pixels per logical pixel.
    pub fn scale(&self) -> f32 {
        self.pixmap.width() as f32 / self.lw as f32
    }
}

struct Frame {
    output: usize,
    format: Option<(wl_shm::Format, u32, u32, u32)>,
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

pub fn capture_all() -> Result<Vec<Shot>> {
    let conn = Connection::connect_to_env().context("no Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();

    let manager: ZwlrScreencopyManagerV1 = globals
        .bind(&qh, 1..=3, ())
        .context("compositor lacks wlr-screencopy")?;
    let mut app = Capture {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).context("no wl_shm")?,
        frames: Vec::new(),
    };
    // First roundtrip binds the outputs, second delivers their info
    queue.roundtrip(&mut app)?;
    queue.roundtrip(&mut app)?;

    let outputs: Vec<wl_output::WlOutput> = app.outputs.outputs().collect();
    if outputs.is_empty() {
        bail!("no outputs");
    }
    for (i, out) in outputs.iter().enumerate() {
        app.frames.push(Frame {
            output: i,
            format: None,
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

    let mut shots = Vec::new();
    for f in &mut app.frames {
        let info = app
            .outputs
            .info(&outputs[f.output])
            .context("output info missing")?;
        if f.result != Some(true) {
            bail!("screencopy failed for {:?}", info.name);
        }
        let (fmt, w, h, stride) = f.format.unwrap();
        let pool = f.pool.as_mut().unwrap();
        let pixmap = to_pixmap(pool.mmap(), fmt, w, h, stride, f.y_invert)?;
        let (lw, lh) = info.logical_size.unwrap_or((w as i32, h as i32));
        let (x, y) = info
            .logical_position
            .or(Some(info.location))
            .unwrap_or((0, 0));
        shots.push(Shot {
            name: info.name.clone().unwrap_or_else(|| format!("output-{}", info.id)),
            x,
            y,
            lw,
            lh,
            pixmap,
        });
        if let Some(b) = f.buffer.take() {
            b.destroy();
        }
    }
    shots.sort_by_key(|s| (s.x, s.y));
    Ok(shots)
}

/// Convert the compositor's shm pixels to an opaque premultiplied RGBA pixmap.
fn to_pixmap(
    src: &[u8],
    fmt: wl_shm::Format,
    w: u32,
    h: u32,
    stride: u32,
    y_invert: bool,
) -> Result<Pixmap> {
    let mut pm = Pixmap::new(w, h).context("empty capture")?;
    let dst = pm.data_mut();
    for row in 0..h as usize {
        let srow = if y_invert { h as usize - 1 - row } else { row };
        let s = &src[srow * stride as usize..][..w as usize * 4];
        let d = &mut dst[row * w as usize * 4..][..w as usize * 4];
        for (sp, dp) in s.chunks_exact(4).zip(d.chunks_exact_mut(4)) {
            let (r, g, b) = match fmt {
                wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => (sp[2], sp[1], sp[0]),
                wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => (sp[0], sp[1], sp[2]),
                wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => {
                    let v = u32::from_le_bytes([sp[0], sp[1], sp[2], sp[3]]);
                    ((v >> 22) as u8, (v >> 12) as u8, (v >> 2) as u8)
                }
                wl_shm::Format::Xbgr2101010 | wl_shm::Format::Abgr2101010 => {
                    let v = u32::from_le_bytes([sp[0], sp[1], sp[2], sp[3]]);
                    ((v >> 2) as u8, (v >> 12) as u8, (v >> 22) as u8)
                }
                other => bail!("unsupported shm format {other:?}"),
            };
            dp.copy_from_slice(&[r, g, b, 255]);
        }
    }
    Ok(pm)
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Capture {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Capture {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
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
                // keep the first offered shm format
                if state.frames[slot].format.is_none() {
                    if let WEnum::Value(f) = format {
                        state.frames[slot].format = Some((f, width, height, stride));
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
            E::Ready { .. } => state.frames[slot].result = Some(true),
            E::Failed => state.frames[slot].result = Some(false),
            _ => {}
        }
    }
}

fn start_copy(
    state: &mut Capture,
    frame: &ZwlrScreencopyFrameV1,
    slot: usize,
    qh: &QueueHandle<Capture>,
) {
    let Some((fmt, w, h, stride)) = state.frames[slot].format else {
        state.frames[slot].result = Some(false);
        return;
    };
    let Ok(mut pool) = RawPool::new((stride * h) as usize, &state.shm) else {
        state.frames[slot].result = Some(false);
        return;
    };
    let buf = pool.create_buffer(0, w as i32, h as i32, stride as i32, fmt, (), qh);
    frame.copy(&buf);
    state.frames[slot].pool = Some(pool);
    state.frames[slot].buffer = Some(buf);
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
