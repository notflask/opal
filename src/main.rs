mod annot;
mod capture;
mod focus;
mod geom;
mod glass;
mod output;
mod overlay;
mod text;
mod theme;
mod toolbar;

use std::{path::PathBuf, thread};

use anyhow::{Result, bail};

use crate::{
    geom::Rect,
    overlay::{Options, Outcome},
};

const USAGE: &str = "opal — Wayland screenshot tool

USAGE: opal <region|output|all> [--no-edit]

  region     drag an area (may span monitors), draw, Enter to copy+save
  output     the focused monitor
  all        every monitor, stitched into one image
  --no-edit  skip the editor, copy+save immediately";

fn main() -> Result<()> {
    let mut mode = None;
    let mut no_edit = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--no-edit" => no_edit = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "region" | "output" | "all" => mode = Some(a),
            _ => bail!("unknown argument {a:?}\n\n{USAGE}"),
        }
    }
    let Some(mode) = mode else { bail!("{USAGE}") };

    let t0 = std::time::Instant::now();
    let trace = |what: &str| {
        if std::env::var_os("OPAL_TRACE").is_some() {
            eprintln!("[{:>5} ms] {what}", t0.elapsed().as_millis());
        }
    };
    // Font discovery is slow; do it while the screen is being captured.
    let fonts = thread::spawn(move || {
        let f = text::Fonts::new();
        if std::env::var_os("OPAL_TRACE").is_some() {
            eprintln!("[{:>5} ms] fonts ready", t0.elapsed().as_millis());
        }
        f
    });
    let focused = focus::focused_output();

    let shots = capture::capture_all()?;
    trace("captured");
    let scale = output::scale(&shots);

    // Which part of the screen the result starts from.
    let preset = match mode.as_str() {
        "all" => Some(output::bounds(&shots)),
        "output" => {
            let shot = shots
                .iter()
                .find(|s| Some(&s.name) == focused.as_ref())
                .unwrap_or(&shots[0]);
            Some(Rect::new(shot.x as f32, shot.y as f32, shot.lw as f32, shot.lh as f32))
        }
        _ => None,
    };

    let (rect, items, shots, mut fonts) = if no_edit && preset.is_some() {
        (preset.unwrap(), Vec::new(), shots, None)
    } else {
        trace("starting overlay");
        let done = overlay::run(
            shots,
            theme::Theme::load(),
            fonts.join().expect("font thread"),
            Options {
                preset,
                finish_on_release: no_edit,
                hint_output: focused,
                canvas_scale: scale,
            },
        )?;
        trace("overlay closed");
        match done.outcome {
            Outcome::Cancel => return Ok(()),
            Outcome::Done { rect, items } => (rect, items, done.shots, Some(done.fonts)),
        }
    };

    let mut image = output::crop(&shots, rect)?;
    if let (false, Some(fonts)) = (items.is_empty(), fonts.as_mut()) {
        let s = output::scale(&shots);
        let ctx = annot::Ctx {
            tf: tiny_skia::Transform::from_row(s, 0.0, 0.0, s, -rect.x * s, -rect.y * s),
            k: s,
            shots: &shots,
        };
        annot::draw_all(&mut image.as_mut(), &ctx, &items, fonts);
    }
    let png = output::encode_png(&image)?;
    let dir = PathBuf::from(std::env::var("HOME")?).join("Screenshots");
    let path = output::save(&png, &dir, &output::timestamp())?;
    output::notify(&path);
    output::copy(&png)?;
    println!("{}", path.display());
    Ok(())
}
