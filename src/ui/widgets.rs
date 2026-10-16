//! Drawing primitives shared by the panels: stacked bars, gauges and column charts.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::stack::{self, Cell, Segment};
use super::theme::Theme;
use crate::model::Slot;

pub const TAG_OTHER: usize = 8;
pub const TAG_HIDDEN: usize = 9;

/// Colour key used by the stack resolver for a workload's palette slot.
pub fn tag_for(slot: Slot) -> usize {
    match slot {
        Slot::Color(index) => usize::from(index).min(7),
        Slot::Other => TAG_OTHER,
    }
}

fn tag_color(theme: &Theme, tag: usize) -> Color {
    match tag {
        0..=7 => theme.slot(Slot::Color(tag as u8)),
        TAG_OTHER => theme.slot(Slot::Other),
        _ => theme.dim().fg.unwrap_or(Color::Gray),
    }
}

/// Fill characters for terminals without colour: eight slots, then "other", then "hidden".
fn mono_fills(theme: &Theme) -> Vec<char> {
    let mut fills: Vec<char> = theme.glyphs.fills.to_vec();
    fills.push('~');
    fills.push('.');
    fills
}

fn paint(buf: &mut Buffer, x: u16, y: u16, cell: Cell, theme: &Theme, fills: &[char]) {
    let Some(target) = buf.cell_mut((x, y)) else {
        return;
    };
    if theme.is_mono() {
        let glyph = cell.fg.map_or(' ', |tag| fills[tag % fills.len()]);
        target.set_char(glyph);
        target.set_style(Style::default());
        return;
    }
    if theme.ascii {
        paint_ascii_colour(target, cell, theme);
        return;
    }
    let fg = cell.fg.map(|tag| tag_color(theme, tag));
    let bg = cell
        .bg
        .map_or(theme.empty_cell(), |tag| tag_color(theme, tag));
    target.set_char(cell.glyph);
    target.set_style(Style::default().fg(fg.unwrap_or(Color::Reset)).bg(bg));
}

/// Without Unicode blocks, a coloured bar is drawn with background colour on plain spaces; a partial cell goes to whichever side covers more of it.
fn paint_ascii_colour(target: &mut ratatui::buffer::Cell, cell: Cell, theme: &Theme) {
    let covered_by_fg = cell.glyph == '█'
        || stack::LEFT_EIGHTHS
            .iter()
            .position(|g| *g == cell.glyph)
            .is_some_and(|n| n >= 4)
        || stack::LOWER_EIGHTHS
            .iter()
            .position(|g| *g == cell.glyph)
            .is_some_and(|n| n >= 4);
    let tag = if covered_by_fg { cell.fg } else { cell.bg };
    let background = tag.map_or(theme.empty_cell(), |t| tag_color(theme, t));
    target.set_char(' ');
    target.set_style(Style::default().bg(background));
}

/// Replaces every non-ASCII character on screen with a plain equivalent, for terminals that cannot draw Unicode.
pub fn asciify(buf: &mut Buffer) {
    for cell in buf.content.iter_mut() {
        let symbol = cell.symbol();
        if symbol.is_ascii() {
            continue;
        }
        let replaced: String = symbol.chars().map(ascii_for).collect();
        cell.set_symbol(&replaced);
    }
}

fn ascii_for(ch: char) -> char {
    match ch {
        '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' => '+',
        '─' => '-',
        '│' => '|',
        '·' | '—' | '•' => '-',
        '…' => '~',
        '°' => 'o',
        '↑' | '▲' => '^',
        '↓' | '▼' => 'v',
        '▶' => '>',
        '×' => 'x',
        '█' | '▉' | '▊' | '▋' | '▌' | '▆' | '▇' => '#',
        '▅' | '▄' => '=',
        '▃' => '-',
        '▁' | '▂' => '_',
        '▏' | '▎' | '▍' => '|',
        '●' | '◐' | '◆' => '*',
        '○' => '.',
        other if other.is_ascii() => other,
        _ => '?',
    }
}

/// A horizontal stacked bar: segments left to right, remainder drawn as an empty track.
pub fn stacked_bar(buf: &mut Buffer, area: Rect, segments: &[Segment], total: f64, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let cells = stack::resolve_horizontal(segments, total, usize::from(area.width));
    let fills = mono_fills(theme);
    for (offset, cell) in cells.into_iter().enumerate() {
        paint(buf, area.x + offset as u16, area.y, cell, theme, &fills);
    }
}

