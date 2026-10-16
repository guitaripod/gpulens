//! Interface state and input handling. Drawing lives in `ui`; this module only decides what the user is looking at.

use std::collections::{HashMap, HashSet, VecDeque};
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
use crate::ui::layout::StoryNeeds;
use crate::ui::theme::Theme;

pub const WINDOWS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
    Duration::from_secs(3600),
];
const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 5000, 10000];
const TOAST_LIFETIME: Duration = Duration::from_secs(5);
const NARRATION_LIFETIME: Duration = Duration::from_secs(90);
const EXPLANATION_LIFETIME: Duration = Duration::from_secs(180);
const STALE_AFTER: Duration = Duration::from_secs(5);

/// Holds a value steady until a different one has been on offer for a while, so wording and verdicts do not flip back and forth when a reading hovers around a boundary. The key decides whether the offer is "the same thing"; the value is refreshed whenever the key matches.
#[derive(Debug)]
struct Settled<K, V> {
    shown: Option<(K, V)>,
    pending: Option<(K, Instant)>,
}

impl<K: PartialEq + Clone, V> Settled<K, V> {
    fn new() -> Self {
        Self {
            shown: None,
            pending: None,
        }
    }

    fn offer(&mut self, key: K, value: V, now: Instant, dwell: Duration) {
        let unchanged = self.shown.as_ref().is_none_or(|(shown, _)| *shown == key);
        if unchanged || dwell.is_zero() {
            self.shown = Some((key, value));
            self.pending = None;
            return;
        }
        match &self.pending {
            Some((waiting, since)) if *waiting == key => {
                if now.duration_since(*since) >= dwell {
                    self.shown = Some((key, value));
                    self.pending = None;
                }
            }
            _ => self.pending = Some((key, now)),
        }
    }

    fn value(&self) -> Option<&V> {
        self.shown.as_ref().map(|(_, value)| value)
    }

    fn key(&self) -> Option<&K> {
        self.shown.as_ref().map(|(key, _)| key)
    }
}

const ROOM_SHRINK_AFTER: Duration = Duration::from_secs(30);

/// How much room the story block holds on to. It grows the moment the wording needs more, but only gives space back after a long quiet spell, so the panels below it move rarely and then only once.
#[derive(Debug)]
pub struct RoomHold {
    headline: u16,
    headline_needed_at: Instant,
    notes: u16,
    notes_needed_at: Instant,
}

impl RoomHold {
    pub fn new(now: Instant) -> Self {
        Self {
            headline: 1,
            headline_needed_at: now,
            notes: 0,
            notes_needed_at: now,
        }
    }

    pub fn fit(&mut self, wanted: StoryNeeds, cap: StoryNeeds, now: Instant) -> StoryNeeds {
        hold_room(
            &mut self.headline,
            &mut self.headline_needed_at,
            wanted.headline_lines.max(1),
            now,
        );
        hold_room(
            &mut self.notes,
            &mut self.notes_needed_at,
            wanted.note_lines,
            now,
        );
        self.headline = self.headline.min(cap.headline_lines.max(1));
        self.notes = self.notes.min(cap.note_lines);
        StoryNeeds {
            headline_lines: self.headline,
            note_lines: self.notes,
        }
    }
}

fn hold_room(reserved: &mut u16, needed_at: &mut Instant, needed: u16, now: Instant) {
    if needed >= *reserved {
        *reserved = needed;
        *needed_at = now;
    } else if now.saturating_duration_since(*needed_at) >= ROOM_SHRINK_AFTER {
        *reserved -= 1;
        *needed_at = now;
    }
}

/// The story with every number blanked out: two stories with the same shape say the same thing.
fn story_shape(story: &Story) -> String {
    let mut shape = String::new();
    let mut previous_was_digit = false;
    let texts =
        std::iter::once(story.headline.as_str()).chain(story.notes.iter().map(|n| n.text.as_str()));
    for text in texts {
        for ch in text.chars() {
            if ch.is_ascii_digit() {
                if !previous_was_digit {
                    shape.push('#');
                }
                previous_was_digit = true;
            } else {
                shape.push(ch);
                previous_was_digit = false;
            }
        }
        shape.push('\n');
    }
    shape
}

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
    pub at: Instant,
}

