//! Plain data shared by every layer: sampling, classification, aggregation and UI.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type Pid = u32;

/// Static facts about one GPU, read once at startup.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub uuid: String,
    pub driver_version: String,
    pub memory_total: u64,
    pub power_limit_w: Option<f32>,
    pub max_sm_clock_mhz: Option<u32>,
    pub max_mem_clock_mhz: Option<u32>,
    pub slowdown_temp_c: Option<u32>,
    pub pcie_max_gen: Option<u32>,
    pub pcie_max_width: Option<u32>,
}

/// Why the GPU is currently running slower than it could.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Throttle {
    pub idle: bool,
    pub power: bool,
    pub thermal: bool,
    pub hardware: bool,
    pub unknown: bool,
}

impl Throttle {
    /// True when something other than being idle is holding clocks down.
    pub fn is_limited(&self) -> bool {
        self.power || self.thermal || self.hardware
    }
}

/// Device-wide readings from one sampling tick. Optional fields are sensors the GPU does not expose.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DeviceSample {
    pub gpu_util: u8,
    pub mem_ctrl_util: u8,
    pub mem_used: u64,
    pub mem_total: u64,
    pub power_w: Option<f32>,
    pub power_limit_w: Option<f32>,
    pub temp_c: Option<u32>,
    pub fan_pct: Option<u32>,
    pub sm_clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub enc_util: Option<u8>,
    pub dec_util: Option<u8>,
    pub pcie_tx_kbps: Option<u32>,
    pub pcie_rx_kbps: Option<u32>,
    pub throttle: Throttle,
    pub per_process_unsupported: bool,
}

impl DeviceSample {
    pub fn mem_free(&self) -> u64 {
        self.mem_total.saturating_sub(self.mem_used)
    }
}

/// Which kind of GPU context a process holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ContextKind {
    Compute,
    Graphics,
    Both,
}

/// Per-process busy percentages over the last sampling window. Zero means "no activity seen".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EngineUtil {
    pub sm: u8,
    pub mem: u8,
    pub enc: u8,
    pub dec: u8,
}

/// One process holding a context on one GPU.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GpuProcess {
    pub pid: Pid,
    pub device: usize,
    pub kind: ContextKind,
    pub mem_bytes: Option<u64>,
    pub util: EngineUtil,
}

/// Everything one backend poll returns. `processes` spans all devices; `GpuProcess::device` is the position in `devices`.
#[derive(Clone, Debug)]
pub struct RawSample {
    pub devices: Vec<DeviceSample>,
    pub processes: Vec<GpuProcess>,
}

/// Container runtimes recognised from a process's cgroup path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Containerd,
    Lxc,
    Flatpak,
    Snap,
}

impl ContainerRuntime {
    pub fn label(self) -> &'static str {
        match self {
            Self::Docker => "Docker",
            Self::Podman => "Podman",
            Self::Containerd => "containerd",
            Self::Lxc => "LXC",
            Self::Flatpak => "Flatpak",
            Self::Snap => "Snap",
        }
    }
}

/// The container a process runs in, with the runtime's own identifier (full id, or app id for Flatpak and Snap).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Container {
    pub runtime: ContainerRuntime,
    pub id: String,
}

impl Container {
    /// Twelve-character id prefix as shown by `docker ps`, or the whole id when it is an app name.
    pub fn short_id(&self) -> &str {
        let is_hex = self.id.len() > 12 && self.id.bytes().all(|b| b.is_ascii_hexdigit());
        if is_hex { &self.id[..12] } else { &self.id }
    }
}

/// What `/proc` says about a process. Fields are best-effort: a process in another namespace or one that exited mid-read has `accessible == false`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProcMeta {
    pub pid: Pid,
    pub accessible: bool,
    pub comm: String,
    pub exe: Option<PathBuf>,
    pub cmdline: Vec<String>,
    pub uid: Option<u32>,
    pub user: Option<String>,
    pub ppid: Option<Pid>,
    pub start_ticks: Option<u64>,
    pub container: Option<Container>,
    pub unit: Option<String>,
    pub env_hints: BTreeMap<String, String>,
}

impl ProcMeta {
    /// Executable file name, falling back to the kernel's `comm` when `/proc/<pid>/exe` is unreadable.
    pub fn exe_name(&self) -> &str {
        self.exe
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.comm)
    }

    /// The full command line joined with spaces, for display and substring rules.
    pub fn command_line(&self) -> String {
        self.cmdline.join(" ")
    }

    /// A stable fingerprint of "what program is this": executable, interpreter script and `--type=` role, with volatile parts removed. Used as the cache key for learned identities.
    pub fn signature(&self) -> String {
        let mut parts = vec![self.exe_name().to_string()];
        if let Some(script) = self.interpreter_script() {
            parts.push(script);
        }
        if let Some(role) = self.cmdline.iter().find_map(|a| a.strip_prefix("--type=")) {
            parts.push(format!("type={role}"));
        }
        parts.join(" ")
    }

    /// For interpreters like `python3 /srv/ComfyUI/main.py`, the last two path components of the script.
    fn interpreter_script(&self) -> Option<String> {
        const INTERPRETERS: [&str; 7] = ["python", "node", "java", "ruby", "perl", "bash", "sh"];
        let exe = self.exe_name();
        let is_interpreter = INTERPRETERS.iter().any(|i| exe.starts_with(i));
        if !is_interpreter {
            return None;
        }
        let script = self
            .cmdline
            .iter()
            .skip(1)
            .find(|a| !a.starts_with('-') && (a.contains('/') || a.contains('.')))?;
        let components: Vec<&str> = script.split('/').filter(|c| !c.is_empty()).collect();
        let tail = components.len().saturating_sub(2);
        Some(components[tail..].join("/"))
    }
}

