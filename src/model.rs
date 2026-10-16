//! The live picture of the GPU: processes grouped into workloads, plus rolling history for the timeline.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::smooth::{Tween, blend, ema_alpha};
use crate::types::{Category, DeviceInfo, DeviceSample, GpuProcess, Identity, ProcMeta};

pub const COLOR_SLOTS: u8 = 8;
const IMPORTANCE_WINDOW: Duration = Duration::from_secs(300);
const IMPACT_MARGIN: f64 = 3.0;
const IMPACT_RATIO: f64 = 1.2;
const RANK_SMOOTHING_FACTOR: u32 = 3;
const IDLE_IMPACT: f64 = 1.0;
const MEMORY_MARGIN_RATIO: f64 = 1.1;
const MEMORY_MARGIN_BYTES: f64 = 64.0 * 1024.0 * 1024.0;
const TWEEN_SHARE_OF_INTERVAL: f64 = 0.85;

/// How calm the picture is kept. `RAW` shows every reading as it arrives (tests, one-shot reports); `calm` is what the interface uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    pub smoothing: Duration,
    pub tween: Duration,
    pub ghost: Duration,
    pub lower_dwell: Duration,
    pub settle: Duration,
}

impl Tuning {
    pub const RAW: Tuning = Tuning {
        smoothing: Duration::ZERO,
        tween: Duration::ZERO,
        ghost: Duration::ZERO,
        lower_dwell: Duration::ZERO,
        settle: Duration::ZERO,
    };

    pub const fn calm() -> Tuning {
        Tuning {
            smoothing: Duration::from_millis(1500),
            tween: Duration::from_millis(700),
            ghost: Duration::from_secs(5),
            lower_dwell: Duration::from_secs(3),
            settle: Duration::from_millis(2500),
        }
    }
}

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

/// What the screen shows for one app, gliding between samples instead of jumping.
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    pub sm: Tween,
    pub mem: Tween,
    pub enc: Tween,
    pub dec: Tween,
}

impl Shown {
    fn settled(sm: f64, mem: f64, enc: f64, dec: f64, now: Instant) -> Self {
        Self {
            sm: Tween::settled_at(sm, now),
            mem: Tween::settled_at(mem, now),
            enc: Tween::settled_at(enc, now),
            dec: Tween::settled_at(dec, now),
        }
    }

    fn retarget(&mut self, sm: f64, mem: f64, enc: f64, dec: f64, now: Instant, span: Duration) {
        self.sm.retarget(sm, now, span);
        self.mem.retarget(mem, now, span);
        self.enc.retarget(enc, now, span);
        self.dec.retarget(dec, now, span);
    }

    fn latest_end(&self) -> Instant {
        [self.sm, self.mem, self.enc, self.dec]
            .iter()
            .map(Tween::ends_at)
            .max()
            .unwrap_or_else(Instant::now)
    }
}

/// One application as the user thinks of it: all processes that share an identity key. `sm`, `enc` and `dec` are the raw latest readings; the `*_avg` values are the calm averages everything user-visible is built from. An app that has just exited lingers for a few seconds with `exited` set, shrinking to nothing.
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
    pub sm_avg: f64,
    pub enc_avg: f64,
    pub dec_avg: f64,
    pub mem_bytes: u64,
    pub activity: Activity,
    pub idle_ticks: u32,
    pub slot: Slot,
    pub exited: bool,
    pub shown: Shown,
}

impl Workload {
    /// Accounted-for memory cannot be known when the driver hides per-process figures.
    pub fn has_memory_figure(&self) -> bool {
        self.procs.iter().any(|p| p.proc.mem_bytes.is_some())
    }

    pub fn category(&self) -> Category {
        self.identity.category
    }

    pub fn calm_sm(&self) -> u8 {
        percent(self.sm_avg)
    }

    pub fn calm_enc(&self) -> u8 {
        percent(self.enc_avg)
    }

    pub fn calm_dec(&self) -> u8 {
        percent(self.dec_avg)
    }

    pub fn shown_sm(&self, now: Instant) -> f64 {
        self.shown.sm.value(now).clamp(0.0, 100.0)
    }

    pub fn shown_mem(&self, now: Instant) -> u64 {
        self.shown.mem.value(now).max(0.0).round() as u64
    }
}

