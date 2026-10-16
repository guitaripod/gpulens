//! Interface state and input handling. Drawing lives in `ui`; this module only decides what the user is looking at.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;

use crate::actions;
use crate::ai::{AiHandle, ExplainRequest, Job, NarrateRequest, Outcome};
use crate::config::Config;
use crate::event::Event;
use crate::model::{Model, Observation, Workload};
use crate::narrate::{self, Level, Story};
use crate::types::{Category, ProcMeta};
use crate::ui::theme::Theme;

pub const WINDOWS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
    Duration::from_secs(3600),
];
const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 5000, 10000];
const TOAST_LIFETIME: Duration = Duration::from_secs(5);
const BACKLOG_LIMIT: usize = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Impact,
    Memory,
    Name,
    Kind,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            Self::Impact => Self::Memory,
            Self::Memory => Self::Name,
            Self::Name => Self::Kind,
            Self::Kind => Self::Impact,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Impact => "impact",
            Self::Memory => "memory",
            Self::Name => "name",
            Self::Kind => "kind",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Compute,
    Memory,
}

#[derive(Clone, Debug)]
pub struct KillPrompt {
    pub name: String,
    pub targets: Vec<ProcMeta>,
    pub refusal: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Overlay {
    None,
    Help,
    Filter,
    ConfirmKill(KillPrompt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiStatus {
    Off,
    Connecting,
    Ready { model: String },
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct Narration {
    pub situation: u64,
    pub headline: String,
    pub tip: Option<String>,
}

#[derive(Debug)]
pub struct AiState {
    pub status: AiStatus,
    pub narration: Option<Narration>,
    pub explanations: HashMap<String, String>,
    pub learned_this_session: Vec<String>,
    pub last_problem: Option<String>,
    narration_pending: bool,
    explain_pending: HashSet<String>,
    last_narration_request: Option<Instant>,
}

impl AiState {
    fn new(status: AiStatus) -> Self {
        Self {
            status,
            narration: None,
            explanations: HashMap::new(),
            learned_this_session: Vec::new(),
            last_problem: None,
            narration_pending: false,
            explain_pending: HashSet::new(),
            last_narration_request: None,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.status, AiStatus::Ready { .. })
    }
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    pub until: Instant,
}

/// Screen regions recorded during the last draw so mouse clicks can be mapped back to rows.
#[derive(Clone, Debug, Default)]
pub struct Hitboxes {
    pub rows: Vec<(Rect, String)>,
    pub table: Rect,
}

/// Requests from the interface to the main loop, which owns the sampler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Refresh,
    SetInterval(Duration),
}

pub struct App {
    pub model: Model,
    pub theme: Theme,
    pub config: Config,
    pub device: usize,
    pub selection: Option<String>,
    pub selected_row: usize,
    pub user_chose: bool,
    pub scroll: usize,
    pub sort: SortMode,
    pub window_index: usize,
    pub metric: Metric,
    pub show_descriptions: bool,
    pub ai_wording: bool,
    pub filter: String,
    pub overlay: Overlay,
    pub paused: bool,
    pub ai: AiState,
    pub toast: Option<Toast>,
    pub backend_error: Option<String>,
    pub quit: bool,
    pub dirty: bool,
    pub hitboxes: Hitboxes,
    pub commands: Vec<Command>,
    ai_handle: Option<AiHandle>,
    backlog: Vec<Observation>,
}

impl App {
    pub fn new(model: Model, theme: Theme, config: Config, ai_handle: Option<AiHandle>) -> Self {
        let status = if ai_handle.is_some() {
            AiStatus::Connecting
        } else {
            AiStatus::Off
        };
        let window_index = WINDOWS
            .iter()
            .position(|w| *w >= config.window())
            .unwrap_or(1);
        Self {
            model,
            theme,
            device: 0,
            selection: None,
            selected_row: 0,
            user_chose: false,
            scroll: 0,
            sort: SortMode::Impact,
            window_index,
            metric: Metric::Compute,
            show_descriptions: true,
            ai_wording: true,
            filter: String::new(),
            overlay: Overlay::None,
            paused: false,
            ai: AiState::new(status),
            toast: None,
            backend_error: None,
            quit: false,
            dirty: true,
            hitboxes: Hitboxes::default(),
            commands: Vec::new(),
            ai_handle,
            backlog: Vec::new(),
            config,
        }
    }

    pub fn window(&self) -> Duration {
        WINDOWS[self.window_index]
    }

    pub fn window_frames(&self) -> usize {
        (self.window().as_secs_f64() / self.model.seconds_per_frame())
            .round()
            .max(2.0) as usize
    }

    pub fn story(&self) -> Story {
        narrate::story(&self.model.devices[self.device], self.model.interval)
    }

    /// The model-written headline, but only while it still describes the current situation.
    pub fn fresh_narration(&self) -> Option<&Narration> {
        let current = narrate::situation_signature(&self.model.devices[self.device]);
        self.ai
            .narration
            .as_ref()
            .filter(|n| n.situation == current)
    }

    /// Workloads after filtering and sorting, in the order the table shows them.
    pub fn visible(&self) -> Vec<&Workload> {
        let device = &self.model.devices[self.device];
        let needle = self.filter.trim().to_lowercase();
        let mut rows: Vec<&Workload> = device
            .workloads
            .iter()
            .filter(|w| needle.is_empty() || matches_filter(w, &needle))
            .collect();
        match self.sort {
            SortMode::Impact => {}
            SortMode::Memory => rows.sort_by_key(|w| std::cmp::Reverse(w.mem_bytes)),
            SortMode::Name => rows.sort_by_key(|w| w.display_name.to_lowercase()),
            SortMode::Kind => rows.sort_by(|a, b| {
                a.category()
                    .cmp(&b.category())
                    .then(b.sm.cmp(&a.sm))
                    .then(b.mem_bytes.cmp(&a.mem_bytes))
            }),
        }
        rows
    }

    pub fn selected(&self) -> Option<&Workload> {
        let key = self.selection.as_deref()?;
        self.model.devices[self.device].workload(key)
    }

    pub fn handle(&mut self, event: Event, now: Instant) {
        match event {
            Event::Input(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key, now)
            }
            Event::Input(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            Event::Input(TermEvent::Resize(..)) => self.dirty = true,
            Event::Input(_) => {}
            Event::Observation(observation) => self.on_observation(*observation, now),
            Event::BackendError(message) => {
                self.backend_error = Some(message);
                self.dirty = true;
            }
            Event::Ai(outcome) => self.on_ai(outcome, now),
            Event::Quit => self.quit = true,
        }
    }

    pub fn tick(&mut self, now: Instant) {
        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
            self.dirty = true;
        }
    }

    fn on_observation(&mut self, observation: Observation, now: Instant) {
        self.backend_error = None;
        if self.paused {
            if self.backlog.len() < BACKLOG_LIMIT {
                self.backlog.push(observation);
            }
            return;
        }
        self.model.ingest(observation);
        self.reconcile_selection();
        self.request_narration(now);
        self.dirty = true;
    }

    fn set_paused(&mut self, paused: bool, now: Instant) {
        self.paused = paused;
        if !paused {
            for observation in std::mem::take(&mut self.backlog) {
                self.model.ingest(observation);
            }
            self.reconcile_selection();
            self.request_narration(now);
        }
        self.dirty = true;
    }

    /// Keeps the highlighted row on the same application when the order changes, and on the same position when it disappears.
    pub fn reconcile_selection(&mut self) {
        let keys: Vec<String> = self.visible().iter().map(|w| w.key.clone()).collect();
        if keys.is_empty() {
            self.selection = None;
            self.selected_row = 0;
            return;
        }
        let index = if self.user_chose {
            self.selection
                .as_ref()
                .and_then(|key| keys.iter().position(|k| k == key))
                .unwrap_or_else(|| self.selected_row.min(keys.len() - 1))
        } else {
            0
        };
        self.selected_row = index;
        self.selection = Some(keys[index].clone());
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.visible().len();
        if count == 0 {
            return;
        }
        let target = (self.selected_row as isize + delta).clamp(0, count as isize - 1) as usize;
        self.select_row(target);
    }

    fn select_row(&mut self, row: usize) {
        let key = self.visible().get(row).map(|w| w.key.clone());
        if let Some(key) = key {
            self.selected_row = row;
            self.selection = Some(key);
            self.user_chose = true;
            self.dirty = true;
        }
    }

    fn on_key(&mut self, key: KeyEvent, now: Instant) {
        self.dirty = true;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.overlay.clone() {
            Overlay::Help => self.overlay = Overlay::None,
            Overlay::Filter => self.on_filter_key(key),
            Overlay::ConfirmKill(prompt) => self.on_confirm_key(key, prompt),
            Overlay::None => self.on_main_key(key, now),
        }
    }

    fn on_main_key(&mut self, key: KeyEvent, now: Instant) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.quit = true;
                } else {
                    self.filter.clear();
                    self.reconcile_selection();
                }
            }
            KeyCode::Char('?') | KeyCode::Char('h') | KeyCode::F(1) => self.overlay = Overlay::Help,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home | KeyCode::Char('g') => self.select_row(0),
            KeyCode::End | KeyCode::Char('G') => {
                let last = self.visible().len().saturating_sub(1);
                self.select_row(last);
            }
            KeyCode::Tab | KeyCode::Char(']') => self.switch_device(1),
            KeyCode::BackTab | KeyCode::Char('[') => self.switch_device(-1),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.reconcile_selection();
            }
            KeyCode::Char('t') => self.window_index = (self.window_index + 1) % WINDOWS.len(),
            KeyCode::Char('m') => {
                self.metric = match self.metric {
                    Metric::Compute => Metric::Memory,
                    Metric::Memory => Metric::Compute,
                }
            }
            KeyCode::Char('d') => self.show_descriptions = !self.show_descriptions,
            KeyCode::Char('a') => self.ai_wording = !self.ai_wording,
            KeyCode::Char(' ') | KeyCode::Char('p') => self.set_paused(!self.paused, now),
            KeyCode::Char('/') => self.overlay = Overlay::Filter,
            KeyCode::Char('x') | KeyCode::Delete => self.begin_kill(now),
            KeyCode::Char('e') => self.request_explanation(now),
            KeyCode::Char('r') => self.commands.push(Command::Refresh),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_interval(-1),
            KeyCode::Char('-') | KeyCode::Char('_') => self.change_interval(1),
            _ => {}
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.overlay = Overlay::None,
            KeyCode::Esc => {
                self.filter.clear();
                self.overlay = Overlay::None;
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) => self.filter.push(c),
            _ => {}
        }
        self.reconcile_selection();
    }

    fn on_confirm_key(&mut self, key: KeyEvent, prompt: KillPrompt) {
        let refused = prompt.refusal.is_some();
        match key.code {
            KeyCode::Char('y') if !refused => self.finish_kill(&prompt, false),
            KeyCode::Char('f') | KeyCode::Char('F') if !refused => self.finish_kill(&prompt, true),
            _ => self.overlay = Overlay::None,
        }
    }

    fn finish_kill(&mut self, prompt: &KillPrompt, force: bool) {
        let report = actions::terminate(&prompt.targets, force);
        let level = if report.signalled.is_empty() {
            Level::Warn
        } else {
            Level::Info
        };
        self.notify(report.summary(&prompt.name, force), level, Instant::now());
        self.overlay = Overlay::None;
        self.commands.push(Command::Refresh);
    }

    fn begin_kill(&mut self, now: Instant) {
        if !self.config.allow_kill {
            self.notify(
                "Stopping apps is disabled (allow_kill = false in the config).".into(),
                Level::Warn,
                now,
            );
            return;
        }
        let Some(workload) = self.selected() else {
            return;
        };
        let refusal = refusal_reason(workload.category(), &workload.display_name);
        self.overlay = Overlay::ConfirmKill(KillPrompt {
            name: workload.display_name.clone(),
            targets: workload.procs.iter().map(|p| p.meta.clone()).collect(),
            refusal,
        });
    }

    fn change_interval(&mut self, direction: isize) {
        let current = self.model.interval.as_millis() as u64;
        let index = INTERVALS_MS
            .iter()
            .position(|ms| *ms >= current)
            .unwrap_or(INTERVALS_MS.len() - 1) as isize;
        let next = (index + direction).clamp(0, INTERVALS_MS.len() as isize - 1) as usize;
        let interval = Duration::from_millis(INTERVALS_MS[next]);
        if interval != self.model.interval {
            self.model.interval = interval;
            self.commands.push(Command::SetInterval(interval));
            self.notify(
                format!("Sampling every {} ms", INTERVALS_MS[next]),
                Level::Info,
                Instant::now(),
            );
        }
    }

    fn switch_device(&mut self, direction: isize) {
        let count = self.model.devices.len() as isize;
        if count > 1 {
            self.device = (self.device as isize + direction).rem_euclid(count) as usize;
            self.selection = None;
            self.user_chose = false;
            self.reconcile_selection();
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .hitboxes
                    .rows
                    .iter()
                    .position(|(rect, _)| rect_contains(*rect, mouse.column, mouse.row));
                if let Some(index) = hit {
                    let row = self.scroll + index;
                    self.select_row(row);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    pub fn notify(&mut self, text: String, level: Level, now: Instant) {
        self.toast = Some(Toast {
            text,
            level,
            until: now + TOAST_LIFETIME,
        });
        self.dirty = true;
    }

    fn on_ai(&mut self, outcome: Outcome, now: Instant) {
        self.dirty = true;
        match outcome {
            Outcome::Ready { model, .. } => {
                self.ai.status = AiStatus::Ready { model };
                self.ai.last_problem = None;
            }
            Outcome::Unavailable(reason) => self.ai.status = AiStatus::Unavailable(reason),
            Outcome::Classified { name, .. } => {
                self.ai.last_problem = None;
                self.ai.learned_this_session.push(name.clone());
                self.notify(
                    format!("The AI helper identified a new program: {name}"),
                    Level::Info,
                    now,
                );
            }
            Outcome::Narrated {
                situation,
                headline,
                tip,
            } => {
                self.ai.narration_pending = false;
                self.ai.last_problem = None;
                self.ai.narration = Some(Narration {
                    situation,
                    headline,
                    tip,
                });
            }
            Outcome::Explained { key, text } => {
                self.ai.explain_pending.remove(&key);
                self.ai.last_problem = None;
                self.ai.explanations.insert(key, text);
            }
            Outcome::Rejected(_) => {
                self.ai.narration_pending = false;
                self.ai.explain_pending.clear();
            }
            Outcome::Failed(reason) => {
                self.ai.narration_pending = false;
                self.ai.explain_pending.clear();
                self.ai.last_problem = Some(reason);
            }
        }
    }

    /// Asks for a friendlier headline when the situation has changed and the cooldown has passed.
    fn request_narration(&mut self, now: Instant) {
        let Some(handle) = self.ai_handle.clone() else {
            return;
        };
        if !self.ai.is_ready() || !self.config.llm.narrate || self.ai.narration_pending {
            return;
        }
        let cooldown = Duration::from_secs(self.config.llm.narrate_cooldown_secs);
        if self
            .ai
            .last_narration_request
            .is_some_and(|at| now.duration_since(at) < cooldown)
        {
            return;
        }
        let device = &self.model.devices[self.device];
        let situation = narrate::situation_signature(device);
        if self
            .ai
            .narration
            .as_ref()
            .is_some_and(|n| n.situation == situation)
        {
            return;
        }
        let story = narrate::story(device, self.model.interval);
        let Some(facts) = narrate::facts(device, &story) else {
            return;
        };
        if handle.submit(Job::Narrate(NarrateRequest { situation, facts })) {
            self.ai.narration_pending = true;
            self.ai.last_narration_request = Some(now);
        }
    }

    fn request_explanation(&mut self, now: Instant) {
        let Some(handle) = self.ai_handle.clone() else {
            self.notify(
                "The AI helper is off. Run `gpulens setup` to enable it.".into(),
                Level::Info,
                now,
            );
            return;
        };
        if !self.ai.is_ready() {
            self.notify(
                "The AI helper is not connected yet.".into(),
                Level::Info,
                now,
            );
            return;
        }
        let Some(workload) = self.selected() else {
            return;
        };
        let key = workload.key.clone();
        let focus = format!(
            "{} ({})",
            workload.display_name, workload.identity.description
        );
        if self.ai.explain_pending.contains(&key) {
            return;
        }
        let device = &self.model.devices[self.device];
        let story = narrate::story(device, self.model.interval);
        let Some(facts) = narrate::facts(device, &story) else {
            return;
        };
        if handle.submit(Job::Explain(ExplainRequest {
            key: key.clone(),
            facts,
            focus,
        })) {
            self.ai.explain_pending.insert(key);
            self.notify("Asking the AI helper…".into(), Level::Info, now);
        }
    }

    pub fn explanation_pending(&self, key: &str) -> bool {
        self.ai.explain_pending.contains(key)
    }
}

fn refusal_reason(category: Category, name: &str) -> Option<String> {
    category
        .is_protected()
        .then(|| format!("{name} is part of your desktop or system. Stopping it could end your session, so gpulens will not do it."))
}

fn matches_filter(workload: &Workload, needle: &str) -> bool {
    workload.display_name.to_lowercase().contains(needle)
        || workload.category().label().to_lowercase().contains(needle)
        || workload
            .procs
            .iter()
            .any(|p| p.meta.command_line().to_lowercase().contains(needle))
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::*;
    use crate::types::{Category, DeviceInfo};
    use ratatui::crossterm::event::KeyEventState;

    fn app() -> App {
        let model = Model::new(
            vec![DeviceInfo::default()],
            Duration::from_secs(1),
            Duration::from_secs(3600),
        );
        App::new(
            model,
            Theme::new(crate::ui::theme::ColorMode::TrueColor, false, false),
            Config::default(),
            None,
        )
    }

    fn key(code: KeyCode) -> Event {
        Event::Input(TermEvent::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    fn feed_three(app: &mut App) {
        let procs = vec![
            observed(1, identity("a", "Alpha", Category::Ai), 5 * GIB, 40),
            observed(2, identity("b", "Beta", Category::Game), 9 * GIB, 10),
            observed(3, identity("c", "Gamma", Category::Desktop), GIB, 2),
        ];
        app.handle(
            Event::Observation(Box::new(Observation {
                devices: vec![sample(52, 15 * GIB)],
                processes: procs,
            })),
            Instant::now(),
        );
    }

    #[test]
    fn first_row_is_selected_after_data_arrives() {
        let mut a = app();
        feed_three(&mut a);
        assert_eq!(a.selection.as_deref(), Some("a"));
    }

    #[test]
    fn arrows_move_and_clamp() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Down), now);
        a.handle(key(KeyCode::Down), now);
        a.handle(key(KeyCode::Down), now);
        assert_eq!(a.selection.as_deref(), Some("c"));
        a.handle(key(KeyCode::Up), now);
        assert_eq!(a.selection.as_deref(), Some("b"));
    }

    #[test]
    fn sort_by_memory_reorders_but_keeps_the_chosen_app() {
        let mut a = app();
        feed_three(&mut a);
        a.handle(key(KeyCode::Down), Instant::now());
        a.handle(key(KeyCode::Up), Instant::now());
        a.handle(key(KeyCode::Char('s')), Instant::now());
        assert_eq!(a.sort, SortMode::Memory);
        let order: Vec<_> = a.visible().iter().map(|w| w.key.clone()).collect();
        assert_eq!(order, vec!["b", "a", "c"]);
        assert_eq!(a.selection.as_deref(), Some("a"));
    }

    #[test]
    fn selection_follows_the_top_row_until_the_user_chooses() {
        let mut a = app();
        feed_three(&mut a);
        a.handle(key(KeyCode::Char('s')), Instant::now());
        assert_eq!(a.selection.as_deref(), Some("b"));
    }

    #[test]
    fn filter_narrows_the_table() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Char('/')), now);
        for c in "bet".chars() {
            a.handle(key(KeyCode::Char(c)), now);
        }
        a.handle(key(KeyCode::Enter), now);
        let names: Vec<_> = a.visible().iter().map(|w| w.display_name.clone()).collect();
        assert_eq!(names, vec!["Beta"]);
        assert_eq!(a.selection.as_deref(), Some("b"));
        a.handle(key(KeyCode::Esc), now);
        assert_eq!(a.visible().len(), 3);
        assert!(!a.quit);
    }

    #[test]
    fn protected_apps_cannot_be_stopped() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Down), now);
        a.handle(key(KeyCode::Down), now);
        a.handle(key(KeyCode::Char('x')), now);
        match &a.overlay {
            Overlay::ConfirmKill(prompt) => assert!(prompt.refusal.is_some()),
            other => panic!("expected a refusal prompt, got {other:?}"),
        }
        a.handle(key(KeyCode::Char('y')), now);
        assert!(matches!(a.overlay, Overlay::None));
        assert!(a.toast.is_none());
    }

    #[test]
    fn enter_does_not_confirm_stopping_an_app() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Char('x')), now);
        match &a.overlay {
            Overlay::ConfirmKill(prompt) => assert!(prompt.refusal.is_none()),
            other => panic!("expected a stop prompt, got {other:?}"),
        }
        a.handle(key(KeyCode::Enter), now);
        assert!(matches!(a.overlay, Overlay::None));
        assert!(a.toast.is_none());
        assert!(a.commands.is_empty());
    }

    #[test]
    fn pause_holds_the_picture_and_resume_catches_up() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Char(' ')), now);
        feed_three(&mut a);
        feed_three(&mut a);
        assert_eq!(a.model.devices[0].frames.len(), 1);
        a.handle(key(KeyCode::Char(' ')), now);
        assert_eq!(a.model.devices[0].frames.len(), 3);
    }

    #[test]
    fn quit_keys() {
        let mut a = app();
        a.handle(key(KeyCode::Char('q')), Instant::now());
        assert!(a.quit);
        let mut b = app();
        b.handle(
            Event::Input(TermEvent::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))),
            Instant::now(),
        );
        assert!(b.quit);
    }

    #[test]
    fn interval_keys_issue_commands_within_bounds() {
        let mut a = app();
        let now = Instant::now();
        a.handle(key(KeyCode::Char('-')), now);
        assert_eq!(
            a.commands.last(),
            Some(&Command::SetInterval(Duration::from_secs(2)))
        );
        for _ in 0..10 {
            a.handle(key(KeyCode::Char('-')), now);
        }
        assert_eq!(a.model.interval, Duration::from_millis(10000));
    }

    #[test]
    fn toast_expires() {
        let mut a = app();
        let now = Instant::now();
        a.notify("hello".into(), Level::Info, now);
        a.tick(now + Duration::from_secs(1));
        assert!(a.toast.is_some());
        a.tick(now + Duration::from_secs(6));
        assert!(a.toast.is_none());
    }

    #[test]
    fn stale_narration_is_not_shown() {
        let mut a = app();
        feed_three(&mut a);
        a.ai.narration = Some(Narration {
            situation: 12345,
            headline: "old".into(),
            tip: None,
        });
        assert!(a.fresh_narration().is_none());
        let current = narrate::situation_signature(&a.model.devices[0]);
        a.ai.narration = Some(Narration {
            situation: current,
            headline: "new".into(),
            tip: None,
        });
        assert!(a.fresh_narration().is_some());
    }

    #[test]
    fn a_failure_is_remembered_until_the_helper_answers_again() {
        let mut a = app();
        let now = Instant::now();
        a.handle(
            Event::Ai(Outcome::Ready {
                endpoint: "http://x".into(),
                model: "m".into(),
            }),
            now,
        );
        a.handle(Event::Ai(Outcome::Failed("timeout".into())), now);
        assert_eq!(a.ai.last_problem.as_deref(), Some("timeout"));
        a.handle(Event::Ai(Outcome::Rejected("bad numbers".into())), now);
        assert_eq!(a.ai.last_problem.as_deref(), Some("timeout"));
        a.handle(
            Event::Ai(Outcome::Narrated {
                situation: 1,
                headline: "ok".into(),
                tip: None,
            }),
            now,
        );
        assert!(a.ai.last_problem.is_none());
    }

    #[test]
    fn disabling_kill_in_config_blocks_the_prompt() {
        let mut a = app();
        a.config.allow_kill = false;
        feed_three(&mut a);
        a.handle(key(KeyCode::Char('x')), Instant::now());
        assert!(matches!(a.overlay, Overlay::None));
        assert!(a.toast.is_some());
    }
}
