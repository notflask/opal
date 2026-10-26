mod annot;
mod capture;
mod config;
mod geom;
mod ipc;
mod lock;
mod output;
mod overlay;
mod pixfont;
mod sig;
mod surface;
mod text;
mod theme;
mod ui;
mod zoom;

use anyhow::{Context, Result, bail};
use wayland_client::Connection;

use crate::{
    config::Config,
    geom::Rect,
    overlay::{Options, Outcome},
};

const USAGE: &str = "opal: Wayland screenshot tool

USAGE: opal <command>

  region [--no-edit]   drag an area (may span monitors), draw, Enter to copy+save
  output [--no-edit]   the focused monitor
  all [--no-edit]      every monitor, stitched into one image
  zoom                 freeze the screen, zoom and pan it (wheel, drag, F flashlight)
  zoom --live [in|out|reset]
                       Hyprland only: zoom the live screen
  config               print the default config.toml
  config --check [F]   check F (default: your config.toml), exit 1 on problems

--no-edit skips the editor and copies+saves at once.";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(output::SERVE_CLIPBOARD) {
        return output::serve_clipboard();
    }
    let mut mode = None;
    let mut no_edit = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-edit" => no_edit = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "config" => return config_command(it.map(String::as_str).collect()),
            "zoom" => return zoom::run(&it.map(String::as_str).collect::<Vec<_>>(), Config::load()),
            "region" | "output" | "all" => mode = Some(a.clone()),
            _ => bail!("unknown argument {a:?}\n\n{USAGE}"),
        }
    }
    let Some(mode) = mode else { bail!("{USAGE}") };
    let cfg = Config::load();
    // a second `opal` closes the first one and quits
    let Some(_guard) = lock::acquire() else { return Ok(()) };

    let t0 = std::time::Instant::now();
    let tracing = std::env::var_os("OPAL_TRACE").is_some();
    let trace = |what: &str| {
        if tracing {
            eprintln!("[{:>5} ms] {what}", t0.elapsed().as_millis());
        }
    };

    let conn = Connection::connect_to_env().context("no Wayland compositor")?;
    let focused = ipc::focused_output();
    // `output --no-edit` needs only the focused monitor
    let only = if mode == "output" && no_edit { focused.as_deref() } else { None };
    let shots = match only.map(|n| capture::capture(&conn, Some(n))) {
        Some(Ok(s)) => s,
        _ => capture::capture(&conn, None)?,
    };
    trace("captured");
    let scale = output::scale(&shots);

    // Which part of the screen the result starts from.
    let preset = match mode.as_str() {
        "all" => Some(output::bounds(&shots)),
        "output" => {
            let shot = shots.iter().find(|s| Some(&s.name) == focused.as_ref()).unwrap_or(&shots[0]);
            Some(shot.logical())
        }
        // dev hook: start with a selection, `OPAL_PRESET=x,y,w,h` (logical px)
        _ => std::env::var("OPAL_PRESET").ok().and_then(|v| {
            let n: Vec<f32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            (n.len() == 4).then(|| Rect::new(n[0], n[1], n[2], n[3]))
        }),
    };

    let mut fonts = text::Fonts::new(&cfg.draw.font);
    let (rect, items, shots) = if no_edit && preset.is_some() {
        (preset.unwrap(), Vec::new(), shots)
    } else {
        trace("starting overlay");
        let theme = theme::Theme::load(&cfg.theme);
        let done = overlay::run(
            &conn,
            shots,
            cfg.clone(),
            theme,
            fonts,
            Options { preset, finish_on_release: no_edit, hint_output: focused, canvas_scale: scale },
        )?;
        trace("overlay closed");
        fonts = done.fonts;
        match done.outcome {
            Outcome::Cancel => return Ok(()),
            Outcome::Done { rect, items } => (rect, items, done.shots),
        }
    };

    let mut image = output::crop(&shots, rect)?;
    if !items.is_empty() {
        let s = output::scale(&shots);
        let ctx = annot::Ctx {
            tf: tiny_skia::Transform::from_row(s, 0.0, 0.0, s, -rect.x * s, -rect.y * s),
            k: s,
            shots: &shots,
            aa: cfg.draw.antialias,
            block: cfg.draw.pixelate_block,
        };
        annot::draw_all(&mut image.as_mut(), &ctx, &items, &mut fonts);
    }
    let png = output::encode_png(&image)?;
    trace("encoded");
    let path = output::finish(&cfg.output, &png)?;
    trace("done");
    if let (Some(p), true) = (path, cfg.output.print_path) {
        println!("{}", p.display());
    }
    Ok(())
}

fn config_command(args: Vec<&str>) -> Result<()> {
    match args.as_slice() {
        [] => {
            print!("{}", config::DEFAULT_TOML);
            Ok(())
        }
        ["--check", rest @ ..] => {
            let path = rest.first().map(std::path::PathBuf::from).unwrap_or_else(config::config_path);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if rest.is_empty() && e.kind() == std::io::ErrorKind::NotFound => {
                    println!("{}: no config file, defaults apply", path.display());
                    return Ok(());
                }
                Err(e) => bail!("{}: {e}", path.display()),
            };
            let (_, warnings) = Config::parse(&text);
            for w in &warnings {
                println!("{}: {w}", path.display());
            }
            if warnings.is_empty() {
                println!("{}: ok", path.display());
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        _ => bail!("usage: opal config [--check [FILE]]"),
    }
}
