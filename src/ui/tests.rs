use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::draw;
use super::theme::{ColorMode, Theme};
use crate::app::{App, KillPrompt, Overlay};
use crate::classify::Identifier;
use crate::config::Config;
use crate::gpu::GpuBackend;
use crate::gpu::demo::DemoBackend;
use crate::model::Model;
use crate::sampler::Sampler;
use crate::types::DeviceInfo;

fn demo_app(theme: Theme) -> App {
    let backend = DemoBackend::starting_at(120);
    let infos = backend.devices().to_vec();
    let mut sampler = Sampler::new(Box::new(backend), Identifier::without_catalog(), None);
    let mut model = Model::new(infos, Duration::from_secs(1), Duration::from_secs(3600));
    for _ in 0..180 {
        model.ingest(sampler.poll().expect("demo sample"));
    }
    let mut app = App::new(model, theme, Config::default(), None);
    app.reconcile_selection();
    app
}

fn truecolor() -> Theme {
    Theme::new(ColorMode::TrueColor, false, false)
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|frame| draw(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            let line: String = (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn wide_layout_snapshot() {
    let mut app = demo_app(truecolor());
    insta::assert_snapshot!(render(&mut app, 150, 46));
}

#[test]
fn medium_layout_snapshot() {
    let mut app = demo_app(truecolor());
    insta::assert_snapshot!(render(&mut app, 100, 30));
}

#[test]
fn compact_layout_snapshot() {
    let mut app = demo_app(truecolor());
    insta::assert_snapshot!(render(&mut app, 80, 24));
}

#[test]
fn ascii_layout_uses_only_ascii_characters() {
    let mut app = demo_app(Theme::new(ColorMode::Mono, true, false));
    let screen = render(&mut app, 120, 40);
    let offenders: Vec<char> = screen.chars().filter(|c| !c.is_ascii()).collect();
    assert!(
        offenders.is_empty(),
        "non-ascii in ascii mode: {offenders:?}\n{screen}"
    );
}

#[test]
fn every_size_renders_without_panicking() {
    let mut app = demo_app(truecolor());
    for width in (40..=220).step_by(7) {
        for height in (8..=70).step_by(3) {
            let screen = render(&mut app, width, height);
            if width >= 60 && height >= 14 {
                assert!(screen.contains("gpulens"), "{width}x{height}");
                assert!(screen.contains("quit"), "{width}x{height}\n{screen}");
            } else {
                assert!(screen.contains("needs a window"), "{width}x{height}");
            }
        }
    }
}

#[test]
fn too_small_terminal_explains_itself() {
    let mut app = demo_app(truecolor());
    let screen = render(&mut app, 50, 10);
    assert!(screen.contains("at least 60×14"), "{screen}");
}

#[test]
fn before_the_first_reading_nothing_panics() {
    let model = Model::new(
        vec![DeviceInfo::default()],
        Duration::from_secs(1),
        Duration::from_secs(60),
    );
    let mut app = App::new(model, truecolor(), Config::default(), None);
    let screen = render(&mut app, 120, 40);
    assert!(screen.contains("Waiting for the first reading"), "{screen}");
}

#[test]
fn backend_failure_is_shown_in_place_of_the_story() {
    let mut app = demo_app(truecolor());
    app.backend_error = Some("GPU 0 stopped responding".into());
    let screen = render(&mut app, 120, 40);
    assert!(
        screen.contains("Cannot read the GPU: GPU 0 stopped responding"),
        "{screen}"
    );
    assert!(screen.contains("NO SIGNAL"), "{screen}");
}

#[test]
fn filter_with_no_match_says_so() {
    let mut app = demo_app(truecolor());
    app.filter = "zzzzzz".into();
    app.reconcile_selection();
    let screen = render(&mut app, 120, 40);
    assert!(screen.contains("No app matches the filter"), "{screen}");
}

#[test]
fn help_overlay_lists_the_keys() {
    let mut app = demo_app(truecolor());
    app.overlay = Overlay::Help;
    let screen = render(&mut app, 120, 40);
    assert!(screen.contains("how to read this"), "{screen}");
    assert!(screen.contains("stop the selected app"), "{screen}");
}

#[test]
fn kill_prompt_shows_the_processes_and_choices() {
    let mut app = demo_app(truecolor());
    let workload = app.selected().expect("selection").clone();
    app.overlay = Overlay::ConfirmKill(KillPrompt {
        name: workload.display_name.clone(),
        targets: workload.procs.iter().map(|p| p.meta.clone()).collect(),
        refusal: None,
    });
    let screen = render(&mut app, 120, 40);
    assert!(
        screen.contains(&format!("Stop {}?", workload.display_name)),
        "{screen}"
    );
    assert!(
        screen.contains("SIGTERM") && screen.contains("SIGKILL"),
        "{screen}"
    );
}

#[test]
fn clicking_a_row_selects_it() {
    use ratatui::crossterm::event::{
        Event as TermEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use crate::event::Event;

    let mut app = demo_app(truecolor());
    render(&mut app, 150, 46);
    let (rect, key) = app.hitboxes.rows[3].clone();
    app.handle(
        Event::Input(TermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 4,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        })),
        std::time::Instant::now(),
    );
    assert_eq!(app.selection.as_deref(), Some(key.as_str()));
    assert!(app.user_chose);
}

#[test]
fn many_workloads_scroll_with_the_selection() {
    let mut app = demo_app(truecolor());
    app.show_descriptions = true;
    for _ in 0..8 {
        app.handle(
            crate::event::Event::Input(ratatui::crossterm::event::Event::Key(
                ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::End),
            )),
            std::time::Instant::now(),
        );
    }
    let screen = render(&mut app, 100, 20);
    let last = app.visible().last().expect("rows").display_name.clone();
    assert!(screen.contains(&last), "{screen}");
}
