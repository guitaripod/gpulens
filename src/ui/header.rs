//! The top of the screen: title bar, the plain-language story, and the two stacked bars.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use super::layout::StoryNeeds;
use super::stack::Segment;
use super::widgets::{self, TAG_HIDDEN, tag_for};
use crate::app::{AiStatus, App};
use crate::format;
use crate::model::{DeviceView, Slot, Workload};
use crate::narrate::{Level, Story};

const LABEL_WIDTH: u16 = 7;
const VALUE_WIDTH: u16 = 17;
const NOMINAL_HEADLINE_WIDTH: usize = 240;

/// What the story block will show, decided once per frame so layout and drawing agree.
pub struct StoryView {
    pub headline: String,
    pub ai_written: bool,
    pub notes: Vec<(Level, String)>,
    pub is_error: bool,
}

pub fn story_view(app: &App, story: &Story) -> StoryView {
    if let Some(error) = &app.backend_error {
        return StoryView {
            headline: format!("Cannot read the GPU: {error}. Retrying…"),
            ai_written: false,
            notes: Vec::new(),
            is_error: true,
        };
    }
    let mut notes: Vec<(Level, String)> = story
        .notes
        .iter()
        .map(|n| (n.level, n.text.clone()))
        .collect();
    match app.fresh_narration() {
        Some(narration) => {
            if let Some(tip) = &narration.tip {
                notes.insert(0, (Level::Info, format!("Tip: {tip}")));
            }
            StoryView {
                headline: narration.headline.clone(),
                ai_written: true,
                notes,
                is_error: false,
            }
        }
        None => StoryView {
            headline: story.headline.clone(),
            ai_written: false,
            notes,
            is_error: false,
        },
    }
}

/// The most room the story block may ever take. It depends only on the window size.
pub fn needs(width: u16, height: u16) -> StoryNeeds {
    let usable = usize::from(width.saturating_sub(4)).max(1);
    let lines = NOMINAL_HEADLINE_WIDTH.div_ceil(usable).clamp(1, 2) as u16;
    let note_lines = match height {
        0..=25 => 0,
        26..=35 => 1,
        36..=47 => 2,
        _ => 3,
    };
    StoryNeeds {
        headline_lines: lines,
        note_lines,
    }
}

/// How many lines the current wording would like.
pub fn wanted(view: &StoryView, width: u16) -> StoryNeeds {
    let usable = usize::from(width.saturating_sub(4)).max(1);
    let lines = (view.headline.width() + 2).div_ceil(usable).clamp(1, 2) as u16;
    StoryNeeds {
        headline_lines: lines,
        note_lines: view.notes.len().min(usize::from(u16::MAX)) as u16,
    }
}

pub fn render_title(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let device = &app.model.devices[app.device];
    let chip = ai_chip(app);
    let chip_width = UnicodeWidthStr::width(chip.0.as_str()) as u16;
    let right = area.x + area.width;
    let left_limit = right.saturating_sub(chip_width + 1);
    let mut x = area.x;
    x += widgets::text(
        buf,
        x,
        area.y,
        " gpulens ",
        theme.title(),
        left_limit.saturating_sub(x),
    );
    if app.model.devices.len() > 1 {
        x += widgets::text(
            buf,
            x,
            area.y,
            " ",
            theme.text(),
            left_limit.saturating_sub(x),
        );
        for (index, other) in app.model.devices.iter().enumerate() {
            let util = other.calm_util();
            let label = format!(" {} {} {util}% ", index + 1, short_name(&other.info.name));
            let style = if index == app.device {
                theme.selected()
            } else {
                theme.dim()
            };
            x += widgets::text(buf, x, area.y, &label, style, left_limit.saturating_sub(x));
        }
    } else {
        let room = left_limit.saturating_sub(x + 1);
        let with_driver = format!(
            "{}  {}  driver {}",
            device.info.name, theme.glyphs.sep, device.info.driver_version
        );
        let name = if UnicodeWidthStr::width(with_driver.as_str()) <= usize::from(room) {
            with_driver
        } else {
            device.info.name.clone()
        };
        widgets::text(
            buf,
            x + 1,
            area.y,
            &format::truncate(&name, usize::from(room)),
            theme.text(),
            room,
        );
    }
    widgets::text_right(buf, right, area.y, &chip.0, chip.1, chip_width);
}

fn short_name(name: &str) -> String {
    let trimmed = name
        .trim_start_matches("NVIDIA ")
        .trim_start_matches("GeForce ");
    format::truncate(trimmed, 22)
}

