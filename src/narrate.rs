//! Turns raw readings into plain-language sentences, and checks that a language model's rewrite stayed truthful.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use serde::Serialize;

use crate::format::{self, GIB};
use crate::model::{Activity, DeviceView, Workload};

const IDLE_HOLD_SECS: u64 = 10;
const LOW_MEMORY_FRACTION: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    pub text: String,
}

/// What to tell the user right now: one sentence about the GPU, one about memory, and a few findings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Story {
    pub headline: String,
    pub memory_line: String,
    pub notes: Vec<Note>,
}

pub fn story(device: &DeviceView, interval: Duration) -> Story {
    let Some(sample) = device.sample.as_ref() else {
        return Story {
            headline: "Waiting for the first reading…".into(),
            memory_line: String::new(),
            notes: Vec::new(),
        };
    };
    Story {
        headline: headline(device),
        memory_line: memory_line(device),
        notes: notes(device, sample.gpu_util, interval),
    }
}

fn busy_phrase(util: u8) -> &'static str {
    match util {
        0..=4 => "idle",
        5..=24 => "lightly used",
        25..=59 => "moderately busy",
        60..=89 => "busy",
        _ => "maxed out",
    }
}

fn working_set(device: &DeviceView) -> Vec<&Workload> {
    let mut active: Vec<&Workload> = device.workloads.iter().filter(|w| w.sm > 0).collect();
    active.sort_by_key(|w| std::cmp::Reverse(w.sm));
    active
}

fn headline(device: &DeviceView) -> String {
    let util = device.sample.as_ref().map_or(0, |s| s.gpu_util);
    let active = working_set(device);
    let phrase = busy_phrase(util);
    if util < 5 {
        return idle_headline(device, util);
    }
    if device
        .sample
        .as_ref()
        .is_some_and(|s| s.per_process_unsupported)
    {
        return unattributable_headline(device, util);
    }
    match active.as_slice() {
        [] => format!(
            "The GPU is {phrase} ({util}%), but no visible app claims the work: another user, a container or the driver may be behind it."
        ),
        [only] => format!(
            "The GPU is {phrase} ({util}%). {} is doing the work ({}%).",
            only.display_name, only.sm
        ),
        [top, rest @ ..] if u32::from(top.sm) * 100 >= u32::from(util) * 60 => format!(
            "The GPU is {phrase} ({util}%). {} is doing most of the work ({}%){}.",
            top.display_name,
            top.sm,
            others_clause(rest)
        ),
        many => format!(
            "The GPU is {phrase} ({util}%). The work is split between {}.",
            format::join_names(
                &many
                    .iter()
                    .take(3)
                    .map(|w| format!("{} ({}%)", w.display_name, w.sm))
                    .collect::<Vec<_>>()
            )
        ),
    }
}

/// Some GPUs (older cards, MIG, virtual GPUs) cannot say which process is busy, so only memory can be attributed.
fn unattributable_headline(device: &DeviceView, util: u8) -> String {
    let mut holders: Vec<&Workload> = device.workloads.iter().collect();
    holders.sort_by_key(|w| std::cmp::Reverse(w.mem_bytes));
    let named: Vec<String> = holders
        .iter()
        .take(3)
        .map(|w| format!("{} ({})", w.display_name, format::bytes(w.mem_bytes)))
        .collect();
    let lead = format!(
        "The GPU is {} ({util}%), but this GPU does not report which app is responsible.",
        busy_phrase(util)
    );
    if named.is_empty() {
        lead
    } else {
        format!(
            "{lead} The biggest memory holders are {}.",
            format::join_names(&named)
        )
    }
}

fn others_clause(rest: &[&Workload]) -> String {
    let names: Vec<String> = rest
        .iter()
        .take(2)
        .map(|w| w.display_name.clone())
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!(", with {} on the side", format::join_names(&names))
    }
}

