//! The history panel: who used the GPU, minute by minute, as stacked columns coloured by app.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Widget};

use super::stack::Segment;
use super::widgets::{self, TAG_HIDDEN, tag_for};
use crate::app::{App, Metric};
use crate::format;
use crate::model::{DeviceView, Frame, Model, WorkloadId};

const AXIS_WIDTH: u16 = 6;
const MIN_CHART_HEIGHT: u16 = 3;

/// One chart's worth of columns, oldest on the left.
#[derive(Debug, PartialEq)]
pub struct ChartData {
    pub columns: Vec<Vec<Segment>>,
    pub total: f64,
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let window = format::duration(app.window());
    let heading = if area.width >= 60 {
        format!(" Who used the GPU · last {window} ")
    } else {
        format!(" History · {window} ")
    };
    let mut block = Block::bordered()
        .border_style(theme.border())
        .title(Line::styled(heading, theme.title()));
    if area.width >= 64 {
        block =
            block.title_top(Line::styled(" t window · m metric ", theme.faint()).right_aligned());
    }
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width < AXIS_WIDTH + 8 || inner.height < MIN_CHART_HEIGHT + 2 {
        return;
    }
    let legend_height = 1;
    let axis_height = 1;
    let charts = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height - legend_height - axis_height,
    );
    let both = charts.height >= 2 * (MIN_CHART_HEIGHT + 1);
    if both {
        let top_height = charts.height * 3 / 5;
        render_chart(
            buf,
            Rect::new(charts.x, charts.y, charts.width, top_height),
            app,
            Metric::Compute,
        );
        let rest = Rect::new(
            charts.x,
            charts.y + top_height,
            charts.width,
            charts.height - top_height,
        );
        render_chart(buf, rest, app, Metric::Memory);
    } else {
        render_chart(buf, charts, app, app.metric);
    }
    render_time_axis(
        buf,
        Rect::new(inner.x, charts.y + charts.height, inner.width, 1),
        app,
    );
    render_legend(
        buf,
        Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
        app,
    );
}

fn render_chart(buf: &mut Buffer, area: Rect, app: &App, metric: Metric) {
    let theme = &app.theme;
    let device = &app.model.devices[app.device];
    let caption = match metric {
        Metric::Compute => "GPU time used",
        Metric::Memory => "Video memory used",
    };
    widgets::text(
        buf,
        area.x + 1,
        area.y,
        caption,
        theme.dim(),
        area.width.saturating_sub(2),
    );
    let plot = Rect::new(
        area.x + AXIS_WIDTH,
        area.y + 1,
        area.width - AXIS_WIDTH - 1,
        area.height - 1,
    );
    if plot.height == 0 || plot.width == 0 {
        return;
    }
    let data = build(
        &app.model,
        device,
        metric,
        usize::from(plot.width),
        app.window_frames(),
    );
    for (index, column) in data.columns.iter().enumerate() {
        widgets::stacked_column(buf, plot.x + index as u16, plot, column, data.total, theme);
    }
    render_axis(buf, area.x, plot, metric, data.total, app);
}

fn render_axis(buf: &mut Buffer, x: u16, plot: Rect, metric: Metric, total: f64, app: &App) {
    let style = app.theme.faint();
    let (top, middle) = match metric {
        Metric::Compute => ("100%".to_string(), "50%".to_string()),
        Metric::Memory => (
            format::bytes(total as u64),
            format::bytes((total / 2.0) as u64),
        ),
    };
    widgets::text_right(buf, x + AXIS_WIDTH - 1, plot.y, &top, style, AXIS_WIDTH - 1);
    if plot.height >= 5 {
        widgets::text_right(
            buf,
            x + AXIS_WIDTH - 1,
            plot.y + plot.height / 2,
            &middle,
            style,
            AXIS_WIDTH - 1,
        );
    }
    widgets::text_right(
        buf,
        x + AXIS_WIDTH - 1,
        plot.y + plot.height - 1,
        "0",
        style,
        AXIS_WIDTH - 1,
    );
}

fn render_time_axis(buf: &mut Buffer, area: Rect, app: &App) {
    let style = app.theme.faint();
    let start = area.x + AXIS_WIDTH;
    let width = area.width.saturating_sub(AXIS_WIDTH + 1);
    let ago = format!("-{}", format::duration(app.window()));
    widgets::text(buf, start, area.y, &ago, style, width);
    widgets::text_right(buf, start + width, area.y, "now", style, width);
}

fn render_legend(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let device = &app.model.devices[app.device];
    let mut x = area.x + 1;
    let limit = area.x + area.width;
    for (id, _) in ranked_in_window(device, app.window_frames())
        .into_iter()
        .take(8)
    {
        let name = format::truncate(app.model.name_of(id), 18);
        let needed = 2 + unicode_width::UnicodeWidthStr::width(name.as_str()) as u16 + 2;
        if x + needed > limit {
            break;
        }
        let (swatch, style) = widgets::swatch(theme, app.model.slot_of(id));
        widgets::text(buf, x, area.y, &swatch, style, 1);
        x += 2;
        x += widgets::text(buf, x, area.y, &name, theme.dim(), limit - x) + 2;
    }
}

/// Every app that appeared in the window, biggest total GPU time first, then biggest memory. Apps that have since left still count.
pub fn ranked_in_window(
    device: &DeviceView,
    window_frames: usize,
) -> Vec<(WorkloadId, (u64, u64))> {
    let mut totals: Vec<(WorkloadId, (u64, u64))> = Vec::new();
    for frame in device.recent_frames(window_frames) {
        for entry in &frame.entries {
            match totals.iter_mut().find(|(id, _)| *id == entry.id) {
                Some((_, (sm, mem))) => {
                    *sm += u64::from(entry.sm);
                    *mem += entry.mem;
                }
                None => totals.push((entry.id, (u64::from(entry.sm), entry.mem))),
            }
        }
    }
    totals.sort_by_key(|(id, key)| (std::cmp::Reverse(*key), *id));
    totals
}

