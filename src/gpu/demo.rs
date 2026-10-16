//! A scripted, deterministic GPU. It backs `--demo`, the screenshot tooling and the render tests.

use std::path::PathBuf;

use super::{BackendError, GpuBackend};
use crate::types::{
    Container, ContainerRuntime, ContextKind, DeviceInfo, DeviceSample, EngineUtil, GpuProcess,
    Pid, ProcMeta, RawSample, Throttle,
};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const TOTAL_MEMORY: u64 = 95 * GIB + 600 * MIB;

/// One pretend program: what it looks like in `/proc` and how it loads the GPU over time.
struct DemoApp {
    pid: Pid,
    exe: &'static str,
    args: &'static [&'static str],
    container: Option<&'static str>,
    kind: ContextKind,
    memory: fn(u64) -> u64,
    load: fn(u64) -> EngineUtil,
}

const APPS: [DemoApp; 11] = [
    DemoApp {
        pid: 41001,
        exe: "/usr/bin/python3",
        args: &["VLLM::EngineCore"],
        container: Some("3f2a9c1d4b7e8a6f0123456789abcdef0123456789abcdef0123456789abcdef"),
        kind: ContextKind::Compute,
        memory: |_| 30 * GIB + 112 * MIB,
        load: |t| sm_only(44.0 + 16.0 * wave(t, 23.0)),
    },
    DemoApp {
        pid: 41002,
        exe: "/usr/bin/python3",
        args: &["VLLM::EngineCore"],
        container: Some("b81c0e5d92aa47f6cb10d3e2a9f4c7810e2d3c4b5a69788796a5b4c3d2e1f001"),
        kind: ContextKind::Compute,
        memory: |_| 22 * GIB + 700 * MIB,
        load: |t| {
            sm_only(if t % 160 > 90 {
                22.0 + 10.0 * wave(t, 11.0)
            } else {
                0.0
            })
        },
    },
    DemoApp {
        pid: 41003,
        exe: "/home/demo/llama.cpp/build/bin/llama-server",
        args: &[
            "llama-server",
            "-m",
            "/models/qwen3-4b-q8_0.gguf",
            "--port",
            "10021",
        ],
        container: None,
        kind: ContextKind::Compute,
        memory: |_| 5 * GIB + 350 * MIB,
        load: |t| sm_only(if t % 40 < 6 { 14.0 } else { 0.0 }),
    },
    DemoApp {
        pid: 41004,
        exe: "/home/demo/.local/share/Steam/steamapps/common/Counter-Strike Global Offensive/game/bin/linuxsteamrt64/cs2",
        args: &["cs2", "-steam"],
        container: None,
        kind: ContextKind::Both,
        memory: |_| 8 * GIB + 360 * MIB,
        load: |t| {
            sm_only(if t % 200 < 120 {
                18.0 + 8.0 * wave(t, 5.0)
            } else {
                0.0
            })
        },
    },
    DemoApp {
        pid: 41005,
        exe: "/usr/bin/kwin_wayland",
        args: &["kwin_wayland", "--wayland-fd", "7"],
        container: None,
        kind: ContextKind::Both,
        memory: |_| 380 * MIB,
        load: |t| sm_only(6.0 + 3.0 * wave(t, 7.0)),
    },
    DemoApp {
        pid: 41006,
        exe: "/opt/vivaldi/vivaldi-bin",
        args: &[
            "/opt/vivaldi/vivaldi-bin",
            "--type=gpu-process",
            "--ozone-platform=wayland",
        ],
        container: None,
        kind: ContextKind::Both,
        memory: |_| GIB + 790 * MIB,
        load: |t| sm_only(if t % 60 < 15 { 4.0 } else { 0.0 }),
    },
    DemoApp {
        pid: 41007,
        exe: "/usr/bin/sunshine",
        args: &["sunshine"],
        container: None,
        kind: ContextKind::Both,
        memory: |_| 564 * MIB,
        load: |t| EngineUtil {
            sm: 1,
            mem: 0,
            enc: if t % 240 > 140 { 28 } else { 0 },
            dec: 0,
        },
    },
    DemoApp {
        pid: 41008,
        exe: "/usr/bin/python3",
        args: &["python3", "/srv/ai/ComfyUI/main.py", "--port", "8188"],
        container: None,
        kind: ContextKind::Compute,
        memory: |t| {
            if t % 300 > 200 {
                9 * GIB
            } else {
                2 * GIB + 200 * MIB
            }
        },
        load: |t| {
            sm_only(if t % 300 > 200 {
                31.0 + 6.0 * wave(t, 3.0)
            } else {
                0.0
            })
        },
    },
    DemoApp {
        pid: 41009,
        exe: "/opt/1Password/1password",
        args: &["/opt/1Password/1password", "--type=gpu-process"],
        container: None,
        kind: ContextKind::Both,
        memory: |_| 38 * MIB,
        load: |_| EngineUtil::default(),
    },
    DemoApp {
        pid: 41010,
        exe: "/home/demo/.local/bin/flaccy",
        args: &["flaccy"],
        container: None,
        kind: ContextKind::Both,
        memory: |_| 97 * MIB,
        load: |_| EngineUtil::default(),
    },
    DemoApp {
        pid: 41011,
        exe: "/usr/bin/plasmashell",
        args: &["plasmashell"],
        container: None,
        kind: ContextKind::Graphics,
        memory: |_| 210 * MIB,
        load: |t| sm_only(1.0 + 2.0 * wave(t, 13.0)),
    },
];

/// Smooth, repeatable oscillation in 0.0..=1.0 with the given period in ticks.
fn wave(tick: u64, period: f64) -> f64 {
    ((tick as f64 / period).sin() + 1.0) / 2.0
}