fn idle_headline(device: &DeviceView, util: u8) -> String {
    let mut holders: Vec<&Workload> = device
        .workloads
        .iter()
        .filter(|w| w.mem_bytes >= 256 * 1024 * 1024)
        .collect();
    holders.sort_by_key(|w| std::cmp::Reverse(w.mem_bytes));
    match holders.as_slice() {
        [] => format!("The GPU is idle ({util}%). Nothing is using it right now."),
        [only] => format!(
            "The GPU is idle ({util}%). {} is holding {} of memory.",
            only.display_name,
            format::bytes(only.mem_bytes)
        ),
        many => {
            let named: Vec<String> = many
                .iter()
                .take(3)
                .map(|w| format!("{} ({})", w.display_name, format::bytes(w.mem_bytes)))
                .collect();
            format!(
                "The GPU is idle ({util}%): nothing is doing real work. The biggest memory holders are {}.",
                format::join_names(&named)
            )
        }
    }
}

fn memory_line(device: &DeviceView) -> String {
    let Some(sample) = device.sample.as_ref() else {
        return String::new();
    };
    format!(
        "{} of video memory in use ({}%), {} free.",
        format::bytes(sample.mem_used),
        format::share_pct(sample.mem_used, sample.mem_total),
        format::bytes(sample.mem_free())
    )
}

fn notes(device: &DeviceView, util: u8, interval: Duration) -> Vec<Note> {
    let mut notes = Vec::new();
    notes.extend(memory_pressure_note(device));
    notes.extend(heat_note(device));
    notes.extend(power_note(device, util));
    notes.extend(idle_holder_note(device, interval));
    notes.extend(encoder_note(device));
    notes.extend(hidden_load_note(device, util));
    notes.truncate(4);
    notes
}

fn note(level: Level, text: String) -> Option<Note> {
    Some(Note { level, text })
}

fn memory_pressure_note(device: &DeviceView) -> Option<Note> {
    let sample = device.sample.as_ref()?;
    let low = sample.mem_total > 0
        && (sample.mem_free() as f64) < sample.mem_total as f64 * LOW_MEMORY_FRACTION;
    if !low {
        return None;
    }
    note(
        Level::Warn,
        format!(
            "Only {} of video memory is left. New models or games may fail to load or run slowly.",
            format::bytes(sample.mem_free())
        ),
    )
}

fn heat_note(device: &DeviceView) -> Option<Note> {
    let sample = device.sample.as_ref()?;
    if !(sample.throttle.thermal || sample.throttle.hardware) {
        return None;
    }
    let temp = sample
        .temp_c
        .map(|t| format!(" ({t}°C)"))
        .unwrap_or_default();
    note(
        Level::Warn,
        format!("The GPU is running hot{temp} and slowing itself down to cool off."),
    )
}

fn power_note(device: &DeviceView, util: u8) -> Option<Note> {
    let sample = device.sample.as_ref()?;
    if !sample.throttle.power || util < 50 {
        return None;
    }
    let detail = match (sample.power_w, sample.power_limit_w) {
        (Some(draw), Some(limit)) => format!(" ({draw:.0} of {limit:.0} W)"),
        _ => String::new(),
    };
    note(
        Level::Info,
        format!("The GPU is at its power limit{detail}, so it cannot clock any higher."),
    )
}

fn idle_holder_note(device: &DeviceView, interval: Duration) -> Option<Note> {
    let sample = device.sample.as_ref()?;
    let threshold = (GIB).max(sample.mem_total / 33);
    let min_ticks = (IDLE_HOLD_SECS as f64 / interval.as_secs_f64()).ceil() as u32;
    let holders: Vec<&Workload> = device
        .workloads
        .iter()
        .filter(|w| w.activity == Activity::Idle && w.idle_ticks >= min_ticks)
        .filter(|w| w.mem_bytes >= threshold)
        .collect();
    let total: u64 = holders.iter().map(|w| w.mem_bytes).sum();
    match holders.as_slice() {
        [] => None,
        [one] => note(
            Level::Info,
            format!(
                "{} is idle but still holds {} ({}% of video memory). Closing it would free that.",
                one.display_name,
                format::bytes(one.mem_bytes),
                format::share_pct(one.mem_bytes, sample.mem_total)
            ),
        ),
        many => {
            let names: Vec<String> = many
                .iter()
                .take(3)
                .map(|w| w.display_name.clone())
                .collect();
            note(
                Level::Info,
                format!(
                    "{} are idle but together hold {}. Closing them would free that.",
                    format::join_names(&names),
                    format::bytes(total)
                ),
            )
        }
    }
}

