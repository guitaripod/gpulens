//! The live picture of the GPU: processes grouped into workloads, plus rolling history for the timeline.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crate::types::{Category, DeviceInfo, DeviceSample, GpuProcess, Identity, ProcMeta};

pub const COLOR_SLOTS: u8 = 8;
const IMPORTANCE_WINDOW: Duration = Duration::from_secs(300);

/// A GPU process together with what `/proc` and the classifier know about it.
#[derive(Clone, Debug)]
pub struct ObservedProc {
    pub proc: GpuProcess,
    pub meta: ProcMeta,
    pub identity: Identity,
}

/// One enriched poll, produced by the sampler thread and consumed by [`Model::ingest`].
#[derive(Clone, Debug)]
pub struct Observation {
    pub devices: Vec<DeviceSample>,
    pub processes: Vec<ObservedProc>,
}

/// How hard a workload is pushing the GPU right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Activity {
    Idle,
    Light,
    Working,
    Busy,
}

impl Activity {
    pub fn from_util(sm: u8, enc: u8, dec: u8) -> Self {
        let peak = sm.max(enc).max(dec);
        match peak {
            0 => Self::Idle,
            1..=4 => Self::Light,
            5..=24 => Self::Working,
            _ => Self::Busy,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Light => "light",
            Self::Working => "working",
            Self::Busy => "busy",
        }
    }

    pub fn is_active(self) -> bool {
        self != Self::Idle
    }
}

pub type WorkloadId = u32;

/// A palette position, or the shared "everything else" grey once the palette is used up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Color(u8),
    Other,
}

/// One application as the user thinks of it: all processes that share an identity key.
#[derive(Clone, Debug)]
pub struct Workload {
    pub id: WorkloadId,
    pub key: String,
    pub identity: Identity,
    pub display_name: String,
    pub procs: Vec<ObservedProc>,
    pub sm: u8,
    pub enc: u8,
    pub dec: u8,
    pub mem_bytes: u64,
    pub activity: Activity,
    pub idle_ticks: u32,
    pub slot: Slot,
}

impl Workload {
    /// Accounted-for memory cannot be known when the driver hides per-process figures.
    pub fn has_memory_figure(&self) -> bool {
        self.procs.iter().any(|p| p.proc.mem_bytes.is_some())
    }