fn percent(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
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
    pub seq: u64,
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
    pub power_avg: Option<f64>,
    pub pcie_tx_avg: Option<f64>,
    pub pcie_rx_avg: Option<f64>,
    next_seq: u64,
    tracks: HashMap<WorkloadId, Track>,
    order: Vec<WorkloadId>,
    util_avg: f64,
    util_tween: Tween,
    mem_used_tween: Tween,
}

/// Everything remembered about one app between samples: its smoothed readings, its debounced state and its on-screen glide.
struct Track {
    sm_avg: f64,
    enc_avg: f64,
    dec_avg: f64,
    activity: Activity,
    below_ticks: u32,
    idle_ticks: u32,
    absent_ticks: u32,
    impact: f64,
    mem: u64,
    shown: Shown,
    last: Option<Workload>,
}

impl DeviceView {
    fn new(info: DeviceInfo) -> Self {
        let now = Instant::now();
        Self {
            info,
            sample: None,
            workloads: Vec::new(),
            frames: VecDeque::new(),
            power_avg: None,
            pcie_tx_avg: None,
            pcie_rx_avg: None,
            next_seq: 0,
            tracks: HashMap::new(),
            order: Vec::new(),
            util_avg: 0.0,
            util_tween: Tween::settled_at(0.0, now),
            mem_used_tween: Tween::settled_at(0.0, now),
        }
    }

    pub fn workload(&self, key: &str) -> Option<&Workload> {
        self.workloads.iter().find(|w| w.key == key)
    }

    /// Apps that are really on the GPU, without the ones that are only fading out.
    pub fn live(&self) -> impl Iterator<Item = &Workload> {
        self.workloads.iter().filter(|w| !w.exited)
    }

    /// The overall busy figure, smoothed so it does not twitch from second to second.
    pub fn calm_util(&self) -> u8 {
        percent(self.util_avg)
    }

    pub fn shown_util(&self, now: Instant) -> f64 {
        self.util_tween.value(now).clamp(0.0, 100.0)
    }

    pub fn shown_mem_used(&self, now: Instant) -> u64 {
        self.mem_used_tween.value(now).max(0.0).round() as u64
    }

    pub fn attributed_sm(&self) -> u32 {
        self.live().map(|w| u32::from(w.calm_sm())).sum()
    }

    /// GPU busy time that no visible process claimed (hidden containers, other users, driver work).
    pub fn unattributed_sm(&self) -> u8 {
        u32::from(self.calm_util())
            .saturating_sub(self.attributed_sm())
            .min(100) as u8
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
    pub tuning: Tuning,
    registries: Vec<Registry>,
    history: Duration,
    capacity: usize,
    animating_until: Option<Instant>,
}

impl Model {
    pub fn new(infos: Vec<DeviceInfo>, interval: Duration, history: Duration) -> Self {
        let devices: Vec<DeviceView> = infos.into_iter().map(DeviceView::new).collect();
        Self {
            registries: devices.iter().map(|_| Registry::default()).collect(),
            devices,
            interval,
            ticks: 0,
            tuning: Tuning::RAW,
            history,
            capacity: frames_for(history, interval),
            animating_until: None,
        }
    }

    pub fn with_tuning(mut self, tuning: Tuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// True while some bar or number is still gliding towards its latest reading, so the interface knows to redraw quickly.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.animating_until.is_some_and(|end| now < end)
    }

    /// Changes the sampling interval. Frames recorded at the old rate would be misread at the new one, so history restarts and idle timers are rescaled.
    pub fn set_interval(&mut self, interval: Duration) {
        if interval == self.interval {
            return;
        }
        let ratio = self.interval.as_secs_f64() / interval.as_secs_f64();
        for device in &mut self.devices {
            device.frames.clear();
            for track in device.tracks.values_mut() {
                track.idle_ticks = (f64::from(track.idle_ticks) * ratio).round() as u32;
                track.below_ticks = 0;
            }
        }
        self.interval = interval;
        self.capacity = frames_for(self.history, interval);
    }

    pub fn name_of(&self, position: usize, id: WorkloadId) -> &str {
        self.registries[position]
            .entries
            .get(id as usize)
            .map_or("?", |e| e.name.as_str())
    }

    pub fn slot_of(&self, position: usize, id: WorkloadId) -> Slot {
        self.registries[position]
            .entries
            .get(id as usize)
            .map_or(Slot::Other, |e| e.slot)
    }

    /// How many frames of history are kept.
    pub fn history_frames(&self) -> usize {
        self.capacity
    }

    /// Seconds of history one frame stands for.
    pub fn seconds_per_frame(&self) -> f64 {
        self.interval.as_secs_f64()
    }

