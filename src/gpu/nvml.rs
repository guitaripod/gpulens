//! NVML-backed sampling. The driver library is loaded at runtime, so the binary starts on machines without NVIDIA.

use std::collections::HashMap;

use nvml_wrapper::bitmasks::device::ThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{
    Clock, PcieUtilCounter, TemperatureSensor, TemperatureThreshold,
};
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::struct_wrappers::device::ProcessUtilizationSample;
use nvml_wrapper::{Device, Nvml};

use super::{BackendError, GpuBackend};
use crate::types::{
    ContextKind, DeviceInfo, DeviceSample, EngineUtil, GpuProcess, Pid, RawSample, Throttle,
};

pub struct NvmlBackend {
    nvml: Nvml,
    infos: Vec<DeviceInfo>,
    last_seen_us: Vec<u64>,
}

impl NvmlBackend {
    pub fn open() -> Result<Self, BackendError> {
        let nvml = Nvml::init().map_err(init_error)?;
        let count = nvml.device_count().map_err(other)?;
        if count == 0 {
            return Err(BackendError::NoDevices);
        }
        let driver = nvml.sys_driver_version().unwrap_or_default();
        let mut infos = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = nvml.device_by_index(index).map_err(other)?;
            infos.push(read_info(&device, index, &driver));
        }
        let last_seen_us = vec![0; infos.len()];
        Ok(Self {
            nvml,
            infos,
            last_seen_us,
        })
    }
}

impl GpuBackend for NvmlBackend {
    fn devices(&self) -> &[DeviceInfo] {
        &self.infos
    }

    fn sample(&mut self) -> Result<RawSample, BackendError> {
        let Self {
            nvml,
            infos,
            last_seen_us,
        } = self;
        let mut devices = Vec::with_capacity(infos.len());
        let mut processes = Vec::new();
        for (position, info) in infos.iter().enumerate() {
            let device = nvml.device_by_index(info.index).map_err(other)?;
            let mut sample = read_sample(&device, position)?;
            let (found, unsupported) =
                read_processes(&device, position, &mut last_seen_us[position])?;
            sample.per_process_unsupported = unsupported;
            devices.push(sample);
            processes.extend(found);
        }
        Ok(RawSample { devices, processes })
    }
}

/// Maps NVML initialisation failures to messages that tell the user what to do.
fn init_error(error: NvmlError) -> BackendError {
    match error {
        NvmlError::LibloadingError(_) | NvmlError::LibraryNotFound => BackendError::LibraryMissing,
        NvmlError::DriverNotLoaded => BackendError::DriverNotLoaded,
        other_error => other(other_error),
    }
}

fn other(error: NvmlError) -> BackendError {
    BackendError::Other(error.to_string())
}

/// Turns "this sensor does not exist" into `None` but keeps a dead GPU fatal.
fn optional<T>(result: Result<T, NvmlError>, position: usize) -> Result<Option<T>, BackendError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(NvmlError::GpuLost) => Err(BackendError::GpuLost(position)),
        Err(_) => Ok(None),
    }
}

fn read_info(device: &Device, index: u32, driver: &str) -> DeviceInfo {
    DeviceInfo {
        index,
        name: device.name().unwrap_or_else(|_| format!("GPU {index}")),
        uuid: device.uuid().unwrap_or_default(),
        driver_version: driver.to_string(),
        memory_total: device.memory_info().map(|m| m.total).unwrap_or(0),
        power_limit_w: device.enforced_power_limit().ok().map(milliwatts_to_watts),
        max_sm_clock_mhz: device.max_clock_info(Clock::SM).ok(),
        max_mem_clock_mhz: device.max_clock_info(Clock::Memory).ok(),
        slowdown_temp_c: device
            .temperature_threshold(TemperatureThreshold::Slowdown)
            .ok(),
        pcie_max_gen: device.max_pcie_link_gen().ok(),
        pcie_max_width: device.max_pcie_link_width().ok(),
    }
}

fn milliwatts_to_watts(milliwatts: u32) -> f32 {
    milliwatts as f32 / 1000.0
}

fn clamp_percent(value: u32) -> u8 {
    value.min(100) as u8
}

