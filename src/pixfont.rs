//! Built-in 8x16 pixel font (Terminus, SIL OFL 1.1, see assets/OFL.txt).

include!(concat!(env!("OUT_DIR"), "/pixel_font.rs"));

pub const GW: i32 = 8;
pub const GH: i32 = 16;

/// Bitmap rows of `c`, top to bottom, MSB is the leftmost pixel.
/// Unknown characters give `?`.
pub fn glyph(c: char) -> &'static [u8; 16] {
    let find = |c: char| {
        let code = u16::try_from(c as u32).ok()?;
        CODES.binary_search(&code).ok().map(|i| &BITS[i])
    };
    find(c).or_else(|| find('?')).unwrap_or(&BITS[0])
}

/// Width in logical pixels of `s`.
pub fn width(s: &str) -> i32 {
    s.chars().count() as i32 * GW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_covers_the_scripts_we_need() {
        assert!(CODES.windows(2).all(|w| w[0] < w[1]));
        for c in ['a', 'Z', '0', 'ä', 'Ж', 'ю', '×', '·', 'ї'] {
            assert_ne!(glyph(c), glyph('\u{1}'), "{c}");
            assert!(CODES.binary_search(&(c as u32 as u16)).is_ok(), "{c}");
        }
        // 'l' is not blank
        assert!(glyph('l').iter().any(|r| *r != 0));
        // space is
        assert!(glyph(' ').iter().all(|r| *r == 0));
    }
}