fn ai_chip(app: &App) -> (String, ratatui::style::Style) {
    let theme = &app.theme;
    let mark = theme.glyphs.ai_mark;
    match &app.ai.status {
        AiStatus::Ready { model } if app.ai.last_problem.is_some() => (
            format!("{mark} {} (not answering) ", format::truncate(model, 28)),
            theme.warn(),
        ),
        AiStatus::Ready { model } => (
            format!("{mark} {} ", format::truncate(model, 32)),
            theme.ai(),
        ),
        AiStatus::Connecting => (format!("{mark} connecting… "), theme.dim()),
        AiStatus::Unavailable(reason) => (
            format!("{mark} AI helper: {} ", format::truncate(reason, 44)),
            theme.dim(),
        ),
        AiStatus::Off => (format!("{mark} AI helper off "), theme.faint()),
    }
}

pub fn render_story(buf: &mut Buffer, area: Rect, app: &App, view: &StoryView, needs: StoryNeeds) {
    let theme = &app.theme;
    if area.height == 0 {
        return;
    }
    let headline_style = if view.is_error {
        theme.bad()
    } else {
        theme.text()
    };
    let mut spans = Vec::new();
    if view.ai_written {
        spans.push(Span::styled(
            format!("{} ", theme.glyphs.ai_mark),
            theme.ai(),
        ));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(view.headline.clone(), headline_style));
    let headline_height = needs.headline_lines.clamp(1, area.height);
    let headline_area = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        headline_height,
    );
    Paragraph::new(Line::from(spans))
        .wrap(Wrap { trim: true })
        .render(headline_area, buf);
    for (index, (level, note)) in view.notes.iter().enumerate() {
        let y = area.y + headline_height + index as u16;
        if y >= area.y + area.height {
            break;
        }
        let (marker, style) = match level {
            Level::Warn => ("!", theme.warn()),
            Level::Info => (theme.glyphs.sep, theme.dim()),
        };
        let line_width = area.width.saturating_sub(5);
        widgets::text(buf, area.x + 2, y, marker, style, 1);
        widgets::text(
            buf,
            area.x + 4,
            y,
            &format::truncate(note, usize::from(line_width)),
            style,
            line_width,
        );
    }
}

pub fn render_bars(buf: &mut Buffer, area: Rect, app: &App) {
    if area.height < 2 {
        return;
    }
    let device = &app.model.devices[app.device];
    let Some(sample) = device.sample.as_ref() else {
        return;
    };
    let theme = &app.theme;
    let now = app.now;
    let bar_width = area.width.saturating_sub(LABEL_WIDTH + VALUE_WIDTH + 1);
    let bar_x = area.x + LABEL_WIDTH;
    let busy_now = device.shown_util(now);
    let used_now = device.shown_mem_used(now);

    widgets::text(buf, area.x + 1, area.y, "GPU", theme.dim(), LABEL_WIDTH - 1);
    let compute = compute_items(device, now);
    widgets::labeled_bar(
        buf,
        Rect::new(bar_x, area.y, bar_width, 1),
        &compute,
        100.0,
        theme,
    );
    widgets::text(
        buf,
        bar_x + bar_width + 1,
        area.y,
        &format!("{:.0}% busy", busy_now),
        theme.heat(busy_now / 100.0),
        VALUE_WIDTH,
    );

    widgets::text(
        buf,
        area.x + 1,
        area.y + 1,
        "VRAM",
        theme.dim(),
        LABEL_WIDTH - 1,
    );
    let memory = memory_items(device, now);
    widgets::labeled_bar(
        buf,
        Rect::new(bar_x, area.y + 1, bar_width, 1),
        &memory,
        sample.mem_total as f64,
        theme,
    );
    widgets::text(
        buf,
        bar_x + bar_width + 1,
        area.y + 1,
        &format::bytes_of(used_now, sample.mem_total),
        theme.heat(used_now as f64 / sample.mem_total.max(1) as f64),
        VALUE_WIDTH,
    );
}

/// Apps in colour order, the same left to right whatever they are doing, so the bars hold still while their segments grow and shrink.
fn by_colour(device: &DeviceView) -> Vec<&Workload> {
    let rank = |slot: Slot| match slot {
        Slot::Color(index) => index,
        Slot::Other => u8::MAX,
    };
    let mut apps: Vec<&Workload> = device.workloads.iter().collect();
    apps.sort_by_key(|w| (rank(w.slot), w.id));
    apps
}

/// Compute segments scaled so the filled part always equals the card's overall busy figure.
fn compute_items(device: &DeviceView, now: Instant) -> Vec<(Segment, String)> {
    let claimed: Vec<(&Workload, f64)> = by_colour(device)
        .into_iter()
        .map(|w| (w, w.shown_sm(now)))
        .filter(|(_, sm)| *sm >= 0.05)
        .collect();
    let sum: f64 = claimed.iter().map(|(_, sm)| sm).sum();
    let busy = device.shown_util(now);
    let scale = if sum > busy && sum > 0.0 {
        busy / sum
    } else {
        1.0
    };
    let mut items: Vec<(Segment, String)> = claimed
        .iter()
        .map(|(w, sm)| {
            (
                Segment {
                    value: sm * scale,
                    tag: tag_for(w.slot),
                },
                format!("{} {:.0}%", w.display_name, sm),
            )
        })
        .collect();
    let rest = busy - sum * scale;
    if rest >= 0.5 {
        items.push((
            Segment {
                value: rest,
                tag: TAG_HIDDEN,
            },
            format!("unclaimed {}%", rest.round()),
        ));
    }
    items
}