fn read_sample(device: &Device, position: usize) -> Result<DeviceSample, BackendError> {
    let rates = device
        .utilization_rates()
        .map_err(|e| lost_or_other(e, position))?;
    let memory = device
        .memory_info()
        .map_err(|e| lost_or_other(e, position))?;
    Ok(DeviceSample {
        gpu_util: clamp_percent(rates.gpu),
        mem_ctrl_util: clamp_percent(rates.memory),
        mem_used: memory.used,
        mem_total: memory.total,
        power_w: optional(device.power_usage(), position)?.map(milliwatts_to_watts),
        power_limit_w: optional(device.enforced_power_limit(), position)?.map(milliwatts_to_watts),
        temp_c: optional(device.temperature(TemperatureSensor::Gpu), position)?,
        fan_pct: optional(device.fan_speed(0), position)?,
        sm_clock_mhz: optional(device.clock_info(Clock::SM), position)?,
        mem_clock_mhz: optional(device.clock_info(Clock::Memory), position)?,
        enc_util: optional(device.encoder_utilization(), position)?
            .map(|u| clamp_percent(u.utilization)),
        dec_util: optional(device.decoder_utilization(), position)?
            .map(|u| clamp_percent(u.utilization)),
        pcie_tx_kbps: optional(device.pcie_throughput(PcieUtilCounter::Send), position)?,
        pcie_rx_kbps: optional(device.pcie_throughput(PcieUtilCounter::Receive), position)?,
        throttle: optional(device.current_throttle_reasons(), position)?
            .map(throttle_from)
            .unwrap_or_default(),
        per_process_unsupported: false,
    })
}

fn lost_or_other(error: NvmlError, position: usize) -> BackendError {
    match error {
        NvmlError::GpuLost => BackendError::GpuLost(position),
        other_error => other(other_error),
    }
}

/// Collapses NVML's clock-limiter bitmask into the four reasons a person cares about.
fn throttle_from(reasons: ThrottleReasons) -> Throttle {
    Throttle {
        idle: reasons.contains(ThrottleReasons::GPU_IDLE),
        power: reasons
            .intersects(ThrottleReasons::SW_POWER_CAP | ThrottleReasons::HW_POWER_BRAKE_SLOWDOWN),
        thermal: reasons.intersects(
            ThrottleReasons::SW_THERMAL_SLOWDOWN | ThrottleReasons::HW_THERMAL_SLOWDOWN,
        ),
        hardware: reasons.contains(ThrottleReasons::HW_SLOWDOWN),
    }
}

fn read_processes(
    device: &Device,
    position: usize,
    last_seen_us: &mut u64,
) -> Result<(Vec<GpuProcess>, bool), BackendError> {
    let compute = optional(device.running_compute_processes(), position)?.unwrap_or_default();
    let graphics = optional(device.running_graphics_processes(), position)?.unwrap_or_default();
    let (utilization, unsupported) = read_utilization(device, position, last_seen_us)?;

    let mut by_pid: HashMap<Pid, GpuProcess> = HashMap::new();
    for (list, kind) in [
        (compute, ContextKind::Compute),
        (graphics, ContextKind::Graphics),
    ] {
        for info in list {
            let memory = match info.used_gpu_memory {
                UsedGpuMemory::Used(bytes) => Some(bytes),
                UsedGpuMemory::Unavailable => None,
            };
            by_pid
                .entry(info.pid)
                .and_modify(|existing| merge_context(existing, kind, memory))
                .or_insert(GpuProcess {
                    pid: info.pid,
                    device: position,
                    kind,
                    mem_bytes: memory,
                    util: utilization.get(&info.pid).copied().unwrap_or_default(),
                });
        }
    }
    let mut processes: Vec<GpuProcess> = by_pid.into_values().collect();
    processes.sort_by_key(|p| p.pid);
    Ok((processes, unsupported))
}

/// A process listed as both compute and graphics holds one allocation pool, so the larger figure is the truth.
fn merge_context(existing: &mut GpuProcess, kind: ContextKind, memory: Option<u64>) {
    if existing.kind != kind {
        existing.kind = ContextKind::Both;
    }
    existing.mem_bytes = match (existing.mem_bytes, memory) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
}

