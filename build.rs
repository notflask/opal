//! Turns `assets/ter-u16n.bdf` (Terminus 8x16, SIL OFL 1.1, see assets/OFL.txt)
//! into a sorted glyph table for the built-in pixel font.

use std::{env, fs, path::PathBuf};

/// Code point ranges worth embedding: Latin, Greek, Cyrillic, punctuation,
/// arrows and math. Everything else in the BDF is left out.
const RANGES: &[(u32, u32)] = &[
    (0x20, 0x24f),
    (0x370, 0x52f),
    (0x2010, 0x205f),
    (0x2190, 0x21ff),
    (0x2200, 0x22ff),
];

fn main() {
    println!("cargo:rerun-if-changed=assets/ter-u16n.bdf");
    println!("cargo:rerun-if-changed=build.rs");
    let bdf = fs::read_to_string("assets/ter-u16n.bdf").expect("assets/ter-u16n.bdf");

    let mut glyphs: Vec<(u32, [u8; 16])> = Vec::new();
    let mut code = None;
    let mut rows: Option<Vec<u8>> = None;
    for line in bdf.lines() {
        if let Some(n) = line.strip_prefix("ENCODING ") {
            code = n.trim().parse::<u32>().ok();
        } else if line == "BITMAP" {
            rows = Some(Vec::with_capacity(16));
        } else if line == "ENDCHAR" {
            if let (Some(c), Some(r)) = (code.take(), rows.take()) {
                if r.len() == 16 && RANGES.iter().any(|&(a, b)| (a..=b).contains(&c)) {
                    let mut g = [0u8; 16];
                    g.copy_from_slice(&r);
                    glyphs.push((c, g));
                }
            }
        } else if let Some(r) = rows.as_mut() {
            r.push(u8::from_str_radix(line.trim(), 16).expect("bitmap row"));
        }
    }
    glyphs.sort_by_key(|g| g.0);
    glyphs.dedup_by_key(|g| g.0);

    let mut out = String::new();
    out.push_str(&format!("pub static CODES: [u16; {}] = [", glyphs.len()));
    for (c, _) in &glyphs {
        out.push_str(&format!("{c},"));
    }
    out.push_str("];\n");
    out.push_str(&format!("pub static BITS: [[u8; 16]; {}] = [", glyphs.len()));
    for (_, g) in &glyphs {
        out.push('[');
        for b in g {
            out.push_str(&format!("{b},"));
        }
        out.push_str("],");
    }
    out.push_str("];\n");
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("pixel_font.rs");
    fs::write(path, out).unwrap();
}