#[derive(Clone, Debug)]
pub struct Explanation {
    pub text: String,
    pub at: Instant,
}

#[derive(Debug)]
pub struct AiState {
    pub status: AiStatus,
    pub narration: Option<Narration>,
    pub explanations: HashMap<String, Explanation>,
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
    pub now: Instant,
    pub room: RoomHold,
    pub toast: Option<Toast>,
    pub backend_error: Option<String>,
    pub stale_secs: Option<u64>,
    pub too_small: bool,
    pub quit: bool,
    pub dirty: bool,
    pub hitboxes: Hitboxes,
    pub commands: Vec<Command>,
    ai_handle: Option<AiHandle>,
    backlog: VecDeque<Observation>,
    last_observation: Option<Instant>,
    stories: Vec<Settled<String, Story>>,
    situations: Vec<Settled<u64, u64>>,
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
            now: Instant::now(),
            room: RoomHold::new(Instant::now()),
            toast: None,
            backend_error: None,
            stale_secs: None,
            too_small: false,
            quit: false,
            dirty: true,
            hitboxes: Hitboxes::default(),
            commands: Vec::new(),
            ai_handle,
            backlog: VecDeque::new(),
            last_observation: None,
            stories: Vec::new(),
            situations: Vec::new(),
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

    /// The plain-language story as it should be shown: held steady until a different one has persisted for a moment.
    pub fn story(&self) -> Story {
        self.stories
            .get(self.device)
            .and_then(|settled| settled.value().cloned())
            .unwrap_or_else(|| {
                narrate::story(&self.model.devices[self.device], self.model.interval)
            })
    }

    /// Runs after every reading: offers the freshly computed story and situation of each GPU to their settlers.
    fn refresh_calm_state(&mut self, now: Instant) {
        let dwell = self.model.tuning.settle;
        while self.stories.len() < self.model.devices.len() {
            self.stories.push(Settled::new());
            self.situations.push(Settled::new());
        }
        for (position, device) in self.model.devices.iter().enumerate() {
            let story = narrate::story(device, self.model.interval);
            self.stories[position].offer(story_shape(&story), story, now, dwell);
            let signature = narrate::situation_signature(device);
            self.situations[position].offer(signature, signature, now, dwell);
        }
    }

    /// The situation the interface has settled on; AI wording is matched against this rather than the flickering raw value.
    fn settled_situation(&self) -> Option<u64> {
        self.situations
            .get(self.device)
            .and_then(|s| s.key().copied())
    }

    /// The model-written headline, but only while it still describes the current situation and is recent enough that its numbers can be trusted.
    pub fn fresh_narration(&self) -> Option<&Narration> {
        if !self.ai_wording {
            return None;
        }
        let current = self.settled_situation()?;
        self.ai
            .narration
            .as_ref()
            .filter(|n| n.situation == current && n.at.elapsed() < NARRATION_LIFETIME)
    }

    /// An explanation of an app, shown for a few minutes after it was written because the figures it was based on move on.
    pub fn explanation_for(&self, key: &str) -> Option<&str> {
        self.ai
            .explanations
            .get(key)
            .filter(|e| e.at.elapsed() < EXPLANATION_LIFETIME)
            .map(|e| e.text.as_str())
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
            SortMode::Memory => {
                rows.sort_by_key(|w| std::cmp::Reverse(w.shown_mem(self.now)));
            }
            SortMode::Name => rows.sort_by_key(|w| w.display_name.to_lowercase()),
            SortMode::Kind => rows.sort_by_key(|w| w.category()),
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
        self.update_staleness(now);
    }

    /// Flags readings that have stopped arriving, so a frozen picture is never mistaken for a calm GPU.
    fn update_staleness(&mut self, now: Instant) {
        let threshold = (self.model.interval * 3).max(STALE_AFTER);
        let stale = self
            .last_observation
            .map(|at| now.duration_since(at))
            .filter(|age| *age > threshold)
            .map(|age| age.as_secs());
        if stale != self.stale_secs {
            self.stale_secs = stale;
            self.dirty = true;
        }
    }