    pub fn ingest(&mut self, observation: Observation) {
        self.ingest_at(observation, Instant::now());
    }

    pub fn ingest_at(&mut self, observation: Observation, now: Instant) {
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
            self.ingest_device(position, sample, procs, now);
        }
    }

    fn tween_span(&self) -> Duration {
        self.tuning
            .tween
            .min(self.interval.mul_f64(TWEEN_SHARE_OF_INTERVAL))
    }

    fn ingest_device(
        &mut self,
        position: usize,
        sample: DeviceSample,
        procs: Vec<ObservedProc>,
        now: Instant,
    ) {
        let mut live = self.group_into_workloads(position, procs);
        self.apply_tracks(position, &mut live, now);
        self.assign_slots(position, &mut live);
        disambiguate_names(&mut live);
        self.remember_display_names(position, &live);
        let live_ids: HashSet<WorkloadId> = live.iter().map(|w| w.id).collect();

        let seq = self.devices[position].next_seq;
        self.devices[position].next_seq += 1;
        let frame = Frame {
            seq,
            gpu_util: sample.gpu_util,
            mem_used: sample.mem_used,
            entries: live
                .iter()
                .map(|w| FrameEntry {
                    id: w.id,
                    sm: w.sm,
                    mem: w.mem_bytes,
                })
                .collect(),
        };

        let mut workloads = live;
        workloads.extend(self.fading_out(position, &live_ids, now));
        self.order_workloads(position, &mut workloads);
        self.smooth_device(position, &sample, now);
        let capacity = self.capacity;
        let device = &mut self.devices[position];
        device.sample = Some(sample);
        device.workloads = workloads;
        device.frames.push_back(frame);
        while device.frames.len() > capacity {
            device.frames.pop_front();
        }
        self.note_animation_end(position);
    }

    /// Folds the new readings into each app's running state: smoothed values, the debounced activity level, how long it has been idle, and where its bar is gliding to.
    fn apply_tracks(&mut self, position: usize, workloads: &mut [Workload], now: Instant) {
        let alpha = ema_alpha(self.interval, self.tuning.smoothing);
        let rank_alpha = ema_alpha(self.interval, self.tuning.smoothing * RANK_SMOOTHING_FACTOR);
        let dwell = ticks_for(self.tuning.lower_dwell, self.interval);
        let span = self.tween_span();
        let device = &mut self.devices[position];
        for workload in workloads.iter_mut() {
            let fresh = !device.tracks.contains_key(&workload.id);
            let raw = (
                f64::from(workload.sm),
                f64::from(workload.enc),
                f64::from(workload.dec),
            );
            let track = device
                .tracks
                .entry(workload.id)
                .and_modify(|t| {
                    t.sm_avg = blend(t.sm_avg, raw.0, alpha);
                    t.enc_avg = blend(t.enc_avg, raw.1, alpha);
                    t.dec_avg = blend(t.dec_avg, raw.2, alpha);
                })
                .or_insert_with(|| Track::appearing(raw, now));
            if fresh {
                track.activity = Activity::from_util(
                    percent(track.sm_avg),
                    percent(track.enc_avg),
                    percent(track.dec_avg),
                );
            } else {
                let peak = track.sm_avg.max(track.enc_avg).max(track.dec_avg);
                let target = settle_level(track.activity, peak);
                track.activity = debounce(track.activity, target, &mut track.below_ticks, dwell);
            }
            track.idle_ticks = if track.activity.is_active() {
                0
            } else {
                track.idle_ticks + 1
            };
            track.absent_ticks = 0;
            let instant_impact = track.sm_avg + 0.5 * track.enc_avg.max(track.dec_avg);
            track.impact = if fresh {
                instant_impact
            } else {
                blend(track.impact, instant_impact, rank_alpha)
            };
            track.mem = workload.mem_bytes;
            track.shown.retarget(
                track.sm_avg,
                workload.mem_bytes as f64,
                track.enc_avg,
                track.dec_avg,
                now,
                span,
            );
            workload.sm_avg = track.sm_avg;
            workload.enc_avg = track.enc_avg;
            workload.dec_avg = track.dec_avg;
            workload.activity = track.activity;
            workload.idle_ticks = track.idle_ticks;
            workload.shown = track.shown;
            track.last = Some(workload.clone());
        }
    }