    pub fn category(&self) -> Category {
        self.identity.category
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameEntry {
    pub id: WorkloadId,
    pub sm: u8,
    pub mem: u64,
}

/// One tick of history for one device.
#[derive(Clone, Debug)]
pub struct Frame {
    pub gpu_util: u8,
    pub mem_used: u64,
    pub entries: Vec<FrameEntry>,
}

impl Frame {
    pub fn entry(&self, id: WorkloadId) -> Option<&FrameEntry> {
        self.entries.iter().find(|e| e.id == id)
    }
}

pub struct DeviceView {
    pub info: DeviceInfo,
    pub sample: Option<DeviceSample>,
    pub workloads: Vec<Workload>,
    pub frames: VecDeque<Frame>,
    idle_ticks: HashMap<WorkloadId, u32>,
}

impl DeviceView {
    fn new(info: DeviceInfo) -> Self {
        Self {
            info,
            sample: None,
            workloads: Vec::new(),
            frames: VecDeque::new(),
            idle_ticks: HashMap::new(),
        }
    }

    pub fn workload(&self, key: &str) -> Option<&Workload> {
        self.workloads.iter().find(|w| w.key == key)
    }

    pub fn attributed_sm(&self) -> u32 {
        self.workloads.iter().map(|w| u32::from(w.sm)).sum()
    }

    /// GPU busy time that no visible process claimed (hidden containers, other users, driver work).
    pub fn unattributed_sm(&self) -> u8 {
        let busy = u32::from(self.sample.as_ref().map_or(0, |s| s.gpu_util));
        busy.saturating_sub(self.attributed_sm()).min(100) as u8
    }

    /// The last `count` frames, oldest first.
    pub fn recent_frames(&self, count: usize) -> impl Iterator<Item = &Frame> {
        let skip = self.frames.len().saturating_sub(count);
        self.frames.iter().skip(skip)
    }
}

struct RegistryEntry {
    name: String,
    category: Category,
    slot: Slot,
}

/// Remembers every workload ever seen so history frames can refer to them by number.
#[derive(Default)]
struct Registry {
    by_key: HashMap<String, WorkloadId>,
    entries: Vec<RegistryEntry>,
}

impl Registry {
    fn intern(&mut self, identity: &Identity) -> WorkloadId {
        if let Some(&id) = self.by_key.get(&identity.key) {
            let entry = &mut self.entries[id as usize];
            entry.name = identity.name.clone();
            entry.category = identity.category;
            return id;
        }
        let id = self.entries.len() as WorkloadId;
        self.entries.push(RegistryEntry {
            name: identity.name.clone(),
            category: identity.category,
            slot: Slot::Other,
        });
        self.by_key.insert(identity.key.clone(), id);
        id
    }
}

pub struct Model {
    pub devices: Vec<DeviceView>,
    pub interval: Duration,
    pub ticks: u64,
    registry: Registry,
    capacity: usize,
}

impl Model {
    pub fn new(infos: Vec<DeviceInfo>, interval: Duration, history: Duration) -> Self {
        let capacity = (history.as_secs_f64() / interval.as_secs_f64())
            .ceil()
            .max(2.0) as usize;
        Self {
            devices: infos.into_iter().map(DeviceView::new).collect(),
            interval,
            ticks: 0,
            registry: Registry::default(),
            capacity,
        }
    }

    pub fn name_of(&self, id: WorkloadId) -> &str {
        self.registry
            .entries
            .get(id as usize)
            .map_or("?", |e| e.name.as_str())
    }

    pub fn slot_of(&self, id: WorkloadId) -> Slot {
        self.registry
            .entries
            .get(id as usize)
            .map_or(Slot::Other, |e| e.slot)
    }

    /// Seconds of history one frame stands for.
    pub fn seconds_per_frame(&self) -> f64 {
        self.interval.as_secs_f64()
    }

    pub fn ingest(&mut self, observation: Observation) {
        self.ticks += 1;
        let Observation { devices, processes } = observation;
        for (position, sample) in devices.into_iter().enumerate() {
            if position >= self.devices.len() {
                break;
            }
            let procs: Vec<ObservedProc> = processes
                .iter()
                .filter(|p| p.proc.device == position)
                .cloned()
                .collect();
            self.ingest_device(position, sample, procs);
        }
    }

    fn ingest_device(&mut self, position: usize, sample: DeviceSample, procs: Vec<ObservedProc>) {
        let mut workloads = self.group_into_workloads(procs);
        self.update_idle_counters(position, &mut workloads);
        self.assign_slots(position, &mut workloads);
        disambiguate_names(&mut workloads);
        self.remember_display_names(&workloads);
        sort_by_impact(&mut workloads);

        let frame = Frame {
            gpu_util: sample.gpu_util,
            mem_used: sample.mem_used,
            entries: workloads
                .iter()
                .map(|w| FrameEntry {
                    id: w.id,
                    sm: w.sm,
                    mem: w.mem_bytes,
                })
                .collect(),
        };
        let device = &mut self.devices[position];
        device.sample = Some(sample);
        device.workloads = workloads;
        device.frames.push_back(frame);
        while device.frames.len() > self.capacity {
            device.frames.pop_front();
        }
    }

    fn group_into_workloads(&mut self, procs: Vec<ObservedProc>) -> Vec<Workload> {
        let mut groups: Vec<(String, Vec<ObservedProc>)> = Vec::new();
        for observed in procs {
            match groups
                .iter_mut()
                .find(|(key, _)| *key == observed.identity.key)
            {
                Some((_, members)) => members.push(observed),
                None => groups.push((observed.identity.key.clone(), vec![observed])),
            }
        }
        groups
            .into_iter()
            .map(|(key, members)| self.build_workload(key, members))
            .collect()
    }

    fn build_workload(&mut self, key: String, mut members: Vec<ObservedProc>) -> Workload {
        members.sort_by(|a, b| {
            b.proc
                .mem_bytes
                .unwrap_or(0)
                .cmp(&a.proc.mem_bytes.unwrap_or(0))
                .then(a.proc.pid.cmp(&b.proc.pid))
        });
        let identity = members[0].identity.clone();
        let id = self.registry.intern(&identity);
        let total = |pick: fn(&ObservedProc) -> u8| -> u8 {
            members
                .iter()
                .map(|m| u32::from(pick(m)))
                .sum::<u32>()
                .min(100) as u8
        };
        let (sm, enc, dec) = (
            total(|m| m.proc.util.sm),
            total(|m| m.proc.util.enc),
            total(|m| m.proc.util.dec),
        );
        Workload {
            id,
            key,
            display_name: identity.name.clone(),
            identity,
            sm,
            enc,
            dec,
            mem_bytes: members.iter().filter_map(|m| m.proc.mem_bytes).sum(),
            activity: Activity::from_util(sm, enc, dec),
            idle_ticks: 0,
            slot: Slot::Other,
            procs: members,
        }
    }

    fn update_idle_counters(&mut self, position: usize, workloads: &mut [Workload]) {
        let device = &mut self.devices[position];
        let present: HashSet<WorkloadId> = workloads.iter().map(|w| w.id).collect();
        device.idle_ticks.retain(|id, _| present.contains(id));
        for workload in workloads.iter_mut() {
            let counter = device.idle_ticks.entry(workload.id).or_insert(0);
            if workload.activity.is_active() {
                *counter = 0;
            } else {
                *counter += 1;
            }
            workload.idle_ticks = *counter;
        }
    }

    /// Gives each workload a stable colour: the busiest ones claim palette slots first, and a slot is only reused once its previous owner has left.
    fn assign_slots(&mut self, position: usize, workloads: &mut [Workload]) {
        let mut order: Vec<usize> = (0..workloads.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse((workloads[i].sm, workloads[i].mem_bytes)));
        let mut taken: HashSet<u8> = HashSet::new();
        for workload in workloads.iter() {
            if let Slot::Color(slot) = self.registry.entries[workload.id as usize].slot {
                taken.insert(slot);
            }
        }
        let present: HashSet<WorkloadId> = workloads.iter().map(|w| w.id).collect();
        for entry_id in 0..self.registry.entries.len() as WorkloadId {
            if !present.contains(&entry_id) {
                self.registry.entries[entry_id as usize].slot =
                    match self.registry.entries[entry_id as usize].slot {
                        Slot::Color(slot) if !taken.contains(&slot) => Slot::Color(slot),
                        _ => Slot::Other,
                    };
            }
        }
        for &index in &order {
            let id = workloads[index].id as usize;
            if self.registry.entries[id].slot == Slot::Other
                && let Some(free) = (0..COLOR_SLOTS).find(|s| !taken.contains(s))
            {
                self.registry.entries[id].slot = Slot::Color(free);
                taken.insert(free);
            }
        }
        self.reclaim_slots_for_important(position, workloads);
        for workload in workloads.iter_mut() {
            workload.slot = self.registry.entries[workload.id as usize].slot;
        }
    }

    /// The history legend and chart refer to apps by name, so it must be the disambiguated one.
    fn remember_display_names(&mut self, workloads: &[Workload]) {
        for workload in workloads {
            self.registry.entries[workload.id as usize].name = workload.display_name.clone();
        }
    }

    /// How much an app has mattered lately: its GPU time over the last few minutes, with what it does right now weighted heavily.
    fn importance_scores(
        &self,
        position: usize,
        workloads: &[Workload],
    ) -> HashMap<WorkloadId, u64> {
        let recent =
            (IMPORTANCE_WINDOW.as_secs_f64() / self.interval.as_secs_f64()).round() as usize;
        let mut scores: HashMap<WorkloadId, u64> = HashMap::new();
        for frame in self.devices[position].frames.iter().rev().take(recent) {
            for entry in &frame.entries {
                *scores.entry(entry.id).or_default() += u64::from(entry.sm);
            }
        }
        for workload in workloads {
            *scores.entry(workload.id).or_default() += 30 * u64::from(workload.sm);
        }
        scores
    }

    /// An important app should never be grey while a much less important one holds a colour, so the least important colour holder gives its slot up. The factor of three keeps colours from trading places over small differences.
    fn reclaim_slots_for_important(&mut self, position: usize, workloads: &[Workload]) {
        let scores = self.importance_scores(position, workloads);
        let score = |id: WorkloadId| scores.get(&id).copied().unwrap_or(0);
        let mut needy: Vec<&Workload> = workloads.iter().collect();
        needy.sort_by_key(|w| std::cmp::Reverse(score(w.id)));
        for candidate in needy {
            let wanted = score(candidate.id);
            if wanted == 0 || self.registry.entries[candidate.id as usize].slot != Slot::Other {
                continue;
            }
            let victim = workloads
                .iter()
                .filter(|w| matches!(self.registry.entries[w.id as usize].slot, Slot::Color(_)))
                .filter(|w| score(w.id) * 3 <= wanted)
                .min_by_key(|w| (score(w.id), w.mem_bytes));
            if let Some(victim) = victim {
                let slot = self.registry.entries[victim.id as usize].slot;
                self.registry.entries[victim.id as usize].slot = Slot::Other;
                self.registry.entries[candidate.id as usize].slot = slot;
            }
        }
    }
}

/// Two workloads with the same friendly name (two vLLM containers) get a suffix so they can be told apart.
fn disambiguate_names(workloads: &mut [Workload]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for workload in workloads.iter() {
        *counts.entry(workload.identity.name.clone()).or_default() += 1;
    }
    for workload in workloads.iter_mut() {
        if counts[&workload.identity.name] > 1 {
            workload.display_name = format!(
                "{} · {}",
                workload.identity.name,
                distinguishing_suffix(workload)
            );
        }
    }
}

fn distinguishing_suffix(workload: &Workload) -> String {
    let primary = &workload.procs[0];
    match &primary.meta.container {
        Some(container) => container.short_id().chars().take(6).collect(),
        None => format!("pid {}", primary.proc.pid),
    }
}

/// Busiest first, then biggest memory holder, then name for a stable order.
fn sort_by_impact(workloads: &mut [Workload]) {
    workloads.sort_by(|a, b| {
        b.activity
            .cmp(&a.activity)
            .then(b.sm.cmp(&a.sm))
            .then((b.enc.max(b.dec)).cmp(&a.enc.max(a.dec)))
            .then(b.mem_bytes.cmp(&a.mem_bytes))
            .then(a.display_name.cmp(&b.display_name))
    });
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use crate::types::{Container, ContainerRuntime, ContextKind, EngineUtil, IdentitySource, Pid};

    pub use crate::format::GIB;

    pub fn identity(key: &str, name: &str, category: Category) -> Identity {
        Identity {
            key: key.into(),
            name: name.into(),
            category,
            description: format!("{name} description."),
            source: IdentitySource::Builtin,
        }
    }

    pub fn observed(pid: Pid, id: Identity, mem: u64, sm: u8) -> ObservedProc {
        ObservedProc {
            proc: GpuProcess {
                pid,
                device: 0,
                kind: ContextKind::Compute,
                mem_bytes: Some(mem),
                util: EngineUtil {
                    sm,
                    mem: 0,
                    enc: 0,
                    dec: 0,
                },
            },
            meta: ProcMeta {
                pid,
                accessible: true,
                comm: id.name.clone(),
                ..ProcMeta::default()
            },
            identity: id,
        }
    }

    pub fn in_container(mut proc: ObservedProc, id: &str) -> ObservedProc {
        proc.meta.container = Some(Container {
            runtime: ContainerRuntime::Docker,
            id: id.into(),
        });
        proc
    }

    pub fn device_info() -> DeviceInfo {
        DeviceInfo {
            name: "Test GPU".into(),
            memory_total: 96 * GIB,
            ..DeviceInfo::default()
        }
    }

    pub fn sample(gpu_util: u8, mem_used: u64) -> DeviceSample {
        DeviceSample {
            gpu_util,
            mem_used,
            mem_total: 96 * GIB,
            ..DeviceSample::default()
        }
    }

    pub fn model() -> Model {
        Model::new(
            vec![device_info()],
            Duration::from_secs(1),
            Duration::from_secs(60),
        )
    }

    pub fn feed(model: &mut Model, gpu_util: u8, mem_used: u64, procs: Vec<ObservedProc>) {
        model.ingest(Observation {
            devices: vec![sample(gpu_util, mem_used)],
            processes: procs,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn vllm() -> Identity {
        identity("vllm", "vLLM", Category::Ai)
    }

    #[test]
    fn processes_with_one_key_form_one_workload() {
        let mut m = model();
        feed(
            &mut m,
            50,
            10 * GIB,
            vec![
                observed(1, vllm(), 4 * GIB, 20),
                observed(2, vllm(), 6 * GIB, 25),
            ],
        );
        let device = &m.devices[0];
        assert_eq!(device.workloads.len(), 1);
        let w = &device.workloads[0];
        assert_eq!((w.sm, w.mem_bytes), (45, 10 * GIB));
    }

    #[test]
    fn activity_levels_follow_the_thresholds() {
        assert_eq!(Activity::from_util(0, 0, 0), Activity::Idle);
        assert_eq!(Activity::from_util(1, 0, 0), Activity::Light);
        assert_eq!(Activity::from_util(5, 0, 0), Activity::Working);
        assert_eq!(Activity::from_util(0, 30, 0), Activity::Busy);
    }

    #[test]
    fn sort_puts_busy_first_then_big_idle_holders() {
        let mut m = model();
        feed(
            &mut m,
            40,
            20 * GIB,
            vec![
                observed(1, identity("a", "Small idle", Category::Other), GIB, 0),
                observed(2, identity("b", "Big idle", Category::Other), 9 * GIB, 0),
                observed(3, identity("c", "Busy", Category::Ai), GIB, 40),
            ],
        );
        let names: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .map(|w| w.display_name.as_str())
            .collect();
        assert_eq!(names, vec!["Busy", "Big idle", "Small idle"]);
    }

    #[test]
    fn same_name_different_containers_are_told_apart() {
        let mut m = model();
        let a = in_container(
            observed(1, identity("vllm@aaaaaa", "vLLM", Category::Ai), GIB, 10),
            "aaaaaa111111222222",
        );
        let b = in_container(
            observed(2, identity("vllm@bbbbbb", "vLLM", Category::Ai), GIB, 5),
            "bbbbbb111111222222",
        );
        feed(&mut m, 15, 2 * GIB, vec![a, b]);
        let names: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .map(|w| w.display_name.clone())
            .collect();
        assert_eq!(names, vec!["vLLM · aaaaaa", "vLLM · bbbbbb"]);
    }

    #[test]
    fn idle_counter_counts_up_and_resets() {
        let mut m = model();
        let idle = || vec![observed(1, vllm(), GIB, 0)];
        feed(&mut m, 0, GIB, idle());
        feed(&mut m, 0, GIB, idle());
        assert_eq!(m.devices[0].workloads[0].idle_ticks, 2);
        feed(&mut m, 30, GIB, vec![observed(1, vllm(), GIB, 30)]);
        assert_eq!(m.devices[0].workloads[0].idle_ticks, 0);
    }

    #[test]
    fn colours_stay_put_between_ticks() {
        let mut m = model();
        let procs = || {
            vec![
                observed(1, identity("a", "A", Category::Ai), GIB, 50),
                observed(2, identity("b", "B", Category::Game), GIB, 30),
            ]
        };
        feed(&mut m, 80, 2 * GIB, procs());
        let first: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .map(|w| (w.key.clone(), w.slot))
            .collect();
        for _ in 0..5 {
            feed(&mut m, 80, 2 * GIB, procs());
        }
        let later: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .map(|w| (w.key.clone(), w.slot))
            .collect();
        assert_eq!(first, later);
        assert_ne!(first[0].1, first[1].1);
    }

    #[test]
    fn beyond_the_palette_workloads_share_the_other_slot() {
        let mut m = model();
        let procs: Vec<_> = (0..12)
            .map(|i| {
                observed(
                    i + 1,
                    identity(&format!("k{i}"), &format!("App{i}"), Category::Other),
                    GIB,
                    (50 - i * 3) as u8,
                )
            })
            .collect();
        feed(&mut m, 90, 12 * GIB, procs);
        let colored = m.devices[0]
            .workloads
            .iter()
            .filter(|w| matches!(w.slot, Slot::Color(_)))
            .count();
        assert_eq!(colored, COLOR_SLOTS as usize);
    }

    #[test]
    fn a_busy_newcomer_takes_the_colour_of_a_quiet_holder() {
        let mut m = model();
        let quiet: Vec<_> = (0..8)
            .map(|i| {
                observed(
                    i + 1,
                    identity(&format!("q{i}"), &format!("Quiet{i}"), Category::Other),
                    GIB * u64::from(i + 1),
                    0,
                )
            })
            .collect();
        feed(&mut m, 0, 40 * GIB, quiet.clone());
        let mut crowd = quiet;
        crowd.push(observed(
            99,
            identity("busy", "Busy", Category::Ai),
            GIB,
            60,
        ));
        feed(&mut m, 60, 41 * GIB, crowd);
        let busy = m.devices[0].workload("busy").unwrap();
        assert!(matches!(busy.slot, Slot::Color(_)));
        let greyed: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .filter(|w| w.slot == Slot::Other)
            .map(|w| w.key.clone())
            .collect();
        assert_eq!(greyed, vec!["q0"]);
    }

    #[test]
    fn two_apps_with_one_name_stay_distinct_in_history_names() {
        let mut m = model();
        let a = in_container(
            observed(1, identity("v@a", "vLLM", Category::Ai), GIB, 10),
            "aaaaaa111111222222",
        );
        let b = in_container(
            observed(2, identity("v@b", "vLLM", Category::Ai), GIB, 5),
            "bbbbbb111111222222",
        );
        feed(&mut m, 15, 2 * GIB, vec![a, b]);
        let ids: Vec<_> = m.devices[0].workloads.iter().map(|w| w.id).collect();
        assert_eq!(m.name_of(ids[0]), "vLLM · aaaaaa");
        assert_eq!(m.name_of(ids[1]), "vLLM · bbbbbb");
    }

    #[test]
    fn a_formerly_busy_app_keeps_its_colour_over_a_tiny_one() {
        let mut m = model();
        let crowd = |busy: u8| -> Vec<_> {
            let mut v: Vec<_> = (0..8)
                .map(|i| {
                    observed(
                        i + 1,
                        identity(&format!("t{i}"), &format!("Tiny{i}"), Category::Other),
                        GIB,
                        0,
                    )
                })
                .collect();
            v.push(observed(
                99,
                identity("big", "Big", Category::Ai),
                GIB,
                busy,
            ));
            v
        };
        for _ in 0..20 {
            feed(&mut m, 60, 9 * GIB, crowd(60));
        }
        feed(&mut m, 0, 9 * GIB, crowd(0));
        let big = m.devices[0].workload("big").unwrap();
        assert!(
            matches!(big.slot, Slot::Color(_)),
            "history makes Big matter even while idle"
        );
    }

    #[test]
    fn a_leaving_workload_frees_its_colour_for_a_newcomer() {
        let mut m = model();
        let crowd: Vec<_> = (0..8)
            .map(|i| {
                observed(
                    i + 1,
                    identity(&format!("k{i}"), &format!("A{i}"), Category::Other),
                    GIB,
                    10,
                )
            })
            .collect();
        feed(&mut m, 80, 8 * GIB, crowd);
        feed(&mut m, 0, 0, vec![]);
        let newcomer = vec![observed(
            99,
            identity("new", "Newcomer", Category::Ai),
            GIB,
            40,
        )];
        feed(&mut m, 40, GIB, newcomer);
        assert!(matches!(m.devices[0].workloads[0].slot, Slot::Color(_)));
    }

    #[test]
    fn unattributed_compute_is_what_processes_do_not_claim() {
        let mut m = model();
        feed(&mut m, 70, 4 * GIB, vec![observed(1, vllm(), 3 * GIB, 50)]);
        let device = &m.devices[0];
        assert_eq!(device.unattributed_sm(), 20);
    }

    #[test]
    fn history_is_capped_to_the_window() {
        let mut m = model();
        for _ in 0..200 {
            feed(&mut m, 10, GIB, vec![observed(1, vllm(), GIB, 10)]);
        }
        assert_eq!(m.devices[0].frames.len(), 60);
        assert_eq!(m.devices[0].recent_frames(5).count(), 5);
    }

    #[test]
    fn registry_keeps_names_for_workloads_that_left() {
        let mut m = model();
        feed(&mut m, 10, GIB, vec![observed(1, vllm(), GIB, 10)]);
        let id = m.devices[0].workloads[0].id;
        feed(&mut m, 0, 0, vec![]);
        assert_eq!(m.name_of(id), "vLLM");
    }
}