    fn on_observation(&mut self, observation: Observation, now: Instant) {
        self.backend_error = None;
        self.last_observation = Some(now);
        if self.paused {
            self.backlog.push_back(observation);
            if self.backlog.len() > self.model.history_frames() {
                self.backlog.pop_front();
            }
            return;
        }
        self.model.ingest_at(observation, now);
        self.refresh_calm_state(now);
        self.reconcile_selection();
        self.request_narration(now);
        self.dirty = true;
    }

    fn set_paused(&mut self, paused: bool, now: Instant) {
        self.paused = paused;
        if !paused {
            for observation in std::mem::take(&mut self.backlog) {
                self.model.ingest_at(observation, now);
            }
            self.refresh_calm_state(now);
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

    /// Selects by identity, not by screen position, so a click lands on the app that was drawn there even if the order has changed since.
    fn select_key(&mut self, key: &str) {
        if let Some(row) = self.visible().iter().position(|w| w.key == key) {
            self.select_row(row);
        }
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
        if self.too_small && matches!(self.overlay, Overlay::ConfirmKill(_)) {
            self.overlay = Overlay::None;
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
        if workload.exited {
            let message = format!("{} has already exited.", workload.display_name);
            self.notify(message, Level::Info, now);
            return;
        }
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
            self.model.set_interval(interval);
            self.backlog.clear();
            self.commands.push(Command::SetInterval(interval));
            self.notify(
                format!(
                    "Sampling every {} ms (history restarted)",
                    INTERVALS_MS[next]
                ),
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
                    let key = self.hitboxes.rows[index].1.clone();
                    self.select_key(&key);
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
                    at: now,
                });
            }
            Outcome::Explained { key, text } => {
                self.ai.explain_pending.remove(&key);
                self.ai.last_problem = None;
                self.ai
                    .explanations
                    .insert(key, Explanation { text, at: now });
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
        let Some(situation) = self.settled_situation() else {
            return;
        };
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
    fn missing_readings_are_flagged_as_stale() {
        let mut a = app();
        let start = Instant::now();
        a.handle(
            Event::Observation(Box::new(Observation {
                devices: vec![sample(10, GIB)],
                processes: vec![],
            })),
            start,
        );
        a.tick(start + Duration::from_secs(2));
        assert_eq!(a.stale_secs, None);
        a.tick(start + Duration::from_secs(12));
        assert_eq!(a.stale_secs, Some(12));
        a.handle(
            Event::Observation(Box::new(Observation {
                devices: vec![sample(10, GIB)],
                processes: vec![],
            })),
            start + Duration::from_secs(13),
        );
        a.tick(start + Duration::from_secs(14));
        assert_eq!(a.stale_secs, None);
    }

    #[test]
    fn a_stop_prompt_cannot_be_confirmed_while_the_window_is_too_small_to_show_it() {
        let mut a = app();
        feed_three(&mut a);
        let now = Instant::now();
        a.handle(key(KeyCode::Char('x')), now);
        assert!(matches!(a.overlay, Overlay::ConfirmKill(_)));
        a.too_small = true;
        a.handle(key(KeyCode::Char('y')), now);
        assert!(matches!(a.overlay, Overlay::None));
        assert!(a.toast.is_none());
    }

    #[test]
    fn clicks_select_the_app_that_was_drawn_not_whatever_sits_there_now() {
        use ratatui::crossterm::event::MouseEvent;
        let mut a = app();
        feed_three(&mut a);
        a.hitboxes.rows = vec![
            (Rect::new(0, 5, 40, 1), "c".to_string()),
            (Rect::new(0, 6, 40, 1), "a".to_string()),
        ];
        a.handle(
            Event::Input(TermEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 5,
                modifiers: KeyModifiers::NONE,
            })),
            Instant::now(),
        );
        assert_eq!(a.selection.as_deref(), Some("c"));
    }

    #[test]
    fn pausing_keeps_the_newest_readings_and_drops_the_oldest() {
        let model = Model::new(
            vec![DeviceInfo::default()],
            Duration::from_secs(1),
            Duration::from_secs(3),
        );
        let mut a = App::new(
            model,
            Theme::new(crate::ui::theme::ColorMode::TrueColor, false, false),
            Config::default(),
            None,
        );
        let now = Instant::now();
        a.handle(key(KeyCode::Char(' ')), now);
        for load in [10, 20, 30, 40, 50] {
            a.handle(
                Event::Observation(Box::new(Observation {
                    devices: vec![sample(load, GIB)],
                    processes: vec![],
                })),
                now,
            );
        }
        a.handle(key(KeyCode::Char(' ')), now);
        let loads: Vec<u8> = a.model.devices[0]
            .frames
            .iter()
            .map(|f| f.gpu_util)
            .collect();
        assert_eq!(loads, vec![30, 40, 50]);
    }

    #[test]
    fn old_narrations_and_explanations_expire() {
        let mut a = app();
        feed_three(&mut a);
        let current = narrate::situation_signature(&a.model.devices[0]);
        let old = Instant::now()
            .checked_sub(Duration::from_secs(600))
            .unwrap_or_else(Instant::now);
        a.ai.narration = Some(Narration {
            situation: current,
            headline: "old news".into(),
            tip: None,
            at: old,
        });
        a.ai.explanations.insert(
            "a".into(),
            Explanation {
                text: "old".into(),
                at: old,
            },
        );
        if old != Instant::now() && old.elapsed() > Duration::from_secs(500) {
            assert!(a.fresh_narration().is_none());
            assert!(a.explanation_for("a").is_none());
        }
        a.ai.explanations.insert(
            "b".into(),
            Explanation {
                text: "fresh".into(),
                at: Instant::now(),
            },
        );
        assert_eq!(a.explanation_for("b"), Some("fresh"));
    }

    struct NoisyScenario {
        seed: u64,
    }

    impl NoisyScenario {
        fn noise(&mut self, amplitude: i32) -> i32 {
            self.seed = self
                .seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.seed >> 33) as i32 % (2 * amplitude + 1)) - amplitude
        }