fn encoder_note(device: &DeviceView) -> Option<Note> {
    let top = device
        .workloads
        .iter()
        .filter(|w| w.enc >= 10)
        .max_by_key(|w| w.enc)?;
    note(
        Level::Info,
        format!(
            "{} is using the video encoder ({}%): that means recording or streaming video.",
            top.display_name, top.enc
        ),
    )
}

fn hidden_load_note(device: &DeviceView, util: u8) -> Option<Note> {
    let hidden = device.unattributed_sm();
    let unsupported = device
        .sample
        .as_ref()
        .is_some_and(|s| s.per_process_unsupported);
    if unsupported || util < 20 || u32::from(hidden) * 100 < u32::from(util) * 60 {
        return None;
    }
    note(
        Level::Info,
        format!(
            "{hidden}% of the load is not tied to any process I can see. Other users, containers or the desktop may be responsible."
        ),
    )
}

/// The facts a language model is allowed to talk about. Every number in a rewrite must come from here.
#[derive(Clone, Debug, Serialize)]
pub struct Facts {
    pub gpu: String,
    pub gpu_busy_percent: u8,
    pub memory_used: String,
    pub memory_total: String,
    pub memory_free: String,
    pub apps: Vec<FactApp>,
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FactApp {
    pub name: String,
    pub kind: String,
    pub what_it_is: String,
    pub compute_percent: u8,
    pub memory: String,
    pub state: String,
}

pub fn facts(device: &DeviceView, story: &Story) -> Option<Facts> {
    let sample = device.sample.as_ref()?;
    Some(Facts {
        gpu: device.info.name.clone(),
        gpu_busy_percent: sample.gpu_util,
        memory_used: format::bytes(sample.mem_used),
        memory_total: format::bytes(sample.mem_total),
        memory_free: format::bytes(sample.mem_free()),
        apps: device
            .workloads
            .iter()
            .take(8)
            .map(|w| FactApp {
                name: w.display_name.clone(),
                kind: w.identity.category.label().to_string(),
                what_it_is: w.identity.description.clone(),
                compute_percent: w.sm,
                memory: format::bytes(w.mem_bytes),
                state: w.activity.label().to_string(),
            })
            .collect(),
        findings: story.notes.iter().map(|n| n.text.clone()).collect(),
    })
}

/// A coarse fingerprint of the situation; a new narration is only worth requesting when it changes. Light flicker from tiny background apps is deliberately ignored.
pub fn situation_signature(device: &DeviceView) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Some(sample) = device.sample.as_ref() {
        (sample.gpu_util / 25).hash(&mut hasher);
        (format::share_pct(sample.mem_used, sample.mem_total) / 10).hash(&mut hasher);
        sample.throttle.is_limited().hash(&mut hasher);
    }
    let mut leaders: Vec<&Workload> = device
        .workloads
        .iter()
        .filter(|w| w.activity >= Activity::Working)
        .collect();
    leaders.sort_by_key(|w| (std::cmp::Reverse(w.sm), w.key.clone()));
    for workload in leaders.iter().take(3) {
        workload.key.hash(&mut hasher);
    }
    if let Some(biggest) = device
        .workloads
        .iter()
        .max_by_key(|w| (w.mem_bytes, w.key.clone()))
    {
        biggest.key.hash(&mut hasher);
    }
    hasher.finish()
}