/// A stacked bar with a caption written inside every segment that is wide enough to hold it.
pub fn labeled_bar(
    buf: &mut Buffer,
    area: Rect,
    items: &[(Segment, String)],
    total: f64,
    theme: &Theme,
) {
    let segments: Vec<Segment> = items.iter().map(|(segment, _)| *segment).collect();
    stacked_bar(buf, area, &segments, total, theme);
    if theme.is_mono() || area.width == 0 {
        return;
    }
    let spans = whole_cell_spans(&segments, total, usize::from(area.width));
    for ((segment, caption), span) in items.iter().zip(spans) {
        let Some((start, len)) = span else { continue };
        let wanted = unicode_width::UnicodeWidthStr::width(caption.as_str());
        if wanted + 2 > len {
            continue;
        }
        let from = start + (len - wanted) / 2;
        let style = Style::default()
            .fg(Color::Black)
            .bg(tag_color(theme, segment.tag))
            .add_modifier(ratatui::style::Modifier::BOLD);
        text(
            buf,
            area.x + from as u16,
            area.y,
            caption,
            style,
            wanted as u16,
        );
    }
}

/// For each segment, the stretch of whole cells it fills, as (first cell, cell count), using the resolver's own scaling and rounding. Segments that draw nothing get `None`.
fn whole_cell_spans(segments: &[Segment], total: f64, width: usize) -> Vec<Option<(usize, usize)>> {
    let valid = |s: &Segment| s.value.is_finite() && s.value > 0.0;
    let sum: f64 = segments.iter().filter(|s| valid(s)).map(|s| s.value).sum();
    if !(total.is_finite() && total > 0.0) || sum <= 0.0 {
        return vec![None; segments.len()];
    }
    let scale = if sum > total { total / sum } else { 1.0 };
    let sub_steps = width * 8;
    let mut cumulative = 0.0;
    let mut previous = 0usize;
    segments
        .iter()
        .map(|segment| {
            if !valid(segment) {
                return None;
            }
            cumulative += segment.value * scale;
            let end = ((cumulative / total) * sub_steps as f64).round() as usize;
            let (start, stop) = (previous, end.min(sub_steps));
            previous = stop;
            let first = start.div_ceil(8);
            let last = stop / 8;
            (last > first).then(|| (first, last - first))
        })
        .collect()
}

/// One column of a stacked time series, drawn bottom-up inside `area` (a single column wide).
pub fn stacked_column(
    buf: &mut Buffer,
    x: u16,
    area: Rect,
    segments: &[Segment],
    total: f64,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    let cells = stack::resolve_vertical(segments, total, usize::from(area.height));
    let fills = mono_fills(theme);
    let bottom = area.y + area.height - 1;
    for (offset, cell) in cells.into_iter().enumerate() {
        paint(buf, x, bottom - offset as u16, cell, theme, &fills);
    }
}

/// A single-colour gauge with a styled fill, used for power, temperature and per-app load.
pub fn gauge(buf: &mut Buffer, area: Rect, fraction: f64, style: Style, theme: &Theme) {
    if area.width == 0 {
        return;
    }
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled_eighths = (fraction * f64::from(area.width) * 8.0).round() as usize;
    for column in 0..usize::from(area.width) {
        let eighths = filled_eighths.saturating_sub(column * 8).min(8);
        let glyph = match eighths {
            0 => ' ',
            8 => theme.glyphs.full,
            n => theme.glyphs.h_eighths[n],
        };
        let Some(cell) = buf.cell_mut((area.x + column as u16, area.y)) else {
            continue;
        };
        cell.set_char(glyph);
        if theme.is_mono() {
            cell.set_style(Style::default());
        } else {
            cell.set_style(style.bg(theme.empty_cell()));
        }
    }
}

/// Writes text and returns the number of columns it used.
pub fn text(buf: &mut Buffer, x: u16, y: u16, content: &str, style: Style, max_width: u16) -> u16 {
    if max_width == 0 {
        return 0;
    }
    let (end_x, _) = buf.set_stringn(x, y, content, usize::from(max_width), style);
    end_x.saturating_sub(x)
}

/// Writes text so that it ends at `right_edge` (exclusive).
pub fn text_right(
    buf: &mut Buffer,
    right_edge: u16,
    y: u16,
    content: &str,
    style: Style,
    max_width: u16,
) {
    let width = unicode_width::UnicodeWidthStr::width(content).min(usize::from(max_width)) as u16;
    text(
        buf,
        right_edge.saturating_sub(width),
        y,
        content,
        style,
        max_width,
    );
}

