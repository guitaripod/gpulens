//! Human-readable numbers and width-aware text trimming.

use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const GIB: u64 = 1024 * 1024 * 1024;

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB_F: f64 = MIB * 1024.0;

/// Memory size in the units VRAM is sold in: `30.1 GB`, `380 MB`.
pub fn bytes(value: u64) -> String {
    let v = value as f64;
    if v >= GIB_F {
        if v >= GIB_F * 100.0 {
            format!("{:.0} GB", v / GIB_F)
        } else {
            format!("{:.1} GB", v / GIB_F)
        }
    } else if v >= MIB {
        format!("{:.0} MB", v / MIB)
    } else if v >= KIB {
        format!("{:.0} KB", v / KIB)
    } else {
        format!("{value} B")
    }
}

/// `used / total` sharing one unit when the total is in gigabytes: `78.4 / 95.6 GB`.
pub fn bytes_of(used: u64, total: u64) -> String {
    if total as f64 >= GIB_F {
        format!(
            "{:.1} / {:.1} GB",
            used as f64 / GIB_F,
            total as f64 / GIB_F
        )
    } else {
        format!("{} / {}", bytes(used), bytes(total))
    }
}

/// Throughput given in kilobytes per second, as NVML reports PCIe traffic.
pub fn rate_kbps(kbps: u32) -> String {
    let v = f64::from(kbps) * KIB;
    if v >= GIB_F {
        format!("{:.1} GB/s", v / GIB_F)
    } else if v >= MIB {
        format!("{:.0} MB/s", v / MIB)
    } else {
        format!("{kbps} KB/s")
    }
}

/// Compact elapsed time: `42s`, `3m05s`, `1h02m`.
pub fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Rough elapsed time for a narrow column: `42s`, `3m`, `2h`.
pub fn duration_coarse(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

/// Clock speed: `1.80 GHz` or `435 MHz`.
pub fn clock_mhz(mhz: u32) -> String {
    if mhz >= 1000 {
        format!("{:.2} GHz", f64::from(mhz) / 1000.0)
    } else {
        format!("{mhz} MHz")
    }
}

/// Share of `part` in `whole` as a whole percentage, 0 when `whole` is 0.
pub fn share_pct(part: u64, whole: u64) -> u32 {
    if whole == 0 {
        0
    } else {
        ((part as f64 / whole as f64) * 100.0).round() as u32
    }
}

/// Trims to a terminal column width, ending with `…` when text was cut.
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Pads with spaces on the right up to a terminal column width.
pub fn pad_right(text: &str, width: usize) -> String {
    let current = text.width();
    if current >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - current))
    }
}

/// Word-wraps to a column width, returning at most `max_lines` lines; the last is cut with `…` when text remains.
pub fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_width = if current.is_empty() {
            word.width()
        } else {
            current.width() + 1 + word.width()
        };
        if candidate_width > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(truncate(&format!("{last}…"), width));
    }
    lines
        .into_iter()
        .map(|line| truncate(&line, width))
        .collect()
}

/// Joins names as prose: `A`, `A and B`, `A, B and C`.
pub fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_pick_sensible_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(2048), "2 KB");
        assert_eq!(bytes(380 * 1024 * 1024), "380 MB");
        assert_eq!(bytes(8 * 1024 * 1024 * 1024 + 300 * 1024 * 1024), "8.3 GB");
        assert_eq!(bytes(120 * 1024 * 1024 * 1024), "120 GB");
    }

    #[test]
    fn bytes_of_shares_the_unit() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(bytes_of(78 * gib, 96 * gib), "78.0 / 96.0 GB");
        assert_eq!(bytes_of(10 * 1024, 100 * 1024), "10 KB / 100 KB");
    }

    #[test]
    fn rates_scale() {
        assert_eq!(rate_kbps(500), "500 KB/s");
        assert_eq!(rate_kbps(65_000), "63 MB/s");
        assert_eq!(rate_kbps(3_000_000), "2.9 GB/s");
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(duration(Duration::from_secs(7)), "7s");
        assert_eq!(duration(Duration::from_secs(185)), "3m05s");
        assert_eq!(duration(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn coarse_durations_stay_short() {
        assert_eq!(duration_coarse(Duration::from_secs(7)), "7s");
        assert_eq!(duration_coarse(Duration::from_secs(185)), "3m");
        assert_eq!(duration_coarse(Duration::from_secs(7300)), "2h");
    }

    #[test]
    fn clocks_switch_to_ghz() {
        assert_eq!(clock_mhz(435), "435 MHz");
        assert_eq!(clock_mhz(1800), "1.80 GHz");
    }

    #[test]
    fn share_handles_zero_whole() {
        assert_eq!(share_pct(5, 0), 0);
        assert_eq!(share_pct(1, 3), 33);
    }

    #[test]
    fn truncate_respects_width_and_wide_chars() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("日本語テキスト", 7), "日本語…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn pad_right_counts_columns() {
        assert_eq!(pad_right("ab", 5), "ab   ");
        assert_eq!(pad_right("日本", 5), "日本 ");
        assert_eq!(pad_right("toolong", 3), "toolong");
    }

    #[test]
    fn wrap_breaks_on_words_and_marks_cut_text() {
        assert_eq!(
            wrap("one two three four", 9, 5),
            vec!["one two", "three", "four"]
        );
        let cut = wrap("alpha beta gamma delta epsilon", 11, 2);
        assert_eq!(cut.len(), 2);
        assert!(cut[1].ends_with('…'), "{cut:?}");
        assert!(wrap("", 10, 2).is_empty());
        assert_eq!(wrap("supercalifragilistic", 8, 1), vec!["superca…"]);
    }

    #[test]
    fn names_read_as_prose() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_names(&names(&[])), "");
        assert_eq!(join_names(&names(&["A"])), "A");
        assert_eq!(join_names(&names(&["A", "B"])), "A and B");
        assert_eq!(join_names(&names(&["A", "B", "C"])), "A, B and C");
    }
}