    /// Apps that were here a moment ago stay on screen, shrinking to nothing, so an exit reads as something that happened rather than a row vanishing.
    fn fading_out(
        &mut self,
        position: usize,
        live_ids: &HashSet<WorkloadId>,
        now: Instant,
    ) -> Vec<Workload> {
        let ghost_ticks = ticks_for(self.tuning.ghost, self.interval);
        let span = self.tween_span();
        let mut ghosts = Vec::new();
        self.devices[position].tracks.retain(|id, track| {
            if live_ids.contains(id) {
                return true;
            }
            track.absent_ticks += 1;
            if track.absent_ticks > ghost_ticks {
                return false;
            }
            let Some(mut ghost) = track.last.clone() else {
                return false;
            };
            if track.absent_ticks == 1 {
                track.sm_avg = 0.0;
                track.enc_avg = 0.0;
                track.dec_avg = 0.0;
                track.shown.retarget(0.0, 0.0, 0.0, 0.0, now, span);
            }
            ghost.exited = true;
            ghost.sm = 0;
            ghost.enc = 0;
            ghost.dec = 0;
            ghost.sm_avg = 0.0;
            ghost.enc_avg = 0.0;
            ghost.dec_avg = 0.0;
            ghost.mem_bytes = 0;
            ghost.activity = Activity::Idle;
            ghost.idle_ticks = track.absent_ticks;
            ghost.shown = track.shown;
            ghosts.push(ghost);
            true
        });
        ghosts
    }

    /// Keeps the order the user already saw and only moves an app when it is clearly ahead of the one above it, so near-equal apps do not trade places every second.
    fn order_workloads(&mut self, position: usize, workloads: &mut [Workload]) {
        let device = &mut self.devices[position];
        let ranks: HashMap<WorkloadId, (f64, u64)> = workloads
            .iter()
            .map(|w| {
                let rank = device
                    .tracks
                    .get(&w.id)
                    .map_or((0.0, 0), |t| (t.impact, t.mem));
                (w.id, rank)
            })
            .collect();
        device.order.retain(|id| ranks.contains_key(id));
        let known: HashSet<WorkloadId> = device.order.iter().copied().collect();
        let mut newcomers: Vec<WorkloadId> = ranks
            .keys()
            .copied()
            .filter(|id| !known.contains(id))
            .collect();
        newcomers.sort_by(|a, b| {
            let (ra, rb) = (ranks[a], ranks[b]);
            rb.0.total_cmp(&ra.0).then(rb.1.cmp(&ra.1)).then(a.cmp(b))
        });
        device.order.extend(newcomers);
        settle_order(&mut device.order, &ranks);
        let place: HashMap<WorkloadId, usize> = device
            .order
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        workloads.sort_by_key(|w| place[&w.id]);
    }

    fn smooth_device(&mut self, position: usize, sample: &DeviceSample, now: Instant) {
        let alpha = ema_alpha(self.interval, self.tuning.smoothing);
        let span = self.tween_span();
        let device = &mut self.devices[position];
        let first = device.sample.is_none();
        let raw = f64::from(sample.gpu_util);
        device.util_avg = if first {
            raw
        } else {
            blend(device.util_avg, raw, alpha)
        };
        device.power_avg = smoothed(device.power_avg, sample.power_w.map(f64::from), alpha);
        device.pcie_tx_avg = smoothed(
            device.pcie_tx_avg,
            sample.pcie_tx_kbps.map(f64::from),
            alpha,
        );
        device.pcie_rx_avg = smoothed(
            device.pcie_rx_avg,
            sample.pcie_rx_kbps.map(f64::from),
            alpha,
        );
        device.util_tween.retarget(device.util_avg, now, span);
        device
            .mem_used_tween
            .retarget(sample.mem_used as f64, now, span);
    }

    fn note_animation_end(&mut self, position: usize) {
        let device = &self.devices[position];
        let latest = device
            .tracks
            .values()
            .map(|t| t.shown.latest_end())
            .chain([device.util_tween.ends_at(), device.mem_used_tween.ends_at()])
            .max();
        self.animating_until = self.animating_until.max(latest);
    }

    fn group_into_workloads(&mut self, position: usize, procs: Vec<ObservedProc>) -> Vec<Workload> {
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
            .map(|(key, members)| self.build_workload(position, key, members))
            .collect()
    }