/// Averages history into one column per terminal cell. The newest frame lands in the last column; a short history leaves the left side empty.
pub fn build(
    model: &Model,
    device: &DeviceView,
    metric: Metric,
    width: usize,
    window_frames: usize,
) -> ChartData {
    let frames: Vec<&Frame> = device.recent_frames(window_frames).collect();
    let offset = window_frames.saturating_sub(frames.len());
    let total = match metric {
        Metric::Compute => 100.0,
        Metric::Memory => device.info.memory_total.max(1) as f64,
    };
    let columns = (0..width)
        .map(|column| {
            let from = column * window_frames / width;
            let to = ((column + 1) * window_frames / width).max(from + 1);
            let lo = from.saturating_sub(offset).min(frames.len());
            let hi = to.saturating_sub(offset).min(frames.len());
            if from < offset && to <= offset || lo >= hi {
                return Vec::new();
            }
            column_segments(model, &frames[lo..hi], metric)
        })
        .collect();
    ChartData { columns, total }
}

fn column_segments(model: &Model, frames: &[&Frame], metric: Metric) -> Vec<Segment> {
    let count = frames.len() as f64;
    let mut sums: Vec<(WorkloadId, f64)> = Vec::new();
    let mut device_total = 0.0;
    for frame in frames {
        device_total += match metric {
            Metric::Compute => f64::from(frame.gpu_util),
            Metric::Memory => frame.mem_used as f64,
        };
        for entry in &frame.entries {
            let value = match metric {
                Metric::Compute => f64::from(entry.sm),
                Metric::Memory => entry.mem as f64,
            };
            match sums.iter_mut().find(|(id, _)| *id == entry.id) {
                Some((_, sum)) => *sum += value,
                None => sums.push((entry.id, value)),
            }
        }
    }
    sums.sort_by_key(|(id, _)| *id);
    let mut segments: Vec<Segment> = sums
        .iter()
        .filter(|(_, sum)| *sum > 0.0)
        .map(|(id, sum)| Segment {
            value: sum / count,
            tag: tag_for(model.slot_of(*id)),
        })
        .collect();
    let claimed: f64 = segments.iter().map(|s| s.value).sum();
    let hidden = device_total / count - claimed;
    if hidden > 0.0 {
        segments.push(Segment {
            value: hidden,
            tag: TAG_HIDDEN,
        });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::*;
    use crate::types::Category;

    fn busy_model(ticks: usize) -> Model {
        let mut m = model();
        for _ in 0..ticks {
            feed(
                &mut m,
                60,
                10 * GIB,
                vec![
                    observed(1, identity("a", "A", Category::Ai), 6 * GIB, 40),
                    observed(2, identity("b", "B", Category::Game), 3 * GIB, 10),
                ],
            );
        }
        m
    }

    #[test]
    fn short_history_is_right_aligned() {
        let m = busy_model(10);
        let data = build(&m, &m.devices[0], Metric::Compute, 60, 60);
        assert_eq!(data.columns.len(), 60);
        assert!(data.columns[..45].iter().all(Vec::is_empty));
        assert!(!data.columns[59].is_empty());
    }

    #[test]
    fn columns_stack_apps_then_unclaimed_remainder() {
        let m = busy_model(5);
        let data = build(&m, &m.devices[0], Metric::Compute, 10, 5);
        let column = &data.columns[9];
        assert_eq!(column.len(), 3);
        assert_eq!(column[0].value, 40.0);
        assert_eq!(column[1].value, 10.0);
        assert_eq!(column[2].tag, TAG_HIDDEN);
        assert_eq!(column[2].value, 10.0);
    }

    #[test]
    fn memory_chart_uses_the_card_size_as_total() {
        let m = busy_model(3);
        let data = build(&m, &m.devices[0], Metric::Memory, 6, 3);
        assert_eq!(data.total, 96.0 * GIB as f64);
        let column = &data.columns[5];
        assert_eq!(column[0].value, 6.0 * GIB as f64);
    }

    #[test]
    fn long_windows_average_many_frames_per_column() {
        let m = busy_model(60);
        let data = build(&m, &m.devices[0], Metric::Compute, 6, 60);
        assert!(data.columns.iter().all(|c| !c.is_empty()));
        assert!((data.columns[0][0].value - 40.0).abs() < 1e-9);
    }

    #[test]
    fn legend_ranks_by_gpu_time_and_remembers_departed_apps() {
        let mut m = model();
        for _ in 0..3 {
            feed(
                &mut m,
                60,
                10 * GIB,
                vec![
                    observed(1, identity("a", "A", Category::Ai), GIB, 10),
                    observed(2, identity("b", "B", Category::Game), 3 * GIB, 40),
                ],
            );
        }
        feed(
            &mut m,
            5,
            GIB,
            vec![observed(1, identity("a", "A", Category::Ai), GIB, 5)],
        );
        let ranked: Vec<_> = ranked_in_window(&m.devices[0], 10)
            .into_iter()
            .map(|(id, _)| m.name_of(id).to_string())
            .collect();
        assert_eq!(ranked, vec!["B", "A"]);
    }

    #[test]
    fn empty_history_gives_empty_columns() {
        let m = model();
        let data = build(&m, &m.devices[0], Metric::Compute, 8, 30);
        assert!(data.columns.iter().all(Vec::is_empty));
    }
}