        fn load(&mut self, base: i32, amplitude: i32) -> u8 {
            (base + self.noise(amplitude)).clamp(0, 100) as u8
        }

        fn tick(&mut self, index: usize) -> Observation {
            let drift = if index < 300 { 3 } else { 1 };
            let loads = [
                self.load(45, 8),
                self.load(41, 8),
                self.load(12, 6),
                self.load(4, 3),
                0,
                0,
                self.load(drift, 3),
            ];
            let names = [
                "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf",
            ];
            let memory = [8, 6, 2, 1, 3, 1, 2];
            let processes: Vec<_> = loads
                .iter()
                .enumerate()
                .map(|(i, load)| {
                    observed(
                        i as u32 + 1,
                        identity(names[i], names[i], Category::Other),
                        memory[i] * GIB,
                        *load,
                    )
                })
                .collect();
            let busy: u32 = loads.iter().map(|l| u32::from(*l)).sum::<u32>() + 2;
            Observation {
                devices: vec![sample(busy.min(100) as u8, 23 * GIB)],
                processes,
            }
        }
    }

    #[derive(Default)]
    struct Jitter {
        row_moves: usize,
        label_changes: usize,
        story_changes: usize,
    }

    fn measure(tuning: crate::model::Tuning) -> Jitter {
        let model = Model::new(
            vec![DeviceInfo::default()],
            Duration::from_secs(1),
            Duration::from_secs(900),
        )
        .with_tuning(tuning);
        let mut a = App::new(
            model,
            Theme::new(crate::ui::theme::ColorMode::TrueColor, false, false),
            Config::default(),
            None,
        );
        let mut scenario = NoisyScenario { seed: 7 };
        let start = Instant::now();
        let mut jitter = Jitter::default();
        let mut previous_order: Vec<String> = Vec::new();
        let mut previous_labels: HashMap<String, &'static str> = HashMap::new();
        let mut previous_shape = String::new();
        for index in 0..600 {
            let now = start + Duration::from_secs(index as u64);
            a.handle(Event::Observation(Box::new(scenario.tick(index))), now);
            if index < 20 {
                previous_order = a.visible().iter().map(|w| w.key.clone()).collect();
                previous_labels = a
                    .visible()
                    .iter()
                    .map(|w| (w.key.clone(), w.activity.label()))
                    .collect();
                previous_shape = story_shape(&a.story());
                continue;
            }
            let order: Vec<String> = a.visible().iter().map(|w| w.key.clone()).collect();
            jitter.row_moves += order
                .iter()
                .zip(&previous_order)
                .filter(|(x, y)| x != y)
                .count();
            previous_order = order;
            for workload in a.visible() {
                if previous_labels.insert(workload.key.clone(), workload.activity.label())
                    != Some(workload.activity.label())
                {
                    jitter.label_changes += 1;
                }
            }
            let shape = story_shape(&a.story());
            if shape != previous_shape {
                jitter.story_changes += 1;
                previous_shape = shape;
            }
        }
        jitter
    }

    #[test]
    fn calm_tuning_removes_most_of_the_jitter_on_noisy_readings() {
        let raw = measure(crate::model::Tuning::RAW);
        let calm = measure(crate::model::Tuning::calm());
        assert!(
            raw.row_moves > 100,
            "the scenario must actually be noisy: {}",
            raw.row_moves
        );
        assert!(
            calm.row_moves * 5 <= raw.row_moves,
            "rows moved {} vs {}",
            calm.row_moves,
            raw.row_moves
        );
        assert!(
            calm.label_changes * 3 <= raw.label_changes,
            "labels {} vs {}",
            calm.label_changes,
            raw.label_changes
        );
        assert!(
            calm.story_changes * 3 <= raw.story_changes,
            "stories {} vs {}",
            calm.story_changes,
            raw.story_changes
        );
    }

    #[test]
    fn an_exited_app_cannot_be_stopped() {
        use crate::model::Tuning;
        let model = Model::new(
            vec![DeviceInfo::default()],
            Duration::from_secs(1),
            Duration::from_secs(60),
        )
        .with_tuning(Tuning {
            ghost: Duration::from_secs(5),
            ..Tuning::RAW
        });
        let mut a = App::new(
            model,
            Theme::new(crate::ui::theme::ColorMode::TrueColor, false, false),
            Config::default(),
            None,
        );
        let now = Instant::now();
        feed_three(&mut a);
        a.handle(
            Event::Observation(Box::new(Observation {
                devices: vec![sample(5, GIB)],
                processes: vec![observed(
                    3,
                    identity("c", "Gamma", Category::Desktop),
                    GIB,
                    2,
                )],
            })),
            now,
        );
        a.handle(key(KeyCode::Char('x')), now);
        assert!(matches!(a.overlay, Overlay::None));
        assert!(
            a.toast
                .as_ref()
                .is_some_and(|t| t.text.contains("already exited"))
        );
    }

    #[test]
    fn story_room_grows_at_once_and_shrinks_only_after_a_long_quiet_spell() {
        let t0 = Instant::now();
        let mut room = RoomHold::new(t0);
        let cap = StoryNeeds {
            headline_lines: 2,
            note_lines: 3,
        };
        let want = |h, n| StoryNeeds {
            headline_lines: h,
            note_lines: n,
        };
        let fitted = room.fit(want(1, 0), cap, t0);
        assert_eq!((fitted.headline_lines, fitted.note_lines), (1, 0));
        let fitted = room.fit(want(2, 2), cap, t0 + Duration::from_secs(1));
        assert_eq!((fitted.headline_lines, fitted.note_lines), (2, 2));
        let fitted = room.fit(want(1, 0), cap, t0 + Duration::from_secs(20));
        assert_eq!((fitted.headline_lines, fitted.note_lines), (2, 2));
        let fitted = room.fit(want(1, 0), cap, t0 + Duration::from_secs(40));
        assert_eq!((fitted.headline_lines, fitted.note_lines), (1, 1));
        let fitted = room.fit(want(1, 0), cap, t0 + Duration::from_secs(75));
        assert_eq!((fitted.headline_lines, fitted.note_lines), (1, 0));
        let tight = StoryNeeds {
            headline_lines: 1,
            note_lines: 1,
        };
        let fitted = room.fit(want(2, 3), tight, t0 + Duration::from_secs(80));
        assert_eq!((fitted.headline_lines, fitted.note_lines), (1, 1));
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
            at: Instant::now(),
        });
        assert!(a.fresh_narration().is_none());
        let current = narrate::situation_signature(&a.model.devices[0]);
        a.ai.narration = Some(Narration {
            situation: current,
            headline: "new".into(),
            tip: None,
            at: Instant::now(),
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
