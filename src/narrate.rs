//! Turns raw readings into plain-language sentences, and checks that a language model's rewrite stayed truthful.

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
    if device.sample.is_none() {
        return Story {
            headline: "Waiting for the first reading…".into(),
            memory_line: String::new(),
            notes: Vec::new(),
        };
    }
    Story {
        headline: headline(device),
        memory_line: memory_line(device),
        notes: notes(device, device.calm_util(), interval),
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
    let mut active: Vec<&Workload> = device.live().filter(|w| w.calm_sm() > 0).collect();
    active.sort_by_key(|w| std::cmp::Reverse(w.calm_sm()));
    active
}

fn headline(device: &DeviceView) -> String {
    let util = device.calm_util();
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
    let claimed = claimed_percent(&active, util);
    if claimed * 100 < u32::from(util) * 50 {
        return underclaimed_headline(phrase, util, claimed, &active);
    }
    match active.as_slice() {
        [only] => format!(
            "The GPU is {phrase} ({util}%). {} is doing the work ({}%).",
            only.display_name,
            only.calm_sm()
        ),
        [top, rest @ ..] if u32::from(top.calm_sm()) * 100 >= u32::from(util) * 60 => format!(
            "The GPU is {phrase} ({util}%). {} is doing most of the work ({}%){}.",
            top.display_name,
            top.calm_sm(),
            others_clause(rest)
        ),
        many => format!(
            "The GPU is {phrase} ({util}%). The work is split between {}.",
            format::join_names(
                &many
                    .iter()
                    .take(3)
                    .map(|w| format!("{} ({}%)", w.display_name, w.calm_sm()))
                    .collect::<Vec<_>>()
            )
        ),
    }
}

/// Busy time that visible apps account for, never more than the card's own figure.
fn claimed_percent(active: &[&Workload], util: u8) -> u32 {
    active
        .iter()
        .map(|w| u32::from(w.calm_sm()))
        .sum::<u32>()
        .min(u32::from(util))
}

/// Used when apps explain less than half of the busy figure, so the headline does not credit them with the work.
fn underclaimed_headline(phrase: &str, util: u8, claimed: u32, active: &[&Workload]) -> String {
    if active.is_empty() {
        return format!(
            "The GPU is {phrase} ({util}%), but no visible app claims the work: another user, a container or the driver may be behind it."
        );
    }
    let named: Vec<String> = active
        .iter()
        .take(3)
        .map(|w| format!("{} ({}%)", w.display_name, w.calm_sm()))
        .collect();
    format!(
        "The GPU is {phrase} ({util}%), but only {claimed}% of that is tied to apps I can see: {}.",
        format::join_names(&named)
    )
}

