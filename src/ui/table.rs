//! The main list: one row per application, with its colour, kind, state, GPU share and memory.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Widget};

use super::widgets;
use crate::app::App;
use crate::format;
use crate::model::{Activity, Workload};
use crate::types::IdentitySource;

const SWATCH_COLUMNS: u16 = 4;
const STATE_WIDTH: u16 = 12;
const GPU_WIDTH: u16 = 11;
const VRAM_WIDTH: u16 = 9;
const KIND_WIDTH: u16 = 9;
const SHOW_KIND_FROM: u16 = 80;
const IDLE_NOTE_AFTER_SECS: u64 = 10;

/// Where each visible row ended up, for mouse hit-testing, and how far the list has scrolled.
pub struct TableOutcome {
    pub rows: Vec<(Rect, String)>,
    pub scroll: usize,
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App) -> TableOutcome {
    let theme = &app.theme;
    let rows = app.visible();
    let block = Block::bordered()
        .border_style(theme.border_focus())
        .title(Line::styled(" Who is using the GPU ", theme.title()))
        .title_top(Line::styled(subtitle(app, rows.len()), theme.dim()).right_aligned());
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 2 || inner.width < 20 {
        return TableOutcome {
            rows: Vec::new(),
            scroll: 0,
        };
    }
    let columns = Columns::for_width(inner.width);
    render_header(buf, inner, &columns, app);
    let list = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    if rows.is_empty() {
        render_empty(buf, list, app);
        return TableOutcome {
            rows: Vec::new(),
            scroll: 0,
        };
    }
    let row_height: u16 = if app.show_descriptions && list.height >= 4 {
        2
    } else {
        1
    };
    let capacity = usize::from(list.height / row_height).max(1);
    let scroll = scroll_for(app.selected_row, app.scroll, capacity, rows.len());
    let mut hit_rows = Vec::new();
    for (offset, workload) in rows.iter().skip(scroll).take(capacity).enumerate() {
        let rect = Rect::new(
            list.x,
            list.y + offset as u16 * row_height,
            list.width,
            row_height,
        );
        let selected = app.selection.as_deref() == Some(workload.key.as_str());
        render_row(buf, rect, workload, &columns, app, selected);
        hit_rows.push((rect, workload.key.clone()));
    }
    TableOutcome {
        rows: hit_rows,
        scroll,
    }
}

fn subtitle(app: &App, total: usize) -> String {
    let filter = if app.filter.is_empty() {
        String::new()
    } else {
        format!(" · filter: {}", app.filter)
    };
    format!(" sort: {}{filter} · {total} shown ", app.sort.label())
}

/// Keeps the selected row on screen with the least movement.
pub fn scroll_for(selected: usize, current: usize, capacity: usize, total: usize) -> usize {
    let max_scroll = total.saturating_sub(capacity);
    let mut scroll = current.min(max_scroll);
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + capacity {
        scroll = selected + 1 - capacity;
    }
    scroll.min(max_scroll)
}

struct Columns {
    name_x: u16,
    name_width: u16,
    kind_x: Option<u16>,
    state_x: u16,
    gpu_x: u16,
    vram_right: u16,
}

impl Columns {
    fn for_width(width: u16) -> Self {
        let show_kind = width >= SHOW_KIND_FROM;
        let kind = if show_kind { KIND_WIDTH } else { 0 };
        let fixed = SWATCH_COLUMNS + kind + STATE_WIDTH + GPU_WIDTH + VRAM_WIDTH + 1;
        let name_width = width.saturating_sub(fixed).max(10);
        let name_x = SWATCH_COLUMNS;
        let kind_x = show_kind.then_some(name_x + name_width + 1);
        let state_x = name_x + name_width + 1 + kind;
        let gpu_x = state_x + STATE_WIDTH;
        Self {
            name_x,
            name_width,
            kind_x,
            state_x,
            gpu_x,
            vram_right: width,
        }
    }
}