/// The colour swatch drawn before an app's name: a solid block, or its fill character without colour.
pub fn swatch(theme: &Theme, slot: Slot) -> (String, Style) {
    if theme.is_mono() {
        let index = usize::from(match slot {
            Slot::Color(i) => i,
            Slot::Other => 5,
        }) % theme.glyphs.fills.len();
        (theme.glyphs.fills[index].to_string(), Style::default())
    } else {
        (
            theme.glyphs.full.to_string(),
            Style::default().fg(theme.slot(slot)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ColorMode;

    fn row(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn mono_bars_use_fill_characters() {
        let theme = Theme::new(ColorMode::Mono, true, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        let segments = [
            Segment {
                value: 50.0,
                tag: 0,
            },
            Segment {
                value: 30.0,
                tag: 1,
            },
        ];
        stacked_bar(&mut buf, Rect::new(0, 0, 10, 1), &segments, 100.0, &theme);
        assert_eq!(row(&buf, 0, 10), "#####===  ");
    }

    #[test]
    fn colour_bars_paint_full_blocks_with_slot_colours() {
        let theme = Theme::new(ColorMode::TrueColor, false, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        stacked_bar(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            &[Segment {
                value: 100.0,
                tag: 2,
            }],
            100.0,
            &theme,
        );
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(3, 0)].fg, theme.slot(Slot::Color(2)));
    }

    #[test]
    fn column_fills_from_the_bottom() {
        let theme = Theme::new(ColorMode::Mono, true, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 4));
        stacked_column(
            &mut buf,
            0,
            Rect::new(0, 0, 1, 4),
            &[Segment {
                value: 50.0,
                tag: 0,
            }],
            100.0,
            &theme,
        );
        let column: String = (0..4).map(|y| buf[(0, y)].symbol().to_string()).collect();
        assert_eq!(column, "  ##");
    }

    #[test]
    fn gauge_handles_nan_and_overflow() {
        let theme = Theme::new(ColorMode::Mono, true, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        gauge(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            f64::NAN,
            Style::default(),
            &theme,
        );
        assert_eq!(row(&buf, 0, 4), "    ");
        gauge(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            9.0,
            Style::default(),
            &theme,
        );
        assert_eq!(row(&buf, 0, 4), "####");
    }

    #[test]
    fn text_right_aligns_to_the_edge() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        text_right(&mut buf, 8, 0, "ab", Style::default(), 8);
        assert_eq!(row(&buf, 0, 8), "      ab");
    }

    #[test]
    fn segments_sharing_a_colour_each_keep_their_own_caption() {
        let theme = Theme::new(ColorMode::TrueColor, false, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
        let items = vec![
            (
                Segment {
                    value: 40.0,
                    tag: 8,
                },
                "Alpha 40%".to_string(),
            ),
            (
                Segment {
                    value: 40.0,
                    tag: 8,
                },
                "Bravo 40%".to_string(),
            ),
        ];
        labeled_bar(&mut buf, Rect::new(0, 0, 40, 1), &items, 100.0, &theme);
        let line = row(&buf, 0, 40);
        assert!(
            line.contains("Alpha 40%") && line.contains("Bravo 40%"),
            "{line}"
        );
        assert!(
            line.find("Alpha").unwrap() < line.find("Bravo").unwrap(),
            "{line}"
        );
    }

    #[test]
    fn whole_cell_spans_follow_cumulative_rounding() {
        let segments = [
            Segment {
                value: 50.0,
                tag: 0,
            },
            Segment { value: 0.0, tag: 1 },
            Segment {
                value: 25.0,
                tag: 2,
            },
        ];
        let spans = whole_cell_spans(&segments, 100.0, 20);
        assert_eq!(spans, vec![Some((0, 10)), None, Some((10, 5))]);
        assert_eq!(whole_cell_spans(&segments, 0.0, 20), vec![None, None, None]);
    }

    #[test]
    fn captions_only_appear_inside_wide_segments() {
        let theme = Theme::new(ColorMode::TrueColor, false, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        let items = vec![
            (
                Segment {
                    value: 60.0,
                    tag: 0,
                },
                "big 60%".to_string(),
            ),
            (
                Segment {
                    value: 10.0,
                    tag: 1,
                },
                "tiny 10%".to_string(),
            ),
        ];
        labeled_bar(&mut buf, Rect::new(0, 0, 20, 1), &items, 100.0, &theme);
        let line = row(&buf, 0, 20);
        assert!(line.contains("big 60%"), "{line}");
        assert!(!line.contains("tiny"), "{line}");
    }

    #[test]
    fn asciify_maps_boxes_and_blocks() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "┌─│…°█", Style::default());
        asciify(&mut buf);
        assert_eq!(row(&buf, 0, 6), "+-|~o#");
    }

    #[test]
    fn colour_ascii_bars_use_background_colour_on_spaces() {
        let theme = Theme::new(ColorMode::TrueColor, true, false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        stacked_bar(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            &[Segment {
                value: 50.0,
                tag: 1,
            }],
            100.0,
            &theme,
        );
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(0, 0)].bg, theme.slot(Slot::Color(1)));
        assert_eq!(buf[(3, 0)].bg, theme.empty_cell());
    }

    #[test]
    fn tags_stay_inside_the_palette() {
        assert_eq!(tag_for(Slot::Color(3)), 3);
        assert_eq!(tag_for(Slot::Color(200)), 7);
        assert_eq!(tag_for(Slot::Other), TAG_OTHER);
    }
}