/// Some GPUs (older cards, MIG, virtual GPUs) cannot say which process is busy, so only memory can be attributed.
fn unattributable_headline(device: &DeviceView, util: u8) -> String {
    let mut holders: Vec<&Workload> = device.live().collect();
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

/// The workload doing the most encoding or decoding, when the shaders are idle but video engines are not.
fn busiest_video_workload(device: &DeviceView) -> Option<&Workload> {
    device
        .live()
        .filter(|w| w.calm_enc() >= 10 || w.calm_dec() >= 10)
        .max_by_key(|w| w.calm_enc().max(w.calm_dec()))
}

fn video_headline(workload: &Workload, util: u8) -> String {
    let engines: Vec<String> = [
        ("encoder", workload.calm_enc()),
        ("decoder", workload.calm_dec()),
    ]
    .into_iter()
    .filter(|(_, percent)| *percent > 0)
    .map(|(engine, percent)| format!("{engine} {percent}%"))
    .collect();
    format!(
        "The GPU's shaders are idle ({util}%), but {} is busy with video work ({}).",
        workload.display_name,
        engines.join(", ")
    )
}

fn idle_headline(device: &DeviceView, util: u8) -> String {
    if let Some(video) = busiest_video_workload(device) {
        return video_headline(video, util);
    }
    let mut holders: Vec<&Workload> = device
        .live()
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
    if sample.throttle.thermal {
        let temp = sample
            .temp_c
            .map(|t| format!(" ({t}°C)"))
            .unwrap_or_default();
        return note(
            Level::Warn,
            format!("The GPU is running hot{temp} and slowing itself down to cool off."),
        );
    }
    if sample.throttle.hardware {
        return note(
            Level::Warn,
            "The GPU's hardware is slowing it down (heat, or a power brake from outside the card)."
                .into(),
        );
    }
    None
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
        .live()
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
        .live()
        .filter(|w| w.calm_enc() >= 10)
        .max_by_key(|w| w.calm_enc())?;
    note(
        Level::Info,
        format!(
            "{} is using the video encoder ({}%): that means recording or streaming video.",
            top.display_name,
            top.calm_enc()
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
        gpu_busy_percent: device.calm_util(),
        memory_used: format::bytes(sample.mem_used),
        memory_total: format::bytes(sample.mem_total),
        memory_free: format::bytes(sample.mem_free()),
        apps: device
            .live()
            .take(8)
            .map(|w| FactApp {
                name: w.display_name.clone(),
                kind: w.identity.category.label().to_string(),
                what_it_is: w.identity.description.clone(),
                compute_percent: w.calm_sm(),
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
    device.info.index.hash(&mut hasher);
    if let Some(sample) = device.sample.as_ref() {
        (device.calm_util() / 25).hash(&mut hasher);
        (format::share_pct(sample.mem_used, sample.mem_total) / 10).hash(&mut hasher);
        sample.throttle.is_limited().hash(&mut hasher);
    }
    let mut leaders: Vec<&Workload> = device
        .live()
        .filter(|w| w.activity >= Activity::Working)
        .collect();
    leaders.sort_by_key(|w| (std::cmp::Reverse(w.calm_sm()), w.key.clone()));
    for workload in leaders.iter().take(3) {
        workload.key.hash(&mut hasher);
    }
    if let Some(biggest) = device.live().max_by_key(|w| (w.mem_bytes, w.key.clone())) {
        biggest.key.hash(&mut hasher);
    }
    hasher.finish()
}

/// The facts as prose with a unit on every figure, so `quantities` can tell what each number measures.
fn facts_text(facts: &Facts) -> String {
    let mut sentences = vec![
        format!("{}.", facts.gpu),
        format!("GPU busy {}%.", facts.gpu_busy_percent),
        format!(
            "Memory used {}, total {}, free {}.",
            facts.memory_used, facts.memory_total, facts.memory_free
        ),
    ];
    sentences.extend(facts.apps.iter().map(|app| {
        format!(
            "{} ({}) {}% {} {}.",
            app.name, app.kind, app.compute_percent, app.memory, app.state
        )
    }));
    sentences.extend(facts.findings.iter().cloned());
    sentences.join(" ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Percent,
    Memory,
    Watts,
    Celsius,
    Count,
}

/// Every decimal number in `text` with the unit written after it, memory normalised to megabytes.
fn quantities(text: &str) -> Vec<(f64, Unit)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if !chars[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let start = at;
        at = skip_digits(&chars, at);
        if chars.get(at) == Some(&'.') && chars.get(at + 1).is_some_and(char::is_ascii_digit) {
            at = skip_digits(&chars, at + 1);
        }
        let literal: String = chars[start..at].iter().collect();
        if let Ok(value) = literal.parse::<f64>() {
            let (scale, unit) = unit_after(&chars, at);
            out.push((value * scale, unit));
        }
    }
    out
}

fn skip_digits(chars: &[char], mut at: usize) -> usize {
    while chars.get(at).is_some_and(char::is_ascii_digit) {
        at += 1;
    }
    at
}

/// The unit written after a number, and the factor that brings the value to that unit's base (megabytes for memory).
fn unit_after(chars: &[char], from: usize) -> (f64, Unit) {
    let mut at = from;
    while chars.get(at) == Some(&' ') {
        at += 1;
    }
    if chars.get(at) == Some(&'%') {
        return (1.0, Unit::Percent);
    }
    if chars.get(at) == Some(&'°') && chars.get(at + 1) == Some(&'C') {
        return (1.0, Unit::Celsius);
    }
    let word: String = chars[at..]
        .iter()
        .take_while(|c| c.is_alphabetic())
        .collect();
    if word == "W" {
        return (1.0, Unit::Watts);
    }
    match word.to_ascii_lowercase().as_str() {
        "gb" | "gib" => (1024.0, Unit::Memory),
        "mb" | "mib" => (1.0, Unit::Memory),
        "kb" => (1.0 / 1024.0, Unit::Memory),
        _ => (1.0, Unit::Count),
    }
}

/// Small numbers pass. Anything else must match a fact of the same unit (any fact for a bare count), or be the headroom left under a fact percentage.
fn is_allowed(value: f64, unit: Unit, known: &[(f64, Unit)]) -> bool {
    if value <= 10.0 {
        return true;
    }
    let matches_fact = known.iter().any(|&(fact, fact_unit)| {
        (unit == Unit::Count || fact_unit == unit) && (value - fact).abs() <= (fact * 0.02).max(0.6)
    });
    let headroom = unit == Unit::Percent
        && known.iter().any(|&(fact, fact_unit)| {
            fact_unit == Unit::Percent && (value - (100.0 - fact)).abs() <= 0.6
        });
    matches_fact || headroom
}

/// True when every figure in `candidate` can be traced to the facts with the same unit (allowing rounding), so a rewrite cannot invent figures.
pub fn is_grounded(candidate: &str, facts: &Facts) -> bool {
    let known = quantities(&facts_text(facts));
    quantities(candidate)
        .into_iter()
        .all(|(value, unit)| is_allowed(value, unit, &known))
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

    fn facts_with_memory_total(total_gib: f64) -> Facts {
        let id = identity("vllm", "vLLM", Category::Ai);
        let mut m = model();
        feed(&mut m, 62, 31 * GIB, vec![observed(1, id, 30 * GIB, 60)]);
        m.devices[0].sample.as_mut().unwrap().mem_total = (total_gib * GIB as f64) as u64;
        let st = story(&m.devices[0], m.interval);
        facts(&m.devices[0], &st).unwrap()
    }

    #[test]
    fn quantities_read_units_and_normalise_memory() {
        assert_eq!(
            quantities("60% 30.0 GB 2 GiB 512 MB 1024 KB 350 W 78°C 3 apps 96."),
            vec![
                (60.0, Unit::Percent),
                (30.0 * 1024.0, Unit::Memory),
                (2048.0, Unit::Memory),
                (512.0, Unit::Memory),
                (1.0, Unit::Memory),
                (350.0, Unit::Watts),
                (78.0, Unit::Celsius),
                (3.0, Unit::Count),
                (96.0, Unit::Count),
            ]
        );
    }

    #[test]
    fn grounding_needs_the_same_unit() {
        let f = facts_with_memory_total(95.6);
        assert!(!is_grounded("The GPU is at 95% load.", &f));
        assert!(!is_grounded("Apps use 93 GB.", &f));
        assert!(!is_grounded("It holds 55 GB.", &f));
        assert!(is_grounded("The GPU is 62% busy and 30 GB is held.", &f));
        assert!(is_grounded("About 38% is free.", &f));
        assert!(is_grounded("There are 3 apps.", &f));
        assert!(is_grounded("Two apps share the card.", &f));
    }

    #[test]
    fn headline_does_not_credit_a_small_share_to_the_only_app() {
        let id = identity("vllm", "vLLM", Category::Ai);
        let s = run(85, 31 * GIB, vec![observed(1, id, 30 * GIB, 10)]);
        assert_eq!(
            s.headline,
            "The GPU is busy (85%), but only 10% of that is tied to apps I can see: vLLM (10%)."
        );
    }

    #[test]
    fn unexplained_load_lists_the_top_three_claims() {
        let procs = vec![
            observed(1, identity("a", "A", Category::Ai), GIB, 8),
            observed(2, identity("b", "B", Category::Game), GIB, 7),
            observed(3, identity("c", "C", Category::Media), GIB, 6),
            observed(4, identity("d", "D", Category::Other), GIB, 5),
        ];
        let s = run(80, 4 * GIB, procs);
        assert_eq!(
            s.headline,
            "The GPU is busy (80%), but only 26% of that is tied to apps I can see: A (8%), B (7%) and C (6%)."
        );
    }

    #[test]
    fn video_work_is_not_called_idle() {
        let id = identity("sunshine", "Sunshine", Category::Media);
        let mut p = observed(1, id, GIB, 0);
        p.proc.util.enc = 28;
        p.proc.util.dec = 5;
        let s = run(2, 2 * GIB, vec![p]);
        assert_eq!(
            s.headline,
            "The GPU's shaders are idle (2%), but Sunshine is busy with video work (encoder 28%, decoder 5%)."
        );
    }

    #[test]
    fn decoder_only_video_work_names_only_the_decoder() {
        let id = identity("player", "Player", Category::Media);
        let mut p = observed(1, id, GIB, 0);
        p.proc.util.dec = 40;
        let s = run(1, 2 * GIB, vec![p]);
        assert_eq!(
            s.headline,
            "The GPU's shaders are idle (1%), but Player is busy with video work (decoder 40%)."
        );
    }

    #[test]
    fn thermal_throttling_says_the_gpu_is_running_hot() {
        let mut m = model();
        feed(&mut m, 90, 10 * GIB, vec![]);
        let sample = m.devices[0].sample.as_mut().unwrap();
        sample.throttle.thermal = true;
        sample.temp_c = Some(87);
        let notes = story(&m.devices[0], m.interval).notes;
        assert!(notes.iter().any(|n| n.level == Level::Warn
            && n.text == "The GPU is running hot (87°C) and slowing itself down to cool off."));
    }

    #[test]
    fn hardware_slowdown_without_heat_is_not_called_hot() {
        let mut m = model();
        feed(&mut m, 90, 10 * GIB, vec![]);
        m.devices[0].sample.as_mut().unwrap().throttle.hardware = true;
        let notes = story(&m.devices[0], m.interval).notes;
        assert!(notes.iter().any(|n| n.level == Level::Warn
            && n.text
                == "The GPU's hardware is slowing it down (heat, or a power brake from outside the card)."));
        assert!(notes.iter().all(|n| !n.text.contains("running hot")));
    }

    #[test]
    fn signature_tells_identical_devices_apart_by_index() {
        let build = || {
            let mut m = model();
            feed(
                &mut m,
                60,
                10 * GIB,
                vec![observed(1, identity("a", "A", Category::Ai), 8 * GIB, 55)],
            );
            m
        };
        let first = build();
        let same = build();
        let mut other = build();
        other.devices[0].info.index = 1;
        assert_eq!(
            situation_signature(&first.devices[0]),
            situation_signature(&same.devices[0])
        );
        assert_ne!(
            situation_signature(&first.devices[0]),
            situation_signature(&other.devices[0])
        );
    }
}
