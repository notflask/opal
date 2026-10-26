//! Talking to the compositor's own socket: which output has focus or holds
//! the pointer, and Hyprland requests. No child processes.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_millis(150);

fn request(path: PathBuf, msg: &str) -> Option<String> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(TIMEOUT)).ok()?;
    s.set_write_timeout(Some(TIMEOUT)).ok()?;
    s.write_all(msg.as_bytes()).ok()?;
    let mut out = String::new();
    // Hyprland closes the socket after its reply, niri ends it with a newline
    let mut buf = [0u8; 4096];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.ends_with('\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (!out.is_empty()).then_some(out)
}

fn hypr_socket() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let run = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(run).join("hypr").join(sig).join(".socket.sock"))
}

pub fn is_hyprland() -> bool {
    hypr_socket().is_some()
}

/// Send a raw Hyprland request such as `j/monitors` or `/keyword a:b c`.
pub fn hypr(msg: &str) -> Option<String> {
    request(hypr_socket()?, msg)
}

fn hypr_json(msg: &str) -> Option<Value> {
    serde_json::from_str(&hypr(msg)?).ok()
}

fn niri(msg: &str) -> Option<Value> {
    let path = PathBuf::from(std::env::var_os("NIRI_SOCKET")?);
    let v: Value = serde_json::from_str(request(path, msg)?.trim()).ok()?;
    v.get("Ok").cloned()
}

/// Name of the focused output.
pub fn focused_output() -> Option<String> {
    if is_hyprland() {
        let m = hypr_json("j/monitors")?;
        return m.as_array()?.iter().find(|m| m["focused"].as_bool() == Some(true))?["name"]
            .as_str()
            .map(str::to_owned);
    }
    niri("\"FocusedOutput\"\n")?["FocusedOutput"]["name"].as_str().map(str::to_owned)
}

/// Name of the output under the pointer (Hyprland), else the focused one.
pub fn pointer_output() -> Option<String> {
    // dev hook for tests
    if let Ok(name) = std::env::var("OPAL_OUTPUT") {
        return Some(name);
    }
    if is_hyprland() {
        let c = hypr_json("j/cursorpos")?;
        let (cx, cy) = (c["x"].as_f64()?, c["y"].as_f64()?);
        let m = hypr_json("j/monitors")?;
        for m in m.as_array()? {
            let (x, y) = (m["x"].as_f64()?, m["y"].as_f64()?);
            let s = m["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
            let (w, h) = (m["width"].as_f64()? / s, m["height"].as_f64()? / s);
            if cx >= x && cx < x + w && cy >= y && cy < y + h {
                return m["name"].as_str().map(str::to_owned);
            }
        }
    }
    focused_output()
}