    fn build_workload(
        &mut self,
        position: usize,
        key: String,
        mut members: Vec<ObservedProc>,
    ) -> Workload {
        members.sort_by(|a, b| {
            b.proc
                .mem_bytes
                .unwrap_or(0)
                .cmp(&a.proc.mem_bytes.unwrap_or(0))
                .then(a.proc.pid.cmp(&b.proc.pid))
        });
        let identity = members[0].identity.clone();
        let id = self.registries[position].intern(&identity);
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
        let mem_bytes: u64 = members.iter().filter_map(|m| m.proc.mem_bytes).sum();
        Workload {
            id,
            key,
            display_name: identity.name.clone(),
            identity,
            sm,
            enc,
            dec,
            sm_avg: f64::from(sm),
            enc_avg: f64::from(enc),
            dec_avg: f64::from(dec),
            mem_bytes,
            activity: Activity::from_util(sm, enc, dec),
            idle_ticks: 0,
            slot: Slot::Other,
            exited: false,
            shown: Shown::settled(
                f64::from(sm),
                mem_bytes as f64,
                f64::from(enc),
                f64::from(dec),
                Instant::now(),
            ),
            procs: members,
        }
    }

    /// Gives each workload a stable colour: the busiest ones claim palette slots first, and a slot is only reused once its previous owner has left.
    fn assign_slots(&mut self, position: usize, workloads: &mut [Workload]) {
        let mut order: Vec<usize> = (0..workloads.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse((workloads[i].sm, workloads[i].mem_bytes)));
        let mut taken: HashSet<u8> = HashSet::new();
        for workload in workloads.iter() {
            if let Slot::Color(slot) = self.registries[position].entries[workload.id as usize].slot
            {
                taken.insert(slot);
            }
        }
        let present: HashSet<WorkloadId> = workloads.iter().map(|w| w.id).collect();
        for entry_id in 0..self.registries[position].entries.len() as WorkloadId {
            if !present.contains(&entry_id) {
                self.registries[position].entries[entry_id as usize].slot =
                    match self.registries[position].entries[entry_id as usize].slot {
                        Slot::Color(slot) if !taken.contains(&slot) => Slot::Color(slot),
                        _ => Slot::Other,
                    };
            }
        }
        for &index in &order {
            let id = workloads[index].id as usize;
            if self.registries[position].entries[id].slot == Slot::Other
                && let Some(free) = (0..COLOR_SLOTS).find(|s| !taken.contains(s))
            {
                self.registries[position].entries[id].slot = Slot::Color(free);
                taken.insert(free);
            }
        }
        self.reclaim_slots_for_important(position, workloads);
        for workload in workloads.iter_mut() {
            workload.slot = self.registries[position].entries[workload.id as usize].slot;
        }
    }

    /// The history legend and chart refer to apps by name, so it must be the disambiguated one.
    fn remember_display_names(&mut self, position: usize, workloads: &[Workload]) {
        for workload in workloads {
            self.registries[position].entries[workload.id as usize].name =
                workload.display_name.clone();
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
            if wanted == 0
                || self.registries[position].entries[candidate.id as usize].slot != Slot::Other
            {
                continue;
            }
            let victim = workloads
                .iter()
                .filter(|w| {
                    matches!(
                        self.registries[position].entries[w.id as usize].slot,
                        Slot::Color(_)
                    )
                })
                .filter(|w| score(w.id) * 3 <= wanted)
                .min_by_key(|w| (score(w.id), w.mem_bytes));
            if let Some(victim) = victim {
                let slot = self.registries[position].entries[victim.id as usize].slot;
                self.registries[position].entries[victim.id as usize].slot = Slot::Other;
                self.registries[position].entries[candidate.id as usize].slot = slot;
            }
        }
    }
}

impl Track {
    /// A newly seen app starts at zero and glides up to its first reading.
    fn appearing(raw: (f64, f64, f64), now: Instant) -> Self {
        Self {
            sm_avg: raw.0,
            enc_avg: raw.1,
            dec_avg: raw.2,
            activity: Activity::Idle,
            below_ticks: 0,
            idle_ticks: 0,
            absent_ticks: 0,
            impact: 0.0,
            mem: 0,
            shown: Shown::settled(0.0, 0.0, 0.0, 0.0, now),
            last: None,
        }
    }
}

fn ticks_for(span: Duration, interval: Duration) -> u32 {
    (span.as_secs_f64() / interval.as_secs_f64()).ceil() as u32
}

/// Where one activity level ends and the next begins, in percent of GPU time.
const LEVEL_EDGES: [f64; 3] = [0.5, 4.5, 24.5];