/// Per-process busy figures, plus whether this GPU can report them at all (older cards, MIG and some virtual GPUs cannot).
fn read_utilization(
    device: &Device,
    position: usize,
    last_seen_us: &mut u64,
) -> Result<(HashMap<Pid, EngineUtil>, bool), BackendError> {
    let samples = match device.process_utilization_stats(Some(*last_seen_us)) {
        Ok(samples) => samples,
        Err(NvmlError::NotSupported) => return Ok((HashMap::new(), true)),
        Err(NvmlError::GpuLost) => return Err(BackendError::GpuLost(position)),
        Err(_) => Vec::new(),
    };
    if let Some(newest) = samples.iter().map(|s| s.timestamp).max() {
        *last_seen_us = newest;
    }
    Ok((average_per_process(&samples), false))
}

/// NVML only reports a process in periods where it did something, so each process's total is divided by the busiest process's period count to get an average over the whole window.
pub fn average_per_process(samples: &[ProcessUtilizationSample]) -> HashMap<Pid, EngineUtil> {
    let mut sums: HashMap<Pid, ([u64; 4], u64)> = HashMap::new();
    for sample in samples {
        let entry = sums.entry(sample.pid).or_default();
        entry.0[0] += u64::from(sample.sm_util);
        entry.0[1] += u64::from(sample.mem_util);
        entry.0[2] += u64::from(sample.enc_util);
        entry.0[3] += u64::from(sample.dec_util);
        entry.1 += 1;
    }
    let periods = sums.values().map(|(_, count)| *count).max().unwrap_or(1);
    let average = |total: u64| clamp_percent((total / periods).min(100) as u32);
    sums.into_iter()
        .map(|(pid, (totals, _))| {
            (
                pid,
                EngineUtil {
                    sm: average(totals[0]),
                    mem: average(totals[1]),
                    enc: average(totals[2]),
                    dec: average(totals[3]),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32, timestamp: u64, sm: u32) -> ProcessUtilizationSample {
        ProcessUtilizationSample {
            pid,
            timestamp,
            sm_util: sm,
            mem_util: 0,
            enc_util: 0,
            dec_util: 0,
        }
    }

    #[test]
    fn busy_process_keeps_its_value() {
        let samples = [sample(1, 1, 60), sample(1, 2, 60), sample(1, 3, 60)];
        assert_eq!(average_per_process(&samples)[&1].sm, 60);
    }

    #[test]
    fn bursty_process_is_scaled_by_the_busiest_process() {
        let samples = [
            sample(1, 1, 100),
            sample(1, 2, 100),
            sample(1, 3, 100),
            sample(1, 4, 100),
            sample(2, 2, 100),
        ];
        let result = average_per_process(&samples);
        assert_eq!(result[&1].sm, 100);
        assert_eq!(result[&2].sm, 25);
    }

    #[test]
    fn no_samples_gives_no_entries() {
        assert!(average_per_process(&[]).is_empty());
    }

    #[test]
    fn values_never_exceed_one_hundred() {
        let samples = [sample(1, 1, 400)];
        assert_eq!(average_per_process(&samples)[&1].sm, 100);
    }

    #[test]
    fn compute_and_graphics_merge_to_both_with_larger_memory() {
        let mut process = GpuProcess {
            pid: 7,
            device: 0,
            kind: ContextKind::Compute,
            mem_bytes: Some(100),
            util: EngineUtil::default(),
        };
        merge_context(&mut process, ContextKind::Graphics, Some(250));
        assert_eq!(process.kind, ContextKind::Both);
        assert_eq!(process.mem_bytes, Some(250));
    }

    #[test]
    fn merge_keeps_known_memory_when_other_is_unavailable() {
        let mut process = GpuProcess {
            pid: 7,
            device: 0,
            kind: ContextKind::Graphics,
            mem_bytes: None,
            util: EngineUtil::default(),
        };
        merge_context(&mut process, ContextKind::Graphics, Some(42));
        assert_eq!(process.kind, ContextKind::Graphics);
        assert_eq!(process.mem_bytes, Some(42));
    }

    #[test]
    fn throttle_reasons_collapse_to_four_flags() {
        let throttle = throttle_from(ThrottleReasons::SW_POWER_CAP | ThrottleReasons::GPU_IDLE);
        assert!(throttle.power && throttle.idle);
        assert!(!throttle.thermal && !throttle.hardware);
        assert!(throttle.is_limited());
    }
}
