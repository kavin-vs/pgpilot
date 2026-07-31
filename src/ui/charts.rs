//! ASCII/Unicode block-ramp rendering: sparklines, area charts, bars, and the
//! small glyph set used throughout the dashboard (cursor markers, pause icon,
//! blocking-tree branches). Ported from the `Postgres Monitor TUI` design
//! mockup's `spark()`/`chart()`/`bar()`/`glyph()` helpers. Pure string
//! building, not `ratatui::widgets::Sparkline` — that widget has no ASCII
//! fallback and doesn't render inline next to a stat-card value.

const RAMP_UNICODE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const RAMP_ASCII: [char; 8] = ['.', ',', ':', ';', '=', '+', '*', '#'];

fn ramp(ascii: bool) -> &'static [char; 8] {
    if ascii { &RAMP_ASCII } else { &RAMP_UNICODE }
}

pub struct Glyphs {
    pub dot: char,
    pub pause: &'static str,
    pub cursor: char,
    pub root: char,
    pub branch: &'static str,
    pub full: char,
    pub empty: char,
    pub up: char,
    pub down: char,
}

pub fn glyphs(ascii: bool) -> Glyphs {
    if ascii {
        Glyphs {
            dot: '*',
            pause: "||",
            cursor: '>',
            root: '*',
            branch: " \\_",
            full: '#',
            empty: '.',
            up: '^',
            down: 'v',
        }
    } else {
        Glyphs {
            dot: '●',
            pause: "❚❚",
            cursor: '›',
            root: '●',
            branch: " └─",
            full: '█',
            empty: '·',
            up: '▲',
            down: '▼',
        }
    }
}

/// Renders up to the last `width` values as a single-row 8-level sparkline,
/// scaled to the slice's own min/max.
pub fn sparkline(values: &[f64], width: usize, ascii: bool) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let ramp = ramp(ascii);
    let start = values.len().saturating_sub(width);
    let slice = &values[start..];
    let hi = slice.iter().cloned().fold(f64::MIN, f64::max);
    let lo = slice.iter().cloned().fold(f64::MAX, f64::min);
    let span = if hi - lo == 0.0 { 1.0 } else { hi - lo };
    slice
        .iter()
        .map(|&v| {
            let idx = (((v - lo) / span * 7.99).floor() as usize).min(7);
            ramp[idx]
        })
        .collect()
}

/// Arrow + absolute percent change between `current` and the value `n_ago`
/// samples back.
pub fn delta_arrow(current: f64, n_ago: f64, ascii: bool) -> (char, f64) {
    let base = if n_ago == 0.0 { 1.0 } else { n_ago };
    let pct = (current - n_ago) / base * 100.0;
    let g = glyphs(ascii);
    let arrow = if pct >= 0.0 { g.up } else { g.down };
    (arrow, pct.abs())
}

/// Fixed-width horizontal bar: `pct` (0-100) filled cells, the rest empty.
pub fn bar(pct: f64, width: usize, ascii: bool) -> String {
    let g = glyphs(ascii);
    let n = ((pct / 100.0 * width as f64).round().max(0.0) as usize).min(width);
    let mut s = String::with_capacity(width);
    for _ in 0..n {
        s.push(g.full);
    }
    for _ in 0..(width - n) {
        s.push(g.empty);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_flat_input_uses_lowest_glyph() {
        let s = sparkline(&[5.0, 5.0, 5.0, 5.0], 4, false);
        assert_eq!(s, "▁▁▁▁");
    }

    #[test]
    fn sparkline_ramp_input_increases_monotonically() {
        let values: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let s = sparkline(&values, 8, false);
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars, RAMP_UNICODE.to_vec());
    }

    #[test]
    fn ascii_and_unicode_sparklines_diverge() {
        let values = [1.0, 2.0, 3.0, 4.0];
        let unicode = sparkline(&values, 4, false);
        let ascii = sparkline(&values, 4, true);
        assert_ne!(unicode, ascii);
        assert!(ascii.is_ascii());
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0.0, 10, true), "..........");
        assert_eq!(bar(100.0, 10, true), "##########");
        assert_eq!(bar(50.0, 10, true), "#####.....");
    }

    #[test]
    fn delta_arrow_signals_direction() {
        let (arrow, pct) = delta_arrow(120.0, 100.0, false);
        assert_eq!(arrow, '▲');
        assert!((pct - 20.0).abs() < 1e-9);

        let (arrow, _) = delta_arrow(80.0, 100.0, false);
        assert_eq!(arrow, '▼');
    }
}
