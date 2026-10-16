//! Footer, toast, help screen and the stop-this-app confirmation.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};

use super::layout::centered;
use super::widgets;
use crate::app::{App, KillPrompt, Overlay};
use crate::format;
use crate::narrate::Level;

pub fn render_footer(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    if matches!(app.overlay, Overlay::Filter) {
        let prompt = format!(" Filter: {}▏  Enter keeps it, Esc clears it", app.filter);
        widgets::text(buf, area.x, area.y, &prompt, theme.accent(), area.width);
        return;
    }
    if let Some(toast) = &app.toast {
        let style = match toast.level {
            Level::Warn => theme.warn(),
            Level::Info => theme.accent(),
        };
        let message = format!(
            " {}",
            format::truncate(&toast.text, usize::from(area.width.saturating_sub(2)))
        );
        widgets::text(buf, area.x, area.y, &message, style, area.width);
        return;
    }
    let badges = badges(app);
    let badge_width = unicode_width::UnicodeWidthStr::width(badges.0.as_str()) as u16;
    let limit = area.width.saturating_sub(badge_width + 1);
    let mut x = area.x + 1;
    for (key, label) in hints(app) {
        let needed = (key.chars().count() + label.chars().count() + 3) as u16;
        if x + needed > area.x + limit {
            break;
        }
        x += widgets::text(buf, x, area.y, &key, theme.key_hint(), limit) + 1;
        x += widgets::text(buf, x, area.y, &label, theme.dim(), limit) + 2;
    }
    widgets::text_right(
        buf,
        area.x + area.width,
        area.y,
        &badges.0,
        badges.1,
        badge_width,
    );
}

fn hints(app: &App) -> Vec<(String, String)> {
    let hint = |key: &str, label: String| (key.to_string(), label);
    vec![
        hint("q", "quit".into()),
        hint("↑↓", "select".into()),
        hint("s", format!("sort: {}", app.sort.label())),
        hint("t", format!("window: {}", format::duration(app.window()))),
        hint("/", "filter".into()),
        hint(
            "space",
            if app.paused {
                "resume".into()
            } else {
                "pause".into()
            },
        ),
        hint("x", "stop app".into()),
        hint("e", "ask AI".into()),
        hint("d", "details".into()),
        hint("?", "help".into()),
    ]
}

fn badges(app: &App) -> (String, Style) {
    let theme = &app.theme;
    let every = format!("{} ms", app.model.interval.as_millis());
    if app.paused {
        (format!("PAUSED · {every} "), theme.warn())
    } else if app.backend_error.is_some() {
        ("NO SIGNAL ".to_string(), theme.bad())
    } else {
        (format!("live · {every} "), theme.good())
    }
}

pub fn render_help(buf: &mut Buffer, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered(area, 78, 26);
    Clear.render(popup, buf);
    let block = Block::bordered()
        .border_style(theme.border_focus())
        .title(Line::styled(" gpulens · how to read this ", theme.title()));
    let inner = block.inner(popup);
    block.render(popup, buf);
    let heading = |text: &str| Line::styled(text.to_string(), theme.accent());
    let body = |text: &str| Line::styled(text.to_string(), theme.text());
    let key = |k: &str, text: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<12}"), theme.key_hint()),
            Span::styled(text.to_string(), theme.text()),
        ])
    };
    let mark = theme.glyphs.ai_mark;
    let lines = vec![
        heading("What you are looking at"),
        body("  Every app has one colour, used in the bars, the list and the history chart."),
        body("  GPU bar     share of GPU time each app is using right now."),
        body("  VRAM bar    video memory each app holds. The dark track is free memory."),
        body("  busy / working / light   the app is doing work.   idle   it only holds memory."),
        body("  unclaimed   load the driver cannot tie to a process you can see:"),
        body("              other users, containers, or the desktop itself."),
        Line::styled(
            format!(
                "  {mark} marks text the local AI helper wrote or identified. Everything else is measured."
            ),
            theme.ai(),
        ),
        Line::raw(""),
        heading("Keys"),
        key("↑ ↓  j k", "select an app       Home/End  first/last"),
        key("Tab  [ ]", "switch GPU (when you have more than one)"),
        key("s", "sort: impact, memory, name, kind"),
        key(
            "t   m",
            "history window: 1m, 5m, 15m, 1h  ·  chart metric on small screens",
        ),
        key("/", "filter by name, kind or command"),
        key(
            "d   a",
            "show or hide the one-line explanations  ·  AI wording or plain wording",
        ),
        key(
            "space",
            "pause the picture (sampling continues in the background)",
        ),
        key("+  -", "sample faster or slower"),
        key(
            "x",
            "stop the selected app (asks first, never touches desktop or system apps)",
        ),
        key(
            "e",
            "ask the AI helper why the selected app is using the GPU",
        ),
        key("r   q", "refresh now  ·  quit"),
        Line::raw(""),
        Line::styled("  Press any key to close.", theme.faint()),
    ];
    Paragraph::new(lines).render(inner, buf);
}

pub fn render_confirm(buf: &mut Buffer, area: Rect, app: &App, prompt: &KillPrompt) {
    let theme = &app.theme;
    let popup = centered(area, 70, 11);
    Clear.render(popup, buf);
    let title = match &prompt.refusal {
        Some(_) => format!(" {} can't be stopped here ", prompt.name),
        None => format!(" Stop {}? ", prompt.name),
    };
    let block = Block::bordered()
        .border_style(if prompt.refusal.is_some() {
            theme.warn()
        } else {
            theme.border_focus()
        })
        .title(Line::styled(title, theme.title()));
    let inner = block.inner(popup);
    block.render(popup, buf);
    let mut lines = Vec::new();
    match &prompt.refusal {
        Some(reason) => {
            lines.push(Line::styled(reason.clone(), theme.text()));
            lines.push(Line::raw(""));
            lines.push(Line::styled("Press any key to close.", theme.faint()));
        }
        None => {
            let pids: Vec<String> = prompt
                .targets
                .iter()
                .take(6)
                .map(|t| t.pid.to_string())
                .collect();
            let more = prompt.targets.len().saturating_sub(6);
            let list = if more > 0 {
                format!("{} and {more} more", pids.join(", "))
            } else {
                pids.join(", ")
            };
            lines.push(Line::styled(
                format!("{} process(es): {list}", prompt.targets.len()),
                theme.text(),
            ));
            lines.push(Line::styled(
                "Anything unsaved in the app will be lost.",
                theme.warn(),
            ));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("y", theme.key_hint()),
                Span::styled("  ask it to quit politely (SIGTERM)", theme.text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("f", theme.key_hint()),
                Span::styled("  force it to end now (SIGKILL)", theme.text()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("n", theme.key_hint()),
                Span::styled("  cancel", theme.text()),
            ]));
        }
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .render(inner, buf);
}

pub fn render_too_small(buf: &mut Buffer, area: Rect, app: &App) {
    let message = format!(
        "gpulens needs a window of at least {}×{} (this one is {}×{}).",
        super::layout::MIN_WIDTH,
        super::layout::MIN_HEIGHT,
        area.width,
        area.height
    );
    let popup = centered(area, area.width, 3);
    Paragraph::new(message)
        .style(app.theme.warn())
        .wrap(Wrap { trim: true })
        .render(popup, buf);
}