fn memory_items(device: &DeviceView, now: Instant) -> Vec<(Segment, String)> {
    let holders: Vec<(&Workload, u64)> = by_colour(device)
        .into_iter()
        .map(|w| (w, w.shown_mem(now)))
        .filter(|(_, mem)| *mem > 0)
        .collect();
    let accounted: u64 = holders.iter().map(|(_, mem)| mem).sum();
    let mut items: Vec<(Segment, String)> = holders
        .iter()
        .map(|(w, mem)| {
            (
                Segment {
                    value: *mem as f64,
                    tag: tag_for(w.slot),
                },
                format!("{} {}", w.display_name, format::bytes(*mem)),
            )
        })
        .collect();
    let rest = device.shown_mem_used(now).saturating_sub(accounted);
    if rest > 0 {
        items.push((
            Segment {
                value: rest as f64,
                tag: TAG_HIDDEN,
            },
            format!("reserved {}", format::bytes(rest)),
        ));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::*;
    use crate::types::Category;

    #[test]
    fn compute_segments_scale_down_when_processes_over_report() {
        let mut m = model();
        feed(
            &mut m,
            50,
            2 * GIB,
            vec![
                observed(1, identity("a", "A", Category::Ai), GIB, 40),
                observed(2, identity("b", "B", Category::Game), GIB, 40),
            ],
        );
        let items = compute_items(&m.devices[0], Instant::now());
        let total: f64 = items.iter().map(|(s, _)| s.value).sum();
        assert!((total - 50.0).abs() < 1e-9);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn unclaimed_compute_gets_its_own_segment() {
        let mut m = model();
        feed(
            &mut m,
            70,
            2 * GIB,
            vec![observed(1, identity("a", "A", Category::Ai), GIB, 50)],
        );
        let items = compute_items(&m.devices[0], Instant::now());
        assert_eq!(items.last().unwrap().0.tag, TAG_HIDDEN);
        assert_eq!(items.last().unwrap().1, "unclaimed 20%");
    }

    #[test]
    fn memory_segments_are_largest_first_with_reserved_remainder() {
        let mut m = model();
        feed(
            &mut m,
            10,
            12 * GIB,
            vec![
                observed(1, identity("a", "Small", Category::Ai), GIB, 5),
                observed(2, identity("b", "Big", Category::Game), 9 * GIB, 5),
            ],
        );
        let items = memory_items(&m.devices[0], Instant::now());
        let names: Vec<_> = items.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["Big 9.0 GB", "Small 1.0 GB", "reserved 2.0 GB"]);
    }

    #[test]
    fn wanted_room_follows_the_wording() {
        let view = |headline: &str, notes: usize| StoryView {
            headline: headline.to_string(),
            ai_written: false,
            notes: vec![(Level::Info, "n".to_string()); notes],
            is_error: false,
        };
        assert_eq!(wanted(&view("short", 2), 100).headline_lines, 1);
        assert_eq!(wanted(&view(&"x".repeat(150), 0), 100).headline_lines, 2);
        assert_eq!(wanted(&view("short", 3), 100).note_lines, 3);
    }

    #[test]
    fn story_room_depends_on_the_window_only() {
        assert_eq!(needs(80, 24).headline_lines, 2);
        assert_eq!(needs(80, 24).note_lines, 0);
        assert_eq!(needs(150, 30).note_lines, 1);
        assert_eq!(needs(150, 40).note_lines, 2);
        assert_eq!(needs(300, 50).headline_lines, 1);
        assert_eq!(needs(150, 50).note_lines, 3);
    }

    #[test]
    fn bar_segments_keep_their_colour_order_whatever_the_load() {
        let mut m = model();
        let apps = |a: u8, b: u8| {
            vec![
                observed(1, identity("a", "A", Category::Ai), GIB, a),
                observed(2, identity("b", "B", Category::Game), GIB, b),
            ]
        };
        feed(&mut m, 80, 2 * GIB, apps(60, 20));
        let first: Vec<String> = compute_items(&m.devices[0], Instant::now())
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        feed(&mut m, 80, 2 * GIB, apps(20, 60));
        let second: Vec<String> = compute_items(&m.devices[0], Instant::now())
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        assert!(
            first[0].starts_with("A ") && second[0].starts_with("A "),
            "{first:?} {second:?}"
        );
    }
}