const LEVELS: [Activity; 4] = [
    Activity::Idle,
    Activity::Light,
    Activity::Working,
    Activity::Busy,
];

/// The level a reading belongs to, with a gap between climbing and falling: a reading has to clear an edge by a margin to move up and sink well below it to move down, so values hovering on a boundary do not flip the label.
fn settle_level(current: Activity, peak: f64) -> Activity {
    let mut level = LEVELS.iter().position(|l| *l == current).unwrap_or(0);
    while level + 1 < LEVELS.len() && peak >= LEVEL_EDGES[level] * 1.25 + 0.6 {
        level += 1;
    }
    while level > 0 && peak < LEVEL_EDGES[level - 1] * 0.8 {
        level -= 1;
    }
    LEVELS[level]
}

/// Rises at once, but only falls after the lower level has held for the dwell time, so a busy app does not flicker to idle between bursts.
fn debounce(current: Activity, target: Activity, below: &mut u32, dwell: u32) -> Activity {
    if target >= current {
        *below = 0;
        return target;
    }
    *below += 1;
    if *below >= dwell {
        *below = 0;
        target
    } else {
        current
    }
}

fn smoothed(previous: Option<f64>, new: Option<f64>, alpha: f64) -> Option<f64> {
    match (previous, new) {
        (Some(old), Some(value)) => Some(blend(old, value, alpha)),
        (None, value) => value,
        (_, None) => None,
    }
}

/// True when `a` is clearly ahead of `b`: by a margin of GPU time, or, between apps that are both idle, by a margin of memory.
fn beats(a: (f64, u64), b: (f64, u64)) -> bool {
    if a.0 > b.0 * IMPACT_RATIO + IMPACT_MARGIN {
        return true;
    }
    a.0 < IDLE_IMPACT
        && b.0 < IDLE_IMPACT
        && a.1 as f64 > b.1 as f64 * MEMORY_MARGIN_RATIO + MEMORY_MARGIN_BYTES
}

