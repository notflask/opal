//! Which output has focus right now (Hyprland, Niri). None if unknown.

use std::process::Command;

pub fn focused_output() -> Option<String> {
    hyprland().or_else(niri)
}

fn run(cmd: &str, args: &[&str]) -> Option<serde_json::Value> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn hyprland() -> Option<String> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    run("hyprctl", &["-j", "monitors"])?
        .as_array()?
        .iter()
        .find(|m| m["focused"].as_bool() == Some(true))?["name"]
        .as_str()
        .map(str::to_owned)
}

fn niri() -> Option<String> {
    std::env::var_os("NIRI_SOCKET")?;
    run("niri", &["msg", "--json", "focused-output"])?["name"]
        .as_str()
        .map(str::to_owned)
}