fn sm_only(sm: f64) -> EngineUtil {
    EngineUtil {
        sm: sm.round().clamp(0.0, 100.0) as u8,
        mem: (sm * 0.4).round().clamp(0.0, 100.0) as u8,
        enc: 0,
        dec: 0,
    }
}

pub struct DemoBackend {
    infos: Vec<DeviceInfo>,
    tick: u64,
}

impl DemoBackend {
    pub fn new() -> Self {
        Self::starting_at(0)
    }

    /// Starts the script at a given second, so a test or screenshot can land on a specific moment.
    pub fn starting_at(tick: u64) -> Self {
        Self {
            infos: vec![DeviceInfo {
                index: 0,
                name: "NVIDIA RTX PRO 6000 Blackwell (demo)".into(),
                uuid: "GPU-demo-0000".into(),
                driver_version: "615.71.09".into(),
                memory_total: TOTAL_MEMORY,
                power_limit_w: Some(600.0),
                max_sm_clock_mhz: Some(3090),
                max_mem_clock_mhz: Some(14001),
                slowdown_temp_c: Some(92),
                pcie_max_gen: Some(5),
                pcie_max_width: Some(16),
            }],
            tick,
        }
    }

    fn app_for(pid: Pid) -> Option<&'static DemoApp> {
        APPS.iter().find(|a| a.pid == pid)
    }
}

impl Default for DemoBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuBackend for DemoBackend {
    fn devices(&self) -> &[DeviceInfo] {
        &self.infos
    }

    fn sample(&mut self) -> Result<RawSample, BackendError> {
        let tick = self.tick;
        self.tick += 1;
        let processes: Vec<GpuProcess> = APPS
            .iter()
            .map(|app| GpuProcess {
                pid: app.pid,
                device: 0,
                kind: app.kind,
                mem_bytes: Some((app.memory)(tick)),
                util: (app.load)(tick),
            })
            .collect();
        Ok(RawSample {
            devices: vec![device_sample(tick, &processes)],
            processes,
        })
    }

    fn process_meta(&mut self, pid: Pid) -> Option<ProcMeta> {
        let app = Self::app_for(pid)?;
        Some(ProcMeta {
            pid,
            accessible: true,
            comm: app
                .exe
                .rsplit('/')
                .next()
                .unwrap_or(app.exe)
                .chars()
                .take(15)
                .collect(),
            exe: Some(PathBuf::from(app.exe)),
            cmdline: app.args.iter().map(|s| s.to_string()).collect(),
            uid: Some(1000),
            user: Some("demo".into()),
            ppid: Some(1),
            start_ticks: Some(u64::from(pid)),
            container: app.container.map(|id| Container {
                runtime: ContainerRuntime::Docker,
                id: id.to_string(),
            }),
            ..ProcMeta::default()
        })
    }
}

fn device_sample(tick: u64, processes: &[GpuProcess]) -> DeviceSample {
    let attributed: u32 = processes.iter().map(|p| u32::from(p.util.sm)).sum();
    let hidden = (3.0 * wave(tick, 9.0)).round() as u32;
    let gpu_util = (attributed + hidden).min(100) as u8;
    let used: u64 = processes.iter().filter_map(|p| p.mem_bytes).sum::<u64>() + 1200 * MIB;
    let load = f32::from(gpu_util);
    let enc = processes.iter().map(|p| p.util.enc).max().unwrap_or(0);
    DeviceSample {
        gpu_util,
        mem_ctrl_util: (load * 0.45).round() as u8,
        mem_used: used.min(TOTAL_MEMORY),
        mem_total: TOTAL_MEMORY,
        power_w: Some(62.0 + load * 4.6),
        power_limit_w: Some(600.0),
        temp_c: Some(36 + (load * 0.42) as u32),
        fan_pct: Some(30 + (load * 0.4) as u32),
        sm_clock_mhz: Some(if gpu_util < 3 {
            435
        } else {
            1800 + (load * 9.0) as u32
        }),
        mem_clock_mhz: Some(if gpu_util < 3 { 810 } else { 13365 }),
        enc_util: Some(enc),
        dec_util: Some(0),
        pcie_tx_kbps: Some(40_000 + (load * 9_000.0) as u32),
        pcie_rx_kbps: Some(65_000 + (load * 14_000.0) as u32),
        throttle: Throttle {
            idle: gpu_util < 3,
            power: tick % 200 > 170,
            ..Throttle::default()
        },
        per_process_unsupported: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_deterministic() {
        let a = DemoBackend::starting_at(50).sample().unwrap();
        let b = DemoBackend::starting_at(50).sample().unwrap();
        assert_eq!(a.devices, b.devices);
        assert_eq!(a.processes, b.processes);
    }

    #[test]
    fn every_process_has_meta() {
        let mut backend = DemoBackend::new();
        let sample = backend.sample().unwrap();
        for process in &sample.processes {
            let meta = backend.process_meta(process.pid).unwrap();
            assert!(meta.accessible);
            assert!(!meta.cmdline.is_empty());
        }
    }

    #[test]
    fn memory_never_exceeds_the_card() {
        let mut backend = DemoBackend::new();
        for _ in 0..600 {
            let sample = backend.sample().unwrap();
            let device = &sample.devices[0];
            assert!(device.mem_used <= device.mem_total);
            assert!(device.gpu_util <= 100);
        }
    }

    #[test]
    fn scenario_has_busy_and_idle_moments() {
        let mut backend = DemoBackend::new();
        let utils: Vec<u8> = (0..400)
            .map(|_| backend.sample().unwrap().devices[0].gpu_util)
            .collect();
        assert!(utils.iter().any(|u| *u >= 70));
        assert!(utils.iter().any(|u| *u <= 55));
    }
}
