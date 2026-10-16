//! Everything that draws. `draw` is the only entry point; panels are free functions over a buffer.

pub mod detail;
pub mod hardware;
pub mod header;
pub mod layout;
pub mod overlay;
pub mod stack;
pub mod table;
pub mod theme;
pub mod timeline;
pub mod widgets;

#[cfg(test)]
mod tests;

use ratatui::Frame;

use crate::app::{App, Overlay};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let story = app.story();
    let view = header::story_view(app, &story);
    let needs = header::needs(&view, area.width);
    let Some(regions) = layout::regions(area, needs) else {
        overlay::render_too_small(frame.buffer_mut(), area, app);
        return;
    };
    let buf = frame.buffer_mut();
    header::render_title(buf, regions.title, app);
    header::render_story(buf, regions.story, app, &view, needs);
    header::render_bars(buf, regions.bars, app);
    let outcome = table::render(buf, regions.table, app);
    if let Some(timeline_area) = regions.timeline {
        timeline::render(buf, timeline_area, app);
    }
    if let Some(detail_area) = regions.detail {
        detail::render(buf, detail_area, app);
    }
    if let Some(hardware_area) = regions.hardware {
        hardware::render(buf, hardware_area, app);
    }
    overlay::render_footer(buf, regions.footer, app);
    match app.overlay.clone() {
        Overlay::Help => overlay::render_help(buf, area, app),
        Overlay::ConfirmKill(prompt) => overlay::render_confirm(buf, area, app, &prompt),
        Overlay::Filter | Overlay::None => {}
    }
    if app.theme.ascii {
        widgets::asciify(buf);
    }
    app.hitboxes.rows = outcome.rows;
    app.hitboxes.table = regions.table;
    app.scroll = outcome.scroll;
}