fn render_header(buf: &mut Buffer, inner: Rect, columns: &Columns, app: &App) {
    let style = app.theme.dim().add_modifier(Modifier::BOLD);
    let y = inner.y;
    widgets::text(
        buf,
        inner.x + columns.name_x,
        y,
        "APP",
        style,
        columns.name_width,
    );
    if let Some(x) = columns.kind_x {
        widgets::text(buf, inner.x + x, y, "KIND", style, KIND_WIDTH);
    }
    widgets::text(
        buf,
        inner.x + columns.state_x,
        y,
        "STATE",
        style,
        STATE_WIDTH,
    );
    widgets::text(
        buf,
        inner.x + columns.gpu_x,
        y,
        "GPU TIME",
        style,
        GPU_WIDTH,
    );
    widgets::text_right(
        buf,
        inner.x + columns.vram_right,
        y,
        "VRAM",
        style,
        VRAM_WIDTH,
    );
}

fn render_empty(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let message = if app.filter.is_empty() {
        "Nothing is using the GPU right now."
    } else {
        "No app matches the filter. Esc clears it."
    };
    widgets::text(
        buf,
        area.x + 2,
        area.y + 1,
        message,
        theme.dim(),
        area.width.saturating_sub(4),
    );
    if app.filter.is_empty() && area.height > 3 {
        let hint = "Running in a container? The driver only reports processes visible to the host (try --pid=host).";
        widgets::text(
            buf,
            area.x + 2,
            area.y + 3,
            &format::truncate(hint, usize::from(area.width.saturating_sub(4))),
            theme.faint(),
            area.width.saturating_sub(4),
        );
    }
}

fn render_row(
    buf: &mut Buffer,
    rect: Rect,
    workload: &Workload,
    columns: &Columns,
    app: &App,
    selected: bool,
) {
    let theme = &app.theme;
    if selected {
        buf.set_style(rect, theme.selected());
    }
    let y = rect.y;
    if selected {
        widgets::text(buf, rect.x, y, theme.glyphs.selected, theme.accent(), 1);
    }
    let (swatch, swatch_style) = widgets::swatch(theme, workload.slot);
    widgets::text(buf, rect.x + 1, y, &swatch, swatch_style, 1);

    let name_style = if workload.exited {
        theme.faint()
    } else if selected {
        theme.text().add_modifier(Modifier::BOLD)
    } else {
        theme.text()
    };
    let count = if workload.procs.len() > 1 {
        format!(" ×{}", workload.procs.len())
    } else {
        String::new()
    };
    let name_room = usize::from(columns.name_width).saturating_sub(count.chars().count());
    let name = format::truncate(&workload.display_name, name_room);
    let used = widgets::text(
        buf,
        rect.x + columns.name_x,
        y,
        &name,
        name_style,
        columns.name_width,
    );
    widgets::text(
        buf,
        rect.x + columns.name_x + used,
        y,
        &count,
        theme.dim(),
        columns.name_width.saturating_sub(used),
    );

    if let Some(x) = columns.kind_x {
        let category = workload.category();
        let style = Style::default().fg(theme.category(category));
        widgets::text(
            buf,
            rect.x + x,
            y,
            category.label(),
            if theme.is_mono() { theme.dim() } else { style },
            KIND_WIDTH,
        );
    }
    render_state(buf, rect.x + columns.state_x, y, workload, app);
    render_gpu(buf, rect.x + columns.gpu_x, y, workload, app);
    let (memory, style) = if workload.has_memory_figure() {
        let style = if workload.exited {
            theme.faint()
        } else {
            theme.text()
        };
        (format::bytes(workload.shown_mem(app.now)), style)
    } else {
        ("n/a".to_string(), theme.faint())
    };
    widgets::text_right(
        buf,
        rect.x + columns.vram_right,
        y,
        &memory,
        style,
        VRAM_WIDTH,
    );

    if rect.height > 1 {
        render_description(buf, rect, workload, app);
    }
}

