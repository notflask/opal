//! Dev tool: inject pointer/keyboard input through virtual-input protocols.
//!
//!   cargo run --example inject -- "m 500 300" "down" "m 800 450" "up" "tap 28"
//!
//! Commands: `m X Y` (absolute, compositor space), `down`/`up` (left button),
//! `rdown`/`rup`, `tap KEY`, `kdown KEY`, `kup KEY` (evdev codes),
//! `mods N` (xkb modifier mask, ctrl=4 shift=1 alt=8), `sleep MS`.

use std::{
    fs::File,
    io::Write,
    os::fd::AsFd,
    thread::sleep,
    time::{Duration, Instant},
};

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

struct S;

macro_rules! noop {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for S {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
noop!(wl_seat::WlSeat, ZwpVirtualKeyboardManagerV1, ZwpVirtualKeyboardV1, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for S {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

const KEYMAP: &str = r#"xkb_keymap {
 xkb_keycodes { include "evdev+aliases(qwerty)" };
 xkb_types { include "complete" };
 xkb_compat { include "complete" };
 xkb_symbols { include "pc+us+inet(evdev)" };
};
"#;

// total layout size, for absolute motion
const EXTENT: (u32, u32) = (4480, 1440);

fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut queue) = registry_queue_init::<S>(&conn).unwrap();
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
    let ptr_mgr: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
    let kb_mgr: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
    let ptr = ptr_mgr.create_virtual_pointer(Some(&seat), &qh, ());
    let kb = kb_mgr.create_virtual_keyboard(&seat, &qh, ());

    let path = std::env::temp_dir().join(format!("opal-inject-{}.xkb", std::process::id()));
    File::create(&path).unwrap().write_all(KEYMAP.as_bytes()).unwrap();
    let f = File::open(&path).unwrap();
    kb.keymap(1, f.as_fd(), KEYMAP.len() as u32 + 1);
    let _ = std::fs::remove_file(&path);

    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u32;
    let mut s = S;
    // no arguments: read one command per line from stdin (devices stay alive)
    let cmds: Vec<String> = if std::env::args().len() > 1 {
        std::env::args().skip(1).collect()
    } else {
        Vec::new()
    };
    let stdin = std::io::stdin();
    let mut lines = stdin.lines();
    let mut next = cmds.into_iter();
    loop {
        let cmd = match next.next() {
            Some(c) => c,
            None => match lines.next() {
                Some(Ok(l)) if !l.trim().is_empty() => l,
                Some(Ok(_)) => continue,
                _ => break,
            },
        };
        let p: Vec<&str> = cmd.split_whitespace().collect();
        match p[..] {
            ["m", x, y] => {
                ptr.motion_absolute(now(), x.parse().unwrap(), y.parse().unwrap(), EXTENT.0, EXTENT.1);
                ptr.frame();
            }
            ["click"] => {
                for st in [wayland_client::protocol::wl_pointer::ButtonState::Pressed, wayland_client::protocol::wl_pointer::ButtonState::Released] {
                    ptr.button(now(), 0x110, st);
                    ptr.frame();
                    conn.flush().unwrap();
                    sleep(Duration::from_millis(30));
                }
            }
            ["down"] | ["up"] | ["rdown"] | ["rup"] => {
                let btn = if p[0].starts_with('r') { 0x111 } else { 0x110 };
                let st = if p[0].ends_with("down") { 1 } else { 0 };
                ptr.button(now(), btn, if st == 1 { wayland_client::protocol::wl_pointer::ButtonState::Pressed } else { wayland_client::protocol::wl_pointer::ButtonState::Released });
                ptr.frame();
            }
            ["tap", k] => {
                kb.key(now(), k.parse().unwrap(), 1);
                conn.flush().unwrap();
                sleep(Duration::from_millis(30));
                kb.key(now(), k.parse().unwrap(), 0);
            }
            ["kdown", k] => kb.key(now(), k.parse().unwrap(), 1),
            ["kup", k] => kb.key(now(), k.parse().unwrap(), 0),
            ["mods", m] => kb.modifiers(m.parse().unwrap(), 0, 0, 0),
            ["sleep", ms] => {
                conn.flush().unwrap();
                sleep(Duration::from_millis(ms.parse().unwrap()));
            }
            _ => panic!("bad command {cmd:?}"),
        }
        conn.flush().unwrap();
        queue.roundtrip(&mut s).unwrap();
        sleep(Duration::from_millis(25));
    }
}