/// Every number token in `text`, as written (`78.4`, `96`, `3`).
fn numbers_in(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, out: &mut Vec<f64>| {
        let trimmed = current.trim_end_matches('.');
        if let Ok(value) = trimmed.parse::<f64>() {
            out.push(value);
        }
        current.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_digit() || (ch == '.' && !current.is_empty()) {
            current.push(ch);
        } else {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);
    out
}

/// True when every number in `candidate` can be traced to the facts (allowing rounding), so a rewrite cannot invent figures.
pub fn is_grounded(candidate: &str, facts: &Facts) -> bool {
    let Ok(serialized) = serde_json::to_string(facts) else {
        return false;
    };
    let allowed: Vec<f64> = numbers_in(&serialized);
    let seen: HashSet<u64> = allowed.iter().map(|v| v.to_bits()).collect();
    numbers_in(candidate).into_iter().all(|value| {
        value <= 10.0
            || seen.contains(&value.to_bits())
            || allowed
                .iter()
                .any(|a| (value - a).abs() <= (a * 0.03).max(0.6))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testing::*;
    use crate::types::Category;

    fn run(gpu_util: u8, mem_used: u64, procs: Vec<crate::model::ObservedProc>) -> Story {
        let mut m = model();
        feed(&mut m, gpu_util, mem_used, procs);
        story(&m.devices[0], m.interval)
    }

    #[test]
    fn empty_device_is_idle() {
        let s = run(0, GIB, vec![]);
        assert!(
            s.headline.starts_with("The GPU is idle (0%)"),
            "{}",
            s.headline
        );
        assert!(s.notes.is_empty());
    }

    #[test]
    fn idle_with_holders_names_them() {
        let id = identity("cs2", "Counter-Strike 2", Category::Game);
        let s = run(0, 9 * GIB, vec![observed(1, id, 8 * GIB, 0)]);
        assert!(
            s.headline
                .contains("Counter-Strike 2 is holding 8.0 GB of memory"),
            "{}",
            s.headline
        );
    }

    #[test]
    fn single_active_app_gets_the_credit() {
        let id = identity("vllm", "vLLM", Category::Ai);
        let s = run(62, 31 * GIB, vec![observed(1, id, 30 * GIB, 60)]);
        assert_eq!(
            s.headline,
            "The GPU is busy (62%). vLLM is doing the work (60%)."
        );
    }

    #[test]
    fn dominant_app_is_named_with_the_others_on_the_side() {
        let a = identity("a", "vLLM", Category::Ai);
        let b = identity("b", "KWin", Category::Desktop);
        let s = run(
            70,
            5 * GIB,
            vec![observed(1, a, 4 * GIB, 60), observed(2, b, GIB, 8)],
        );
        assert!(
            s.headline
                .contains("vLLM is doing most of the work (60%), with KWin on the side"),
            "{}",
            s.headline
        );
    }

    #[test]
    fn shared_load_lists_the_top_three() {
        let procs = vec![
            observed(1, identity("a", "A", Category::Ai), GIB, 30),
            observed(2, identity("b", "B", Category::Game), GIB, 25),
            observed(3, identity("c", "C", Category::Media), GIB, 20),
        ];
        let s = run(80, 3 * GIB, procs);
        assert!(
            s.headline
                .contains("split between A (30%), B (25%) and C (20%)"),
            "{}",
            s.headline
        );
    }

    #[test]
    fn load_without_visible_processes_is_explained() {
        let s = run(40, 3 * GIB, vec![]);
        assert!(
            s.headline.contains("no visible app claims the work"),
            "{}",
            s.headline
        );
        assert!(
            s.notes
                .iter()
                .any(|n| n.text.contains("not tied to any process"))
        );
    }

    #[test]
    fn gpus_without_per_process_figures_are_honest_about_it() {
        let mut m = model();
        feed(
            &mut m,
            70,
            10 * GIB,
            vec![observed(
                1,
                identity("a", "Alpha", Category::Ai),
                8 * GIB,
                0,
            )],
        );
        m.devices[0]
            .sample
            .as_mut()
            .unwrap()
            .per_process_unsupported = true;
        let s = story(&m.devices[0], m.interval);
        assert!(
            s.headline
                .contains("does not report which app is responsible"),
            "{}",
            s.headline
        );
        assert!(s.headline.contains("Alpha (8.0 GB)"), "{}", s.headline);
        assert!(
            s.notes
                .iter()
                .all(|n| !n.text.contains("not tied to any process"))
        );
    }

    #[test]
    fn low_memory_warns() {
        let id = identity("vllm", "vLLM", Category::Ai);
        let s = run(10, 90 * GIB, vec![observed(1, id, 88 * GIB, 10)]);
        assert!(
            s.notes
                .iter()
                .any(|n| n.level == Level::Warn && n.text.contains("Only"))
        );
    }

    #[test]
    fn idle_holder_note_needs_ten_idle_seconds() {
        let id = identity("cs2", "Counter-Strike 2", Category::Game);
        let mut m = model();
        for _ in 0..3 {
            feed(
                &mut m,
                0,
                9 * GIB,
                vec![observed(1, id.clone(), 8 * GIB, 0)],
            );
        }
        assert!(
            story(&m.devices[0], m.interval)
                .notes
                .iter()
                .all(|n| !n.text.contains("idle but still holds"))
        );
        for _ in 0..10 {
            feed(
                &mut m,
                0,
                9 * GIB,
                vec![observed(1, id.clone(), 8 * GIB, 0)],
            );
        }
        let notes = story(&m.devices[0], m.interval).notes;
        assert!(
            notes.iter().any(|n| n
                .text
                .contains("Counter-Strike 2 is idle but still holds 8.0 GB (8% of video memory)")),
            "{notes:?}"
        );
    }

    #[test]
    fn encoder_use_is_called_out() {
        let id = identity("sunshine", "Sunshine", Category::Media);
        let mut p = observed(1, id, GIB, 1);
        p.proc.util.enc = 28;
        let s = run(10, 2 * GIB, vec![p]);
        assert!(
            s.notes
                .iter()
                .any(|n| n.text.contains("Sunshine is using the video encoder (28%)"))
        );
    }

    #[test]
    fn notes_are_capped_at_four() {
        let s = run(0, 0, vec![]);
        assert!(s.notes.len() <= 4);
    }

    #[test]
    fn grounding_accepts_facts_and_rounding_but_not_inventions() {
        let id = identity("vllm", "vLLM", Category::Ai);
        let mut m = model();
        feed(&mut m, 62, 31 * GIB, vec![observed(1, id, 30 * GIB, 60)]);
        let st = story(&m.devices[0], m.interval);
        let f = facts(&m.devices[0], &st).unwrap();
        assert!(is_grounded(
            "vLLM is using 60% of the GPU and 30.0 GB of memory.",
            &f
        ));
        assert!(is_grounded("About 62% busy with roughly 30 GB held.", &f));
        assert!(!is_grounded("vLLM is using 85% of the GPU.", &f));
        assert!(!is_grounded("It holds 55 GB.", &f));
        assert!(is_grounded("Two apps share the card.", &f));
    }

    #[test]
    fn signature_ignores_light_flicker_from_background_apps() {
        let a = identity("a", "A", Category::Ai);
        let tiny = identity("t", "Tiny", Category::Other);
        let mut m1 = model();
        feed(
            &mut m1,
            60,
            10 * GIB,
            vec![
                observed(1, a.clone(), 8 * GIB, 55),
                observed(2, tiny.clone(), 10, 0),
            ],
        );
        let mut m2 = model();
        feed(
            &mut m2,
            60,
            10 * GIB,
            vec![observed(1, a, 8 * GIB, 55), observed(2, tiny, 10, 2)],
        );
        assert_eq!(
            situation_signature(&m1.devices[0]),
            situation_signature(&m2.devices[0])
        );
    }

    #[test]
    fn signature_ignores_small_wobble_but_sees_a_new_leader() {
        let a = identity("a", "A", Category::Ai);
        let b = identity("b", "B", Category::Game);
        let mut m1 = model();
        feed(&mut m1, 60, 10 * GIB, vec![observed(1, a.clone(), GIB, 55)]);
        let mut m2 = model();
        feed(&mut m2, 62, 10 * GIB, vec![observed(1, a, GIB, 58)]);
        let mut m3 = model();
        feed(&mut m3, 60, 10 * GIB, vec![observed(2, b, GIB, 55)]);
        assert_eq!(
            situation_signature(&m1.devices[0]),
            situation_signature(&m2.devices[0])
        );
        assert_ne!(
            situation_signature(&m1.devices[0]),
            situation_signature(&m3.devices[0])
        );
    }

    #[test]
    fn numbers_are_extracted_without_trailing_dots() {
        assert_eq!(numbers_in("78.4 GB of 96. Done 3."), vec![78.4, 96.0, 3.0]);
    }
}