/// Bubbles apps upwards past neighbours they clearly beat; apps that are merely close keep their places.
fn settle_order(order: &mut [WorkloadId], ranks: &HashMap<WorkloadId, (f64, u64)>) {
    for _ in 0..order.len() {
        let mut moved = false;
        for index in 1..order.len() {
            if beats(ranks[&order[index]], ranks[&order[index - 1]]) {
                order.swap(index, index - 1);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

fn frames_for(history: Duration, interval: Duration) -> usize {
    (history.as_secs_f64() / interval.as_secs_f64())
        .ceil()
        .max(2.0) as usize
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

    pub fn feed_at(
        model: &mut Model,
        now: Instant,
        gpu_util: u8,
        mem_used: u64,
        procs: Vec<ObservedProc>,
    ) {
        model.ingest_at(
            Observation {
                devices: vec![sample(gpu_util, mem_used)],
                processes: procs,
            },
            now,
        );
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
        assert_eq!(m.name_of(0, ids[0]), "vLLM · aaaaaa");
        assert_eq!(m.name_of(0, ids[1]), "vLLM · bbbbbb");
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
    fn colours_are_independent_per_gpu() {
        let mut m = Model::new(
            vec![device_info(), device_info()],
            Duration::from_secs(1),
            Duration::from_secs(60),
        );
        let on_first = |i: u32| {
            observed(
                i,
                identity(&format!("a{i}"), &format!("A{i}"), Category::Ai),
                GIB,
                30,
            )
        };
        let mut on_second = observed(50, identity("b", "B", Category::Game), GIB, 30);
        on_second.proc.device = 1;
        for _ in 0..3 {
            m.ingest(Observation {
                devices: vec![sample(60, 2 * GIB), sample(30, GIB)],
                processes: vec![on_first(1), on_first(2), on_second.clone()],
            });
        }
        let slots_first: Vec<_> = m.devices[0].workloads.iter().map(|w| w.slot).collect();
        assert_ne!(slots_first[0], slots_first[1]);
        assert_eq!(
            m.slot_of(1, m.devices[1].workloads[0].id),
            m.devices[1].workloads[0].slot
        );
        assert_eq!(m.devices[1].workloads[0].slot, Slot::Color(0));
    }

    #[test]
    fn changing_the_interval_restarts_history_and_rescales_idle_time() {
        let mut m = model();
        for _ in 0..10 {
            feed(
                &mut m,
                0,
                GIB,
                vec![observed(1, identity("a", "A", Category::Ai), GIB, 0)],
            );
        }
        assert_eq!(m.devices[0].workloads[0].idle_ticks, 10);
        m.set_interval(Duration::from_millis(250));
        assert!(m.devices[0].frames.is_empty());
        feed(
            &mut m,
            0,
            GIB,
            vec![observed(1, identity("a", "A", Category::Ai), GIB, 0)],
        );
        assert_eq!(m.devices[0].workloads[0].idle_ticks, 41);
        assert_eq!(m.interval, Duration::from_millis(250));
    }

    #[test]
    fn registry_keeps_names_for_workloads_that_left() {
        let mut m = model();
        feed(&mut m, 10, GIB, vec![observed(1, vllm(), GIB, 10)]);
        let id = m.devices[0].workloads[0].id;
        feed(&mut m, 0, 0, vec![]);
        assert_eq!(m.name_of(0, id), "vLLM");
    }
}

#[cfg(test)]
mod calm_tests {
    use super::testing::*;
    use super::*;

    fn calm_model() -> Model {
        let tuning = Tuning {
            smoothing: Duration::from_secs(1),
            tween: Duration::from_millis(500),
            ghost: Duration::from_secs(3),
            lower_dwell: Duration::from_secs(2),
            settle: Duration::ZERO,
        };
        model().with_tuning(tuning)
    }

    fn app(key: &str, pid: u32, mem: u64, sm: u8) -> ObservedProc {
        observed(pid, identity(key, key, Category::Other), mem, sm)
    }

    #[test]
    fn calm_values_trail_the_raw_reading() {
        let mut m = calm_model();
        let t0 = Instant::now();
        feed_at(&mut m, t0, 10, GIB, vec![app("a", 1, GIB, 10)]);
        feed_at(
            &mut m,
            t0 + Duration::from_secs(1),
            90,
            GIB,
            vec![app("a", 1, GIB, 90)],
        );
        let w = &m.devices[0].workloads[0];
        assert_eq!(w.sm, 90);
        assert!(w.calm_sm() > 10 && w.calm_sm() < 90, "{}", w.calm_sm());
        assert!(m.devices[0].calm_util() < 90);
    }

    #[test]
    fn a_new_app_glides_in_from_nothing() {
        let mut m = calm_model();
        let t0 = Instant::now();
        feed_at(&mut m, t0, 50, 4 * GIB, vec![app("a", 1, 4 * GIB, 50)]);
        let w = &m.devices[0].workloads[0];
        assert_eq!(w.shown_sm(t0), 0.0);
        assert_eq!(w.shown_mem(t0), 0);
        let end = t0 + Duration::from_millis(500);
        assert_eq!(w.shown_sm(end), 50.0);
        assert_eq!(w.shown_mem(end), 4 * GIB);
        assert!(m.is_animating(t0 + Duration::from_millis(100)));
        assert!(!m.is_animating(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn near_equal_apps_keep_their_places_but_a_clear_leader_moves_up() {
        let mut m = calm_model();
        let t0 = Instant::now();
        let mut at = |tick: u64, a: u8, b: u8| {
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                80,
                2 * GIB,
                vec![app("a", 1, GIB, a), app("b", 2, GIB, b)],
            );
            m.devices[0]
                .workloads
                .iter()
                .map(|w| w.key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(at(0, 40, 38), vec!["a", "b"]);
        for tick in 1..10 {
            let (a, b) = if tick % 2 == 0 { (38, 41) } else { (42, 37) };
            assert_eq!(at(tick, a, b), vec!["a", "b"], "tick {tick}");
        }
        let mut last = Vec::new();
        for tick in 10..30 {
            last = at(tick, 10, 70);
        }
        assert_eq!(last, vec!["b", "a"]);
    }

    #[test]
    fn a_busy_app_stays_busy_until_it_has_been_quiet_for_the_dwell_time() {
        let mut m = calm_model();
        let t0 = Instant::now();
        let mut label = |tick: u64, sm: u8| {
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                50,
                GIB,
                vec![app("a", 1, GIB, sm)],
            );
            m.devices[0].workloads[0].activity
        };
        for tick in 0..6 {
            label(tick, 80);
        }
        assert_eq!(label(6, 0), Activity::Busy);
        for tick in 7..30 {
            label(tick, 0);
        }
        assert_eq!(label(30, 0), Activity::Idle);
    }

    #[test]
    fn labels_do_not_flip_on_a_reading_that_hovers_at_an_edge() {
        let mut m = calm_model().with_tuning(Tuning {
            smoothing: Duration::ZERO,
            lower_dwell: Duration::ZERO,
            ..Tuning::RAW
        });
        let t0 = Instant::now();
        let mut changes = 0;
        let mut previous = None;
        for tick in 0..40u64 {
            let sm = if tick % 2 == 0 { 4 } else { 5 };
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                20,
                GIB,
                vec![app("a", 1, GIB, sm)],
            );
            let activity = m.devices[0].workloads[0].activity;
            if previous.is_some_and(|p| p != activity) {
                changes += 1;
            }
            previous = Some(activity);
        }
        assert_eq!(changes, 0);
    }

    #[test]
    fn an_exited_app_lingers_shrinking_then_disappears() {
        let mut m = calm_model();
        let t0 = Instant::now();
        let both = |sm: u8| vec![app("a", 1, 4 * GIB, sm), app("b", 2, GIB, 10)];
        for tick in 0..4 {
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                60,
                5 * GIB,
                both(50),
            );
        }
        let gone_at = t0 + Duration::from_secs(4);
        feed_at(&mut m, gone_at, 10, GIB, vec![app("b", 2, GIB, 10)]);
        let device = &m.devices[0];
        assert_eq!(device.workloads.len(), 2);
        let ghost = device.workload("a").unwrap();
        assert!(ghost.exited);
        assert!(ghost.shown_sm(gone_at) > 10.0);
        assert_eq!(ghost.shown_sm(gone_at + Duration::from_secs(1)), 0.0);
        assert_eq!(ghost.shown_mem(gone_at + Duration::from_secs(1)), 0);
        assert_eq!(device.live().count(), 1);
        assert_eq!(
            device.attributed_sm(),
            u32::from(device.workload("b").unwrap().calm_sm())
        );
        assert!(device.frames.back().unwrap().entry(ghost.id).is_none());
        for tick in 5..7 {
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                10,
                GIB,
                vec![app("b", 2, GIB, 10)],
            );
        }
        assert!(m.devices[0].workload("a").is_some());
        feed_at(
            &mut m,
            t0 + Duration::from_secs(7),
            10,
            GIB,
            vec![app("b", 2, GIB, 10)],
        );
        assert!(m.devices[0].workload("a").is_none());
    }

    #[test]
    fn a_ghost_keeps_its_row_while_others_change_rank() {
        let mut m = calm_model();
        let t0 = Instant::now();
        for tick in 0..3 {
            feed_at(
                &mut m,
                t0 + Duration::from_secs(tick),
                80,
                3 * GIB,
                vec![
                    app("a", 1, GIB, 60),
                    app("b", 2, GIB, 20),
                    app("c", 3, GIB, 1),
                ],
            );
        }
        feed_at(
            &mut m,
            t0 + Duration::from_secs(3),
            25,
            2 * GIB,
            vec![app("b", 2, GIB, 20), app("c", 3, GIB, 1)],
        );
        let keys: Vec<_> = m.devices[0]
            .workloads
            .iter()
            .map(|w| w.key.clone())
            .collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_returning_app_replaces_its_ghost() {
        let mut m = calm_model();
        let t0 = Instant::now();
        feed_at(&mut m, t0, 50, GIB, vec![app("a", 1, GIB, 50)]);
        feed_at(&mut m, t0 + Duration::from_secs(1), 0, 0, vec![]);
        assert!(m.devices[0].workload("a").unwrap().exited);
        feed_at(
            &mut m,
            t0 + Duration::from_secs(2),
            50,
            GIB,
            vec![app("a", 1, GIB, 50)],
        );
        assert_eq!(m.devices[0].workloads.len(), 1);
        assert!(!m.devices[0].workload("a").unwrap().exited);
    }

    #[test]
    fn hardware_readings_are_smoothed() {
        let mut m = calm_model();
        let t0 = Instant::now();
        let mut with_power = |tick: u64, watts: f32| {
            let mut sample = sample(10, GIB);
            sample.power_w = Some(watts);
            m.ingest_at(
                Observation {
                    devices: vec![sample],
                    processes: vec![],
                },
                t0 + Duration::from_secs(tick),
            );
        };
        with_power(0, 100.0);
        with_power(1, 500.0);
        let shown = m.devices[0].power_avg.unwrap();
        assert!(shown > 100.0 && shown < 500.0, "{shown}");
    }
}