/// How an application is grouped and coloured. Order is the display order in summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Ai,
    Game,
    Media,
    Creative,
    Browser,
    Desktop,
    Dev,
    System,
    Other,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Self::Ai,
        Self::Game,
        Self::Media,
        Self::Creative,
        Self::Browser,
        Self::Desktop,
        Self::Dev,
        Self::System,
        Self::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ai => "AI",
            Self::Game => "Game",
            Self::Media => "Video",
            Self::Creative => "Creative",
            Self::Browser => "Browser",
            Self::Desktop => "Desktop",
            Self::Dev => "Dev",
            Self::System => "System",
            Self::Other => "Other",
        }
    }

    /// True for categories whose processes must never be offered for termination.
    pub fn is_protected(self) -> bool {
        matches!(self, Self::Desktop | Self::System)
    }

    pub fn from_label(label: &str) -> Option<Self> {
        let wanted = label.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|c| {
            c.label().to_ascii_lowercase() == wanted
                || format!("{c:?}").to_ascii_lowercase() == wanted
        })
    }
}

/// Where an identity came from, which decides how much to trust it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    Builtin,
    Catalog,
    Learned,
    Fallback,
}

/// A human-readable answer to "what is this process?".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub key: String,
    pub name: String,
    pub category: Category,
    pub description: String,
    pub source: IdentitySource,
}

impl Identity {
    /// The last-resort identity: the program's own name, uncategorised.
    pub fn fallback(meta: &ProcMeta) -> Self {
        let name = meta.exe_name().to_string();
        Self {
            key: format!("exe:{}", meta.signature()),
            description: String::new(),
            name,
            category: Category::Other,
            source: IdentitySource::Fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(exe: &str, args: &[&str]) -> ProcMeta {
        ProcMeta {
            pid: 1,
            accessible: true,
            comm: exe.to_string(),
            exe: Some(PathBuf::from(format!("/usr/bin/{exe}"))),
            cmdline: args.iter().map(|s| s.to_string()).collect(),
            ..ProcMeta::default()
        }
    }

    #[test]
    fn signature_of_plain_binary_is_its_name() {
        assert_eq!(
            meta("ffmpeg", &["ffmpeg", "-i", "a.mp4"]).signature(),
            "ffmpeg"
        );
    }

    #[test]
    fn signature_keeps_interpreter_script_tail() {
        let m = meta(
            "python3",
            &["python3", "-u", "/srv/ai/ComfyUI/main.py", "--port", "8188"],
        );
        assert_eq!(m.signature(), "python3 ComfyUI/main.py");
    }

    #[test]
    fn signature_keeps_chromium_role() {
        let m = meta(
            "vivaldi-bin",
            &[
                "vivaldi-bin",
                "--type=gpu-process",
                "--ozone-platform=wayland",
            ],
        );
        assert_eq!(m.signature(), "vivaldi-bin type=gpu-process");
    }

    #[test]
    fn exe_name_falls_back_to_comm() {
        let mut m = meta("x", &[]);
        m.exe = None;
        m.comm = "mystery".into();
        assert_eq!(m.exe_name(), "mystery");
    }

    #[test]
    fn container_short_id_truncates_hex_only() {
        let docker = Container {
            runtime: ContainerRuntime::Docker,
            id: "3f2a9c1d4b7e8a6f0123456789abcdef".into(),
        };
        assert_eq!(docker.short_id(), "3f2a9c1d4b7e");
        let flatpak = Container {
            runtime: ContainerRuntime::Flatpak,
            id: "org.mozilla.firefox".into(),
        };
        assert_eq!(flatpak.short_id(), "org.mozilla.firefox");
    }

    #[test]
    fn category_parses_labels_and_variants() {
        assert_eq!(Category::from_label("AI"), Some(Category::Ai));
        assert_eq!(Category::from_label(" game "), Some(Category::Game));
        assert_eq!(Category::from_label("video"), Some(Category::Media));
        assert_eq!(Category::from_label("media"), Some(Category::Media));
        assert_eq!(Category::from_label("nonsense"), None);
    }

    #[test]
    fn protected_categories_are_desktop_and_system() {
        let protected: Vec<_> = Category::ALL
            .into_iter()
            .filter(|c| c.is_protected())
            .collect();
        assert_eq!(protected, vec![Category::Desktop, Category::System]);
    }
}