fn render_state(buf: &mut Buffer, x: u16, y: u16, workload: &Workload, app: &App) {
    let theme = &app.theme;
    let glyphs = &theme.glyphs;
    let marker = match workload.activity {
        Activity::Busy => glyphs.busy,
        Activity::Working => glyphs.working,
        Activity::Light => glyphs.light,
        Activity::Idle => glyphs.idle,
    };
    if workload.exited {
        widgets::text(
            buf,
            x,
            y,
            &format!("{} exited", glyphs.idle),
            theme.faint(),
            STATE_WIDTH - 1,
        );
        return;
    }
    let idle_secs = (f64::from(workload.idle_ticks) * app.model.seconds_per_frame()) as u64;
    let mut label = workload.activity.label().to_string();
    if workload.activity == Activity::Idle && idle_secs >= IDLE_NOTE_AFTER_SECS {
        label = format!(
            "idle {}",
            format::duration_coarse(std::time::Duration::from_secs(idle_secs))
        );
    }
    let style = theme.activity_style(workload.activity.label());
    widgets::text(
        buf,
        x,
        y,
        &format!("{marker} {label}"),
        style,
        STATE_WIDTH - 1,
    );
}

fn render_gpu(buf: &mut Buffer, x: u16, y: u16, workload: &Workload, app: &App) {
    let theme = &app.theme;
    let sm = workload.shown_sm(app.now);
    if sm >= 0.5 {
        let bar = Rect::new(x, y, 5, 1);
        let style = if workload.exited {
            theme.faint()
        } else {
            theme.heat(sm / 100.0)
        };
        widgets::gauge(buf, bar, sm / 100.0, style, theme);
        widgets::text(buf, x + 6, y, &format!("{sm:>3.0}%"), theme.text(), 4);
    } else if workload.calm_enc() > 0 || workload.calm_dec() > 0 {
        let (label, value) = if workload.calm_enc() >= workload.calm_dec() {
            ("enc", workload.calm_enc())
        } else {
            ("dec", workload.calm_dec())
        };
        widgets::text(
            buf,
            x,
            y,
            &format!("{label} {value}%"),
            theme.text(),
            GPU_WIDTH - 1,
        );
    } else {
        let unsupported = app.model.devices[app.device]
            .sample
            .as_ref()
            .is_some_and(|s| s.per_process_unsupported);
        let placeholder = if unsupported { "n/a" } else { "—" };
        widgets::text(buf, x, y, placeholder, theme.faint(), GPU_WIDTH - 1);
    }
}

fn render_description(buf: &mut Buffer, rect: Rect, workload: &Workload, app: &App) {
    let theme = &app.theme;
    let y = rect.y + 1;
    let x = rect.x + SWATCH_COLUMNS;
    let room = rect.width.saturating_sub(SWATCH_COLUMNS + 1);
    let (text, style) = describe(workload, app);
    let mut start = x;
    if workload.identity.source == IdentitySource::Learned {
        start += widgets::text(
            buf,
            start,
            y,
            &format!("{} ", theme.glyphs.ai_mark),
            theme.ai(),
            room,
        );
    }
    let remaining = room.saturating_sub(start - x);
    widgets::text(
        buf,
        start,
        y,
        &format::truncate(&text, usize::from(remaining)),
        style,
        remaining,
    );
}

/// The plain-language line under an app: what it is, or failing that the command that started it.
fn describe(workload: &Workload, app: &App) -> (String, Style) {
    let theme = &app.theme;
    if !workload.identity.description.is_empty() {
        return (workload.identity.description.clone(), theme.dim());
    }
    let command = workload
        .procs
        .first()
        .map(|p| p.meta.command_line())
        .unwrap_or_default();
    if command.is_empty() {
        ("Unknown program.".to_string(), theme.faint())
    } else {
        (command, theme.faint())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keeps_selection_visible() {
        assert_eq!(scroll_for(0, 0, 5, 20), 0);
        assert_eq!(scroll_for(7, 0, 5, 20), 3);
        assert_eq!(scroll_for(2, 3, 5, 20), 2);
        assert_eq!(scroll_for(6, 3, 5, 20), 3);
    }

    #[test]
    fn scroll_never_passes_the_end() {
        assert_eq!(scroll_for(19, 99, 5, 20), 15);
        assert_eq!(scroll_for(0, 5, 10, 4), 0);
    }

    #[test]
    fn columns_fit_inside_the_width() {
        for width in [58, 70, 79, 80, 100, 140] {
            let c = Columns::for_width(width);
            assert!(c.gpu_x + GPU_WIDTH <= c.vram_right + 2, "{width}");
            assert!(c.name_width >= 10);
            assert_eq!(c.kind_x.is_some(), width >= SHOW_KIND_FROM);
        }
    }
}
