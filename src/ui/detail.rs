//! The pane under the table: everything about the selected app, in plain words, with its own history.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Sparkline, Widget};

use super::buckets;
use super::widgets;
use crate::app::App;
use crate::format;
use crate::model::Workload;

pub fn render(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(workload) = app.selected() else {
        let block = Block::bordered()
            .border_style(theme.border())
            .title(Line::styled(" Details ", theme.title()));
        let inner = block.inner(area);
        block.render(area, buf);
        widgets::text(
            buf,
            inner.x + 1,
            inner.y,
            "Select an app with ↑ ↓ to see what it is doing.",
            theme.faint(),
            inner.width.saturating_sub(2),
        );
        return;
    };
    let title = format!(
        " {} · {} ",
        workload.display_name,
        workload.category().label()
    );
    let block = Block::bordered()
        .border_style(theme.border())
        .title(Line::styled(title, theme.title()));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width < 40 || inner.height < 2 {
        return;
    }
    let right_width = (inner.width / 3).clamp(20, 36);
    let left = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width - right_width - 4,
        inner.height,
    );
    let right = Rect::new(
        inner.x + inner.width - right_width,
        inner.y,
        right_width - 1,
        inner.height,
    );
    render_text(buf, left, workload, app);
    render_history(buf, right, workload, app);
}

fn render_text(buf: &mut Buffer, area: Rect, workload: &Workload, app: &App) {
    let theme = &app.theme;
    let width = usize::from(area.width);
    let mut y = area.y;
    let bottom = area.y + area.height;
    let put = |buf: &mut Buffer, line: &str, style: Style, y: &mut u16| {
        if *y < bottom {
            widgets::text(
                buf,
                area.x,
                *y,
                &format::truncate(line, width),
                style,
                area.width,
            );
            *y += 1;
        }
    };

    let description = if workload.identity.description.is_empty() {
        workload
            .procs
            .first()
            .map(|p| p.meta.command_line())
            .unwrap_or_default()
    } else {
        workload.identity.description.clone()
    };
    for line in format::wrap(&description, width, 2) {
        put(buf, &line, theme.dim(), &mut y);
    }
    if workload.exited {
        let (sm, mem) = workload.last_seen.unwrap_or((0.0, 0));
        put(
            buf,
            &format!(
                "Exited. Last seen using {sm:.0}% of GPU time and holding {}.",
                format::bytes(mem)
            ),
            theme.text(),
            &mut y,
        );
        put(buf, &context_line(workload), theme.dim(), &mut y);
        return;
    }
    if let Some(explanation) = app.explanation_for(&workload.key) {
        for (index, line) in format::wrap(explanation, width.saturating_sub(2), 3)
            .into_iter()
            .enumerate()
        {
            let prefix = if index == 0 {
                format!("{} ", theme.glyphs.ai_mark)
            } else {
                "  ".to_string()
            };
            put(buf, &format!("{prefix}{line}"), theme.ai(), &mut y);
        }
    } else if app.explanation_pending(&workload.key) {
        put(
            buf,
            &format!("{} asking the AI helper…", theme.glyphs.ai_mark),
            theme.ai(),
            &mut y,
        );
    } else if app.ai.is_ready() {
        put(
            buf,
            "Press e to ask the AI helper why this is using the GPU.",
            theme.faint(),
            &mut y,
        );
    }

    let total = app.model.devices[app.device]
        .sample
        .as_ref()
        .map_or(0, |s| s.mem_total);
    let shown_mem = workload.shown_mem(app.now);
    let memory = if workload.has_memory_figure() {
        format!(
            "{} ({}% of video memory)",
            format::bytes(shown_mem),
            format::share_pct(shown_mem, total)
        )
    } else {
        "memory not reported".to_string()
    };
    let engines = format!(
        "video encode {}%, decode {}%",
        workload.calm_enc(),
        workload.calm_dec()
    );
    put(
        buf,
        &format!(
            "Now: {:.0}% GPU time · {memory} · {engines}",
            workload.shown_sm(app.now)
        ),
        theme.text(),
        &mut y,
    );
    put(buf, &context_line(workload), theme.dim(), &mut y);

    if y < bottom {
        let header = format!(
            "{:>7}  {:<9} {:>9} {:>5}  COMMAND",
            "PID", "USER", "VRAM", "GPU"
        );
        put(buf, &header, theme.faint(), &mut y);
    }
    for proc in &workload.procs {
        if y >= bottom {
            break;
        }
        let memory = proc.proc.mem_bytes.map_or("n/a".to_string(), format::bytes);
        let user = proc.meta.user.clone().unwrap_or_else(|| "?".into());
        let command = if proc.meta.cmdline.is_empty() {
            proc.meta.comm.clone()
        } else {
            proc.meta.command_line()
        };
        let line = format!(
            "{:>7}  {:<9} {:>9} {:>4}%  {}",
            proc.proc.pid,
            format::truncate(&user, 9),
            memory,
            workload.proc_calm_sm(proc.proc.pid),
            command
        );
        put(buf, &line, theme.text(), &mut y);
    }
}

fn context_line(workload: &Workload) -> String {
    let primary = &workload.procs[0].meta;
    let mut parts = Vec::new();
    if let Some(container) = &primary.container {
        parts.push(format!(
            "{} {}",
            container.runtime.label(),
            container.short_id()
        ));
    }
    if let Some(unit) = &primary.unit {
        parts.push(format!("service {unit}"));
    }
    if let Some(user) = &primary.user {
        parts.push(format!("user {user}"));
    }
    let count = workload.procs.len();
    parts.push(format!(
        "{count} process{}",
        if count == 1 { "" } else { "es" }
    ));
    parts.join(" · ")
}

fn render_history(buf: &mut Buffer, area: Rect, workload: &Workload, app: &App) {
    let theme = &app.theme;
    if area.height < 2 {
        return;
    }
    let device = &app.model.devices[app.device];
    let frames = app.window_frames();
    let width = usize::from(area.width);
    let seqs: Vec<u64> = device.recent_frames(frames).map(|f| f.seq).collect();
    let compute: Vec<u64> = device
        .recent_frames(frames)
        .map(|f| f.entry(workload.id).map_or(0, |e| u64::from(e.sm)))
        .collect();
    let memory: Vec<u64> = device
        .recent_frames(frames)
        .map(|f| f.entry(workload.id).map_or(0, |e| e.mem))
        .collect();
    let colour = theme.slot(workload.slot);
    let style = if theme.is_mono() {
        Style::default()
    } else {
        Style::default().fg(colour)
    };

    let label = format!("GPU time · last {}", format::duration(app.window()));
    widgets::text(
        buf,
        area.x,
        area.y,
        &format::truncate(&label, width),
        theme.dim(),
        area.width,
    );
    let first = Rect::new(area.x, area.y + 1, area.width, 1);
    Sparkline::default()
        .data(resample(&compute, &seqs, width, frames))
        .max(100)
        .style(style)
        .render(first, buf);
    if area.height >= 5 {
        widgets::text(
            buf,
            area.x,
            area.y + 3,
            "Video memory",
            theme.dim(),
            area.width,
        );
        let max = memory.iter().copied().max().unwrap_or(1).max(1);
        let second = Rect::new(area.x, area.y + 4, area.width, 1);
        Sparkline::default()
            .data(resample(&memory, &seqs, width, frames))
            .max(max)
            .style(style)
            .render(second, buf);
    }
}

/// Averages each anchored column of the window, so a finished column keeps its value as the window scrolls. Columns with no frames read as zero.
pub fn resample(values: &[u64], seqs: &[u64], width: usize, window_frames: usize) -> Vec<u64> {
    buckets::column_ranges(seqs, width, window_frames)
        .into_iter()
        .map(|range| {
            range
                .and_then(|range| values.get(range))
                .filter(|slice| !slice.is_empty())
                .map_or(0, |slice| slice.iter().sum::<u64>() / slice.len() as u64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_averages_each_anchored_column() {
        let seqs = [0, 1, 2, 3];
        assert_eq!(resample(&[10, 20, 30, 40], &seqs, 2, 4), vec![15, 35]);
    }

    #[test]
    fn resample_keeps_finished_columns_when_the_window_scrolls() {
        assert_eq!(
            resample(&[20, 30, 40, 50], &[1, 2, 3, 4], 2, 4),
            vec![35, 50]
        );
    }

    #[test]
    fn resample_leaves_columns_without_frames_at_zero() {
        assert_eq!(resample(&[1, 2], &[0, 1], 5, 5), vec![0, 0, 0, 1, 2]);
    }

    #[test]
    fn resample_handles_empty_and_zero_width() {
        assert_eq!(resample(&[], &[], 3, 3), vec![0, 0, 0]);
        assert!(resample(&[1], &[0], 0, 3).is_empty());
    }
}
