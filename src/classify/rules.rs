//! Built-in rules that name well-known GPU processes. The first matching rule wins.

use super::steam::SteamLibrary;
use crate::types::{Category, Identity, IdentitySource, ProcMeta};

const DESKTOP_APP: &str = "Desktop app that draws its window with the GPU.";
const DESKTOP_SHELL: &str = "The desktop shell: panels, widgets and the app launcher.";
const COMPOSITOR: &str = "Draws your desktop windows and effects.";
const SERVER: &str =
    "Serves a large language model; memory stays reserved while it waits for requests.";
const IMAGE_SERVER: &str = "Image generation server; keeps models loaded between runs.";
const SPEECH: &str = "Speech-to-text model that runs on the GPU.";
const WINE: &str = "A Windows program running through Wine.";
const OPT_PROGRAM: &str = "Program installed under /opt.";
const RECORDER: &str = "Records your screen to a video file.";
const LAUNCHER: &str = "Game launcher that runs Windows games through Wine.";
const WINDOWS_MARKERS: [&str; 3] = ["wine", "proton", ".exe"];
const EXE_SUFFIX: &str = ".exe";

#[derive(Clone, Copy, Debug)]
enum Test {
    Exe(&'static str),
    Comm(&'static str),
    Arg(&'static str),
    Path(&'static str),
    All(&'static [Test]),
}

#[derive(Debug)]
struct Rule {
    tests: &'static [Test],
    key: &'static str,
    name: &'static str,
    category: Category,
    description: &'static str,
}

impl Rule {
    fn matches(&self, probe: &Probe) -> bool {
        self.tests.iter().any(|test| probe.matches(test))
    }

    fn identity(&self) -> Identity {
        builtin(self.key, self.name, self.category, self.description)
    }
}

const fn rule(
    tests: &'static [Test],
    key: &'static str,
    name: &'static str,
    category: Category,
    description: &'static str,
) -> Rule {
    Rule {
        tests,
        key,
        name,
        category,
        description,
    }
}

struct ChromiumApp {
    stems: &'static [&'static str],
    name: &'static str,
    category: Category,
}

const fn chromium_app(
    stems: &'static [&'static str],
    name: &'static str,
    category: Category,
) -> ChromiumApp {
    ChromiumApp {
        stems,
        name,
        category,
    }
}

/// Executable name, family key and display name for each browser binary.
const BROWSERS: &[(&str, &str, &str)] = &[
    ("vivaldi", "vivaldi", "Vivaldi"),
    ("vivaldi-bin", "vivaldi", "Vivaldi"),
    ("chrome", "chrome", "Google Chrome"),
    ("google-chrome", "chrome", "Google Chrome"),
    ("chromium", "chromium", "Chromium"),
    ("chromium-browser", "chromium", "Chromium"),
    ("brave", "brave", "Brave"),
    ("brave-browser", "brave", "Brave"),
    ("microsoft-edge", "edge", "Microsoft Edge"),
    ("msedge", "edge", "Microsoft Edge"),
    ("opera", "opera", "Opera"),
    ("firefox", "firefox", "Firefox"),
    ("firefox-bin", "firefox", "Firefox"),
    ("firefox-esr", "firefox", "Firefox"),
    ("librewolf", "librewolf", "LibreWolf"),
    ("zen", "zen", "Zen Browser"),
    ("zen-bin", "zen", "Zen Browser"),
];

const CHROMIUM_APPS: &[ChromiumApp] = &[
    chromium_app(&["1password"], "1Password", Category::Other),
    chromium_app(&["discord"], "Discord", Category::Other),
    chromium_app(&["slack"], "Slack", Category::Other),
    chromium_app(
        &["code", "visual studio code"],
        "Visual Studio Code",
        Category::Dev,
    ),
    chromium_app(&["codium", "vscodium"], "VSCodium", Category::Dev),
    chromium_app(&["cursor"], "Cursor", Category::Dev),
    chromium_app(&["zed", "zed-editor"], "Zed", Category::Dev),
    chromium_app(&["obsidian"], "Obsidian", Category::Other),
    chromium_app(&["spotify"], "Spotify", Category::Other),
    chromium_app(&["signal-desktop", "signal"], "Signal", Category::Other),
    chromium_app(&["element-desktop", "element"], "Element", Category::Other),
    chromium_app(
        &["teams", "teams-for-linux"],
        "Microsoft Teams",
        Category::Other,
    ),
    chromium_app(&["zoom"], "Zoom", Category::Other),
    chromium_app(&["heroic"], "Heroic Games Launcher", Category::Game),
];

const STEAM_RULES: &[Rule] = &[
    rule(
        &[Test::Exe("steam"), Test::Exe("steamwebhelper")],
        "steam",
        "Steam",
        Category::Other,
        "The Steam launcher and its built-in browser.",
    ),
    rule(
        &[
            Test::Exe("reaper"),
            Test::Exe("srt-bwrap"),
            Test::Exe("pressure-vessel-wrap"),
            Test::Exe("pressure-vessel-adverb"),
            Test::Comm("pressure-vessel"),
        ],
        "steam-helper",
        "Steam helper",
        Category::System,
        "Steam game launcher helper that sets up a game's runtime.",
    ),
    rule(
        &[Test::Exe("gamescope"), Test::Comm("gamescope")],
        "gamescope",
        "Gamescope",
        Category::Game,
        "Game-focused window compositor that wraps a game.",
    ),
];

const DESKTOP_RULES: &[Rule] = &[
    rule(
        &[
            Test::Exe("kwin_wayland"),
            Test::Exe("kwin_wayland_wrapper"),
            Test::Exe("kwin_x11"),
            Test::Comm("kwin_"),
        ],
        "desktop:kwin",
        "KWin compositor",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("xorg"), Test::Exe("x")],
        "desktop:xorg",
        "X.Org server",
        Category::Desktop,
        "The X11 display server that draws your windows.",
    ),
    rule(
        &[Test::Exe("xwayland")],
        "desktop:xwayland",
        "Xwayland",
        Category::Desktop,
        "Compatibility layer that lets old X11 apps run on Wayland.",
    ),
    rule(
        &[Test::Exe("gnome-shell")],
        "desktop:gnome-shell",
        "GNOME Shell",
        Category::Desktop,
        DESKTOP_SHELL,
    ),
    rule(
        &[Test::Exe("mutter")],
        "desktop:mutter",
        "Mutter",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("sway")],
        "desktop:sway",
        "Sway",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("hyprland")],
        "desktop:hyprland",
        "Hyprland",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("weston")],
        "desktop:weston",
        "Weston",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("labwc")],
        "desktop:labwc",
        "labwc",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("wayfire")],
        "desktop:wayfire",
        "Wayfire",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("picom")],
        "desktop:picom",
        "picom",
        Category::Desktop,
        "X11 compositor that draws window effects.",
    ),
    rule(
        &[Test::Exe("plasmashell")],
        "desktop:plasmashell",
        "Plasma shell",
        Category::Desktop,
        DESKTOP_SHELL,
    ),
    rule(
        &[Test::Exe("kded"), Test::Exe("kded5"), Test::Exe("kded6")],
        "desktop:kded",
        "KDE daemon",
        Category::Desktop,
        "Background services that keep KDE Plasma running.",
    ),
    rule(
        &[Test::Exe("krunner")],
        "desktop:krunner",
        "KRunner",
        Category::Desktop,
        "Plasma search and command runner.",
    ),
    rule(
        &[Test::Exe("cosmic-comp")],
        "desktop:cosmic-comp",
        "COSMIC compositor",
        Category::Desktop,
        COMPOSITOR,
    ),
    rule(
        &[Test::Exe("niri")],
        "desktop:niri",
        "niri",
        Category::Desktop,
        COMPOSITOR,
    ),
];

const AI_RULES: &[Rule] = &[
    rule(
        &[Test::Exe("llama-server"), Test::Comm("llama-server")],
        "llama-server",
        "llama.cpp server",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Exe("llama-cli")],
        "llama-cli",
        "llama.cpp CLI",
        Category::Ai,
        "Runs a large language model for one chat or prompt.",
    ),
    rule(
        &[Test::Exe("ollama")],
        "ollama",
        "Ollama",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Comm("vllm"), Test::Arg("vllm")],
        "vllm",
        "vLLM",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Comm("sglang"), Test::Arg("sglang")],
        "sglang",
        "SGLang",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[
            Test::Exe("tritonserver"),
            Test::Exe("triton_python_backend_stub"),
        ],
        "triton",
        "Triton Inference Server",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[
            Test::Exe("text-generation-launcher"),
            Test::Exe("text-generation-router"),
        ],
        "text-generation-inference",
        "Text Generation Inference",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Exe("koboldcpp"), Test::Arg("koboldcpp")],
        "koboldcpp",
        "KoboldCpp",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[
            Test::Exe("lm-studio"),
            Test::Exe("lmstudio"),
            Test::Path("lm studio"),
            Test::Path("lm-studio"),
        ],
        "lm-studio",
        "LM Studio",
        Category::Ai,
        "Desktop app that runs and chats with local language models.",
    ),
    rule(
        &[Test::Exe("jan")],
        "jan",
        "Jan",
        Category::Ai,
        "Desktop app that runs local language models.",
    ),
    rule(
        &[Test::Exe("llamafile"), Test::Arg("llamafile")],
        "llamafile",
        "llamafile",
        Category::Ai,
        "Single-file program that bundles a model and its server.",
    ),
    rule(
        &[
            Test::Exe("whisper"),
            Test::Exe("whisper-cli"),
            Test::Exe("whisper-server"),
        ],
        "whisper",
        "whisper.cpp",
        Category::Ai,
        SPEECH,
    ),
    rule(
        &[
            Test::Exe("faster-whisper"),
            Test::Arg("faster-whisper"),
            Test::Arg("faster_whisper"),
        ],
        "faster-whisper",
        "faster-whisper",
        Category::Ai,
        SPEECH,
    ),
    rule(
        &[
            Test::Arg("comfyui"),
            Test::All(&[Test::Arg("comfy"), Test::Arg("main.py")]),
        ],
        "comfyui",
        "ComfyUI",
        Category::Ai,
        "Node-based image and video generation server; keeps models loaded between runs.",
    ),
    rule(
        &[Test::Arg("stable-diffusion-webui")],
        "stable-diffusion-webui",
        "Stable Diffusion WebUI",
        Category::Ai,
        IMAGE_SERVER,
    ),
    rule(
        &[Test::Arg("invokeai")],
        "invokeai",
        "InvokeAI",
        Category::Ai,
        IMAGE_SERVER,
    ),
    rule(
        &[Test::Arg("fooocus")],
        "fooocus",
        "Fooocus",
        Category::Ai,
        IMAGE_SERVER,
    ),
    rule(
        &[
            Test::Exe("torchrun"),
            Test::Arg("torchrun"),
            Test::Arg("torch.distributed.run"),
            Test::All(&[Test::Arg("accelerate"), Test::Arg(" launch")]),
        ],
        "pytorch-run",
        "PyTorch run",
        Category::Ai,
        "Model training or distributed run (PyTorch).",
    ),
    rule(
        &[
            Test::Arg("trtllm-serve"),
            Test::Arg("tensorrt_llm"),
            Test::Exe("trtllm-build"),
        ],
        "tensorrt-llm",
        "TensorRT-LLM",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Exe("trtexec")],
        "tensorrt",
        "TensorRT",
        Category::Ai,
        "Builds or benchmarks a TensorRT engine on the GPU.",
    ),
    rule(
        &[Test::Exe("mlc_llm"), Test::Arg("mlc_llm")],
        "mlc-llm",
        "MLC LLM",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Arg("exllama")],
        "exllama",
        "ExLlama",
        Category::Ai,
        SERVER,
    ),
    rule(
        &[Test::Arg("tabbyapi")],
        "tabbyapi",
        "TabbyAPI",
        Category::Ai,
        SERVER,
    ),
];

const MEDIA_RULES: &[Rule] = &[
    rule(
        &[Test::Exe("ffmpeg")],
        "ffmpeg",
        "FFmpeg",
        Category::Media,
        "Encodes or converts video and audio files.",
    ),
    rule(
        &[
            Test::Exe("obs"),
            Test::Exe("obs-studio"),
            Test::Exe("obs-ffmpeg-mux"),
        ],
        "obs-studio",
        "OBS Studio",
        Category::Media,
        "Records and streams your screen and cameras.",
    ),
    rule(
        &[Test::Exe("sunshine")],
        "sunshine",
        "Sunshine",
        Category::Media,
        "Game streaming server; encodes your screen for Moonlight.",
    ),
    rule(
        &[Test::Exe("mpv")],
        "mpv",
        "mpv",
        Category::Media,
        "Video player.",
    ),
    rule(
        &[Test::Exe("vlc")],
        "vlc",
        "VLC",
        Category::Media,
        "Video player.",
    ),
    rule(
        &[Test::Exe("celluloid")],
        "celluloid",
        "Celluloid",
        Category::Media,
        "Video player.",
    ),
    rule(
        &[
            Test::Exe("ghb"),
            Test::Exe("handbrakecli"),
            Test::Exe("handbrake"),
        ],
        "handbrake",
        "HandBrake",
        Category::Media,
        "Converts video files to smaller, compatible formats.",
    ),
    rule(
        &[Test::Exe("kdenlive")],
        "kdenlive",
        "Kdenlive",
        Category::Media,
        "Video editor.",
    ),
    rule(
        &[Test::Exe("shotcut")],
        "shotcut",
        "Shotcut",
        Category::Media,
        "Video editor.",
    ),
    rule(
        &[Test::Exe("resolve"), Test::Path("davinci resolve")],
        "davinci-resolve",
        "DaVinci Resolve",
        Category::Media,
        "Video editing and colour grading app.",
    ),
    rule(
        &[Test::Exe("gst-launch-1.0"), Test::Exe("gst-launch")],
        "gstreamer",
        "GStreamer",
        Category::Media,
        "Media pipeline that decodes, processes or encodes video.",
    ),
    rule(
        &[Test::Exe("plex transcoder"), Test::Comm("plex transcoder")],
        "plex-transcoder",
        "Plex Transcoder",
        Category::Media,
        "Transcodes media for Plex Media Server.",
    ),
    rule(
        &[Test::Exe("jellyfin-ffmpeg")],
        "jellyfin-ffmpeg",
        "Jellyfin FFmpeg",
        Category::Media,
        "Transcodes media for the Jellyfin server.",
    ),
    rule(
        &[Test::Exe("wf-recorder")],
        "wf-recorder",
        "wf-recorder",
        Category::Media,
        "Records your Wayland screen to a video file.",
    ),
    rule(
        &[Test::Exe("kooha")],
        "kooha",
        "Kooha",
        Category::Media,
        RECORDER,
    ),
    rule(
        &[Test::Exe("gpu-screen-recorder")],
        "gpu-screen-recorder",
        "GPU Screen Recorder",
        Category::Media,
        "Records your screen using the GPU encoder.",
    ),
    rule(
        &[Test::Exe("simplescreenrecorder")],
        "simplescreenrecorder",
        "SimpleScreenRecorder",
        Category::Media,
        RECORDER,
    ),
];

const CREATIVE_RULES: &[Rule] = &[
    rule(
        &[Test::Exe("blender"), Test::Comm("blender")],
        "blender",
        "Blender",
        Category::Creative,
        "3D modelling, animation and rendering app.",
    ),
    rule(
        &[Test::Exe("krita")],
        "krita",
        "Krita",
        Category::Creative,
        "Digital painting app.",
    ),
    rule(
        &[Test::Exe("gimp"), Test::Comm("gimp")],
        "gimp",
        "GIMP",
        Category::Creative,
        "Image editor.",
    ),
    rule(
        &[Test::Exe("inkscape")],
        "inkscape",
        "Inkscape",
        Category::Creative,
        "Vector graphics editor.",
    ),
    rule(
        &[Test::Exe("godot"), Test::Comm("godot")],
        "godot",
        "Godot",
        Category::Creative,
        "Game engine editor.",
    ),
    rule(
        &[Test::Exe("unity"), Test::Path("/unity/editor/")],
        "unity",
        "Unity",
        Category::Creative,
        "Game engine editor.",
    ),
    rule(
        &[Test::Exe("unrealeditor"), Test::Comm("unrealeditor")],
        "unreal-editor",
        "Unreal Editor",
        Category::Creative,
        "Game engine editor.",
    ),
    rule(
        &[Test::Exe("darktable"), Test::Exe("darktable-bin")],
        "darktable",
        "darktable",
        Category::Creative,
        "Photo editor for raw files.",
    ),
    rule(
        &[Test::Exe("rawtherapee")],
        "rawtherapee",
        "RawTherapee",
        Category::Creative,
        "Photo editor for raw files.",
    ),
    rule(
        &[Test::Exe("freecad"), Test::Comm("freecad")],
        "freecad",
        "FreeCAD",
        Category::Creative,
        "Parametric 3D CAD modeller.",
    ),
    rule(
        &[
            Test::Exe("houdini"),
            Test::Exe("houdinifx"),
            Test::Exe("hbatch"),
            Test::Comm("houdini"),
        ],
        "houdini",
        "Houdini",
        Category::Creative,
        "3D effects and simulation app.",
    ),
    rule(
        &[Test::Exe("maya"), Test::Exe("maya.bin")],
        "maya",
        "Maya",
        Category::Creative,
        "3D modelling and animation app.",
    ),
    rule(
        &[
            Test::Exe("adobe substance 3d painter"),
            Test::Exe("substance painter"),
            Test::Path("substance 3d painter"),
        ],
        "substance-painter",
        "Substance Painter",
        Category::Creative,
        "Texture painting app for 3D models.",
    ),
];

const DEV_SYSTEM_RULES: &[Rule] = &[
    rule(
        &[
            Test::Exe("nsys"),
            Test::Exe("nsys-ui"),
            Test::Exe("nsight-sys"),
            Test::Comm("nsight-sys"),
        ],
        "nsight-systems",
        "Nsight Systems",
        Category::Dev,
        "Profiler that records GPU and CPU timelines.",
    ),
    rule(
        &[
            Test::Exe("ncu"),
            Test::Exe("ncu-ui"),
            Test::Exe("nsight-compute"),
            Test::Comm("nsight-compute"),
        ],
        "nsight-compute",
        "Nsight Compute",
        Category::Dev,
        "Profiler that inspects individual CUDA kernels.",
    ),
    rule(
        &[Test::Exe("cuda-gdb")],
        "cuda-gdb",
        "cuda-gdb",
        Category::Dev,
        "Debugger for CUDA GPU code.",
    ),
    rule(
        &[Test::Exe("compute-sanitizer")],
        "compute-sanitizer",
        "Compute Sanitizer",
        Category::Dev,
        "Checks CUDA programs for memory errors and races.",
    ),
    rule(
        &[Test::Exe("nvtop")],
        "nvtop",
        "nvtop",
        Category::System,
        "Terminal GPU monitor that shows per-process usage.",
    ),
    rule(
        &[Test::Exe("gpulens")],
        "gpulens",
        "gpulens",
        Category::System,
        "This GPU monitor, showing what is using your graphics card.",
    ),
    rule(
        &[Test::Exe("nvidia-smi")],
        "nvidia-smi",
        "nvidia-smi",
        Category::System,
        "NVIDIA's command-line GPU status tool.",
    ),
    rule(
        &[Test::Exe("nvidia-persistenced")],
        "nvidia-persistenced",
        "NVIDIA persistence daemon",
        Category::System,
        "Keeps the GPU driver loaded so the GPU responds quickly.",
    ),
    rule(
        &[Test::Exe("nvidia-modprobe")],
        "nvidia-modprobe",
        "nvidia-modprobe",
        Category::System,
        "Loads NVIDIA kernel modules on demand.",
    ),
    rule(
        &[Test::Exe("nvidia-powerd")],
        "nvidia-powerd",
        "NVIDIA Dynamic Boost",
        Category::System,
        "Shares power between the CPU and GPU in laptops.",
    ),
    rule(
        &[Test::Exe("nvidia-settings")],
        "nvidia-settings",
        "NVIDIA Settings",
        Category::System,
        "Configures the NVIDIA driver and display settings.",
    ),
    rule(
        &[
            Test::Exe("nv-hostengine"),
            Test::Exe("dcgmi"),
            Test::Exe("dcgm-exporter"),
        ],
        "dcgm",
        "NVIDIA DCGM",
        Category::System,
        "Collects GPU health metrics for fleet monitoring.",
    ),
];

const LAUNCHER_AND_APP_RULES: &[Rule] = &[
    rule(
        &[Test::Exe("lutris")],
        "lutris",
        "Lutris",
        Category::Game,
        LAUNCHER,
    ),
    rule(
        &[Test::Exe("bottles"), Test::Exe("bottles-cli")],
        "bottles",
        "Bottles",
        Category::Game,
        LAUNCHER,
    ),
    rule(
        &[Test::Exe("thunderbird")],
        "app:thunderbird",
        "Thunderbird",
        Category::Other,
        "Email client; draws its window with the GPU.",
    ),
    rule(
        &[Test::Exe("ghostty")],
        "terminal:ghostty",
        "Ghostty",
        Category::Other,
        "Terminal emulator; draws its text on the GPU.",
    ),
    rule(
        &[Test::Exe("kitty")],
        "terminal:kitty",
        "kitty",
        Category::Other,
        "Terminal emulator; draws its text on the GPU.",
    ),
    rule(
        &[Test::Exe("alacritty")],
        "terminal:alacritty",
        "Alacritty",
        Category::Other,
        "Terminal emulator; draws its text on the GPU.",
    ),
    rule(
        &[Test::Exe("wezterm-gui")],
        "terminal:wezterm",
        "WezTerm",
        Category::Other,
        "Terminal emulator; draws its text on the GPU.",
    ),
    rule(
        &[Test::Exe("foot")],
        "terminal:foot",
        "foot",
        Category::Other,
        "Terminal emulator; draws its text on the GPU.",
    ),
    rule(
        &[Test::Exe("emojipick")],
        "app:emojipick",
        "emojipick",
        Category::Other,
        "Emoji picker that copies the emoji you choose.",
    ),
    rule(
        &[Test::Exe("handy")],
        "app:handy",
        "Handy",
        Category::Other,
        DESKTOP_APP,
    ),
    rule(
        &[Test::Exe("tailscode")],
        "app:tailscode",
        "Tailscode",
        Category::Other,
        "Client for coding agents such as Claude Code and opencode.",
    ),
    rule(
        &[Test::Exe("flaccy")],
        "app:flaccy",
        "Flaccy",
        Category::Other,
        DESKTOP_APP,
    ),
];

/// Names what a process is, or `None` when no built-in rule applies.
pub fn classify(meta: &ProcMeta, steam: &SteamLibrary) -> Option<Identity> {
    let probe = Probe::new(meta);
    let found = steam_identity(meta, steam, &probe)
        .or_else(|| table_identity(&probe, DESKTOP_RULES))
        .or_else(|| table_identity(&probe, AI_RULES))
        .or_else(|| table_identity(&probe, MEDIA_RULES))
        .or_else(|| table_identity(&probe, CREATIVE_RULES))
        .or_else(|| browser_identity(meta, &probe))
        .or_else(|| chromium_app_identity(meta, &probe))
        .or_else(|| table_identity(&probe, DEV_SYSTEM_RULES))
        .or_else(|| wine_identity(meta, &probe))
        .or_else(|| table_identity(&probe, LAUNCHER_AND_APP_RULES))?;
    Some(with_container(found, meta))
}

/// Lowercased copies of the process facts that rule tests compare against.
struct Probe {
    exe: String,
    comm: String,
    line: String,
    path: String,
}

impl Probe {
    fn new(meta: &ProcMeta) -> Self {
        Self {
            exe: meta.exe_name().to_ascii_lowercase(),
            comm: meta.comm.to_ascii_lowercase(),
            line: meta.command_line().to_ascii_lowercase(),
            path: meta
                .exe
                .as_ref()
                .map(|path| path.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
        }
    }

    fn matches(&self, test: &Test) -> bool {
        match test {
            Test::Exe(name) => self.exe == *name,
            Test::Comm(prefix) => self.comm.starts_with(*prefix),
            Test::Arg(needle) => self.line.contains(*needle),
            Test::Path(needle) => self.path.contains(*needle),
            Test::All(tests) => tests.iter().all(|test| self.matches(test)),
        }
    }
}

fn builtin(
    key: impl Into<String>,
    name: impl Into<String>,
    category: Category,
    description: impl Into<String>,
) -> Identity {
    Identity {
        key: key.into(),
        name: name.into(),
        category,
        description: description.into(),
        source: IdentitySource::Builtin,
    }
}

fn table_identity(probe: &Probe, rules: &[Rule]) -> Option<Identity> {
    rules
        .iter()
        .find(|rule| rule.matches(probe))
        .map(Rule::identity)
}

fn steam_identity(meta: &ProcMeta, steam: &SteamLibrary, probe: &Probe) -> Option<Identity> {
    table_identity(probe, STEAM_RULES).or_else(|| steam_game_identity(meta, steam, probe))
}

fn steam_game_identity(meta: &ProcMeta, steam: &SteamLibrary, probe: &Probe) -> Option<Identity> {
    let name = steam_name_from_paths(meta, steam).or_else(|| steam_name_from_env(meta, steam))?;
    let description = if is_windows_program(probe) {
        "A game running through Steam. Windows game via Proton."
    } else {
        "A game running through Steam."
    };
    Some(builtin(
        format!("game:{}", slug(&name)),
        name,
        Category::Game,
        description,
    ))
}

fn steam_name_from_paths(meta: &ProcMeta, steam: &SteamLibrary) -> Option<String> {
    let exe = meta.exe.as_ref().and_then(|path| path.to_str());
    exe.into_iter()
        .chain(meta.cmdline.iter().map(String::as_str))
        .find_map(|text| steam.name_for_path(text))
}

fn steam_name_from_env(meta: &ProcMeta, steam: &SteamLibrary) -> Option<String> {
    const STEAM_APP_ID_VARS: [&str; 3] = ["SteamAppId", "SteamGameId", "STEAM_COMPAT_APP_ID"];
    STEAM_APP_ID_VARS
        .iter()
        .filter_map(|var| meta.env_hints.get(*var))
        .find_map(|app_id| steam.name_for_app_id(app_id))
}

fn is_windows_program(probe: &Probe) -> bool {
    WINDOWS_MARKERS
        .iter()
        .any(|marker| probe.path.contains(marker) || probe.line.contains(marker))
}

/// Lowercase words joined by dashes, so "Half-Life: Alyx" becomes "half-life-alyx".
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn browser_identity(meta: &ProcMeta, probe: &Probe) -> Option<Identity> {
    let (family, name) = BROWSERS
        .iter()
        .find(|entry| entry.0 == probe.exe)
        .map(|entry| (entry.1, entry.2))?;
    Some(builtin(
        format!("browser:{family}"),
        name,
        Category::Browser,
        browser_description(meta),
    ))
}

fn browser_description(meta: &ProcMeta) -> &'static str {
    match chromium_role(meta) {
        Some("gpu-process") => "Draws web pages with the GPU.",
        Some("renderer") => "Renders a web page tab.",
        Some(_) => "Browser helper process for web pages and extensions.",
        None if meta.cmdline.iter().any(|arg| arg == "-contentproc") => "Renders a web page tab.",
        None => "Web browser window and its tabs.",
    }
}

fn chromium_role(meta: &ProcMeta) -> Option<&str> {
    meta.cmdline
        .iter()
        .find_map(|arg| arg.strip_prefix("--type="))
}

fn chromium_app_identity(meta: &ProcMeta, probe: &Probe) -> Option<Identity> {
    let role = chromium_role(meta);
    let opt_dir = opt_app_dir(meta);
    match CHROMIUM_APPS
        .iter()
        .find(|app| matches_stem(app, opt_dir, &probe.exe))
    {
        Some(app) => Some(builtin(
            format!("app:{}", app.stems[0]),
            app.name,
            app.category,
            DESKTOP_APP,
        )),
        None if role == Some("gpu-process") || (opt_dir.is_some() && role.is_some()) => {
            let name = opt_dir.unwrap_or_else(|| meta.exe_name());
            let description = if role == Some("gpu-process") {
                DESKTOP_APP
            } else {
                OPT_PROGRAM
            };
            Some(builtin(
                format!("app:{}", name.to_ascii_lowercase()),
                name,
                Category::Other,
                description,
            ))
        }
        None => None,
    }
}

fn matches_stem(app: &ChromiumApp, opt_dir: Option<&str>, exe: &str) -> bool {
    let dir = opt_dir.map(str::to_ascii_lowercase);
    app.stems
        .iter()
        .any(|stem| dir.as_deref() == Some(*stem) || exe == *stem)
}

/// The app folder directly under `/opt`, as in `/opt/1Password/1password`.
fn opt_app_dir(meta: &ProcMeta) -> Option<&str> {
    let exe = meta.exe.as_deref()?.to_str()?;
    exe.strip_prefix("/opt/")?
        .split('/')
        .next()
        .filter(|dir| !dir.is_empty())
}

fn wine_identity(meta: &ProcMeta, probe: &Probe) -> Option<Identity> {
    let program = meta
        .cmdline
        .iter()
        .find_map(|arg| windows_program_stem(arg));
    match program {
        Some(stem) => Some(builtin(
            format!("wine:{}", slug(stem)),
            format!("Wine app ({stem})"),
            Category::Other,
            WINE,
        )),
        None if probe.exe.starts_with("wine") => {
            Some(builtin("wine", "Wine", Category::Other, WINE))
        }
        None => None,
    }
}

/// The file stem of a `.exe` argument, whichever slash style it uses.
fn windows_program_stem(arg: &str) -> Option<&str> {
    let file = arg.rsplit(['/', '\\']).next()?;
    let stem_len = file.len().checked_sub(EXE_SUFFIX.len())?;
    let tail = file.get(stem_len..)?;
    let stem = file.get(..stem_len)?;
    (tail.eq_ignore_ascii_case(EXE_SUFFIX) && !stem.is_empty()).then_some(stem)
}

/// Groups every helper process of one app under a single key, but keeps containers apart.
fn with_container(mut identity: Identity, meta: &ProcMeta) -> Identity {
    let scoped = !matches!(identity.category, Category::Desktop | Category::Browser);
    if let Some(container) = meta.container.as_ref().filter(|_| scoped) {
        identity.key = format!("{}@{}", identity.key, container.short_id());
    }
    identity
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::classify::steam::test_library;
    use crate::types::{Container, ContainerRuntime};

    fn build(
        exe: &str,
        args: &[&str],
        container: Option<Container>,
        env: &[(&str, &str)],
    ) -> ProcMeta {
        let file_name = exe.rsplit('/').next().unwrap_or(exe);
        ProcMeta {
            pid: 4242,
            accessible: true,
            comm: file_name.chars().take(15).collect(),
            exe: Some(PathBuf::from(exe)),
            cmdline: args.iter().map(|arg| arg.to_string()).collect(),
            container,
            env_hints: env
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            ..ProcMeta::default()
        }
    }

    fn plain(exe: &str, args: &[&str]) -> ProcMeta {
        build(exe, args, None, &[])
    }

    fn docker(id: &str) -> Container {
        Container {
            runtime: ContainerRuntime::Docker,
            id: id.to_string(),
        }
    }

    fn identify(meta: &ProcMeta) -> Option<Identity> {
        classify(meta, &SteamLibrary::with_roots(Vec::new()))
    }

    fn key_of(meta: &ProcMeta) -> String {
        identify(meta).expect("identity").key
    }

    const CASES: &[(&str, &[&str], &str, Category)] = &[
        (
            "/usr/bin/kwin_wayland",
            &["kwin_wayland"],
            "desktop:kwin",
            Category::Desktop,
        ),
        (
            "/usr/bin/kwin_wayland_wrapper",
            &["kwin_wayland_wrapper", "--wayland"],
            "desktop:kwin",
            Category::Desktop,
        ),
        (
            "/usr/bin/Xwayland",
            &["Xwayland", ":0"],
            "desktop:xwayland",
            Category::Desktop,
        ),
        (
            "/usr/lib/xorg/Xorg",
            &["Xorg", ":0"],
            "desktop:xorg",
            Category::Desktop,
        ),
        (
            "/usr/bin/gnome-shell",
            &["gnome-shell"],
            "desktop:gnome-shell",
            Category::Desktop,
        ),
        (
            "/usr/bin/mutter",
            &["mutter"],
            "desktop:mutter",
            Category::Desktop,
        ),
        (
            "/usr/bin/sway",
            &["sway"],
            "desktop:sway",
            Category::Desktop,
        ),
        (
            "/usr/bin/Hyprland",
            &["Hyprland"],
            "desktop:hyprland",
            Category::Desktop,
        ),
        (
            "/usr/bin/plasmashell",
            &["plasmashell"],
            "desktop:plasmashell",
            Category::Desktop,
        ),
        (
            "/usr/bin/kded6",
            &["kded6"],
            "desktop:kded",
            Category::Desktop,
        ),
        (
            "/usr/bin/niri",
            &["niri"],
            "desktop:niri",
            Category::Desktop,
        ),
        (
            "/usr/bin/picom",
            &["picom"],
            "desktop:picom",
            Category::Desktop,
        ),
        (
            "/usr/bin/llama-server",
            &["llama-server", "-m", "a.gguf"],
            "llama-server",
            Category::Ai,
        ),
        (
            "/usr/bin/llama-cli",
            &["llama-cli", "-p", "hi"],
            "llama-cli",
            Category::Ai,
        ),
        (
            "/usr/bin/ollama",
            &["ollama", "runner", "--model", "x"],
            "ollama",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "-m", "vllm.entrypoints.openai.api_server"],
            "vllm",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "-m", "sglang.launch_server"],
            "sglang",
            Category::Ai,
        ),
        (
            "/opt/tritonserver/bin/tritonserver",
            &["tritonserver"],
            "triton",
            Category::Ai,
        ),
        (
            "/usr/bin/text-generation-launcher",
            &["text-generation-launcher"],
            "text-generation-inference",
            Category::Ai,
        ),
        (
            "/usr/bin/koboldcpp",
            &["koboldcpp"],
            "koboldcpp",
            Category::Ai,
        ),
        (
            "/opt/LM Studio/lm-studio",
            &["lm-studio"],
            "lm-studio",
            Category::Ai,
        ),
        ("/usr/bin/jan", &["jan"], "jan", Category::Ai),
        (
            "/usr/bin/llamafile",
            &["llamafile", "-m", "x"],
            "llamafile",
            Category::Ai,
        ),
        (
            "/usr/bin/whisper-cli",
            &["whisper-cli", "-f", "a.wav"],
            "whisper",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "/home/u/faster-whisper/transcribe.py"],
            "faster-whisper",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "-u", "/srv/ai/ComfyUI/main.py", "--port", "8188"],
            "comfyui",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "/srv/stable-diffusion-webui/launch.py"],
            "stable-diffusion-webui",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "/srv/InvokeAI/invoke.py"],
            "invokeai",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "/srv/Fooocus/entry_with_update.py"],
            "fooocus",
            Category::Ai,
        ),
        (
            "/usr/bin/torchrun",
            &["torchrun", "--nproc-per-node", "2", "train.py"],
            "pytorch-run",
            Category::Ai,
        ),
        (
            "/usr/src/tensorrt/bin/trtexec",
            &["trtexec", "--onnx=a.onnx"],
            "tensorrt",
            Category::Ai,
        ),
        (
            "/usr/bin/mlc_llm",
            &["mlc_llm", "serve"],
            "mlc-llm",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "-m", "exllamav2.server"],
            "exllama",
            Category::Ai,
        ),
        (
            "/usr/bin/python3",
            &["python3", "/srv/tabbyAPI/main.py"],
            "tabbyapi",
            Category::Ai,
        ),
        (
            "/usr/bin/ffmpeg",
            &["ffmpeg", "-i", "a.mkv"],
            "ffmpeg",
            Category::Media,
        ),
        ("/usr/bin/obs", &["obs"], "obs-studio", Category::Media),
        (
            "/usr/bin/sunshine",
            &["sunshine"],
            "sunshine",
            Category::Media,
        ),
        ("/usr/bin/mpv", &["mpv", "a.mp4"], "mpv", Category::Media),
        ("/usr/bin/vlc", &["vlc"], "vlc", Category::Media),
        (
            "/usr/bin/celluloid",
            &["celluloid"],
            "celluloid",
            Category::Media,
        ),
        ("/usr/bin/ghb", &["ghb"], "handbrake", Category::Media),
        (
            "/usr/bin/kdenlive",
            &["kdenlive"],
            "kdenlive",
            Category::Media,
        ),
        ("/usr/bin/shotcut", &["shotcut"], "shotcut", Category::Media),
        (
            "/opt/resolve/bin/resolve",
            &["resolve"],
            "davinci-resolve",
            Category::Media,
        ),
        (
            "/usr/bin/gst-launch-1.0",
            &["gst-launch-1.0", "videotestsrc"],
            "gstreamer",
            Category::Media,
        ),
        (
            "/usr/lib/plexmediaserver/Plex Transcoder",
            &["Plex Transcoder"],
            "plex-transcoder",
            Category::Media,
        ),
        (
            "/usr/lib/jellyfin-ffmpeg/jellyfin-ffmpeg",
            &["jellyfin-ffmpeg"],
            "jellyfin-ffmpeg",
            Category::Media,
        ),
        (
            "/usr/bin/wf-recorder",
            &["wf-recorder"],
            "wf-recorder",
            Category::Media,
        ),
        ("/usr/bin/kooha", &["kooha"], "kooha", Category::Media),
        (
            "/usr/bin/gpu-screen-recorder",
            &["gpu-screen-recorder"],
            "gpu-screen-recorder",
            Category::Media,
        ),
        (
            "/usr/bin/simplescreenrecorder",
            &["simplescreenrecorder"],
            "simplescreenrecorder",
            Category::Media,
        ),
        (
            "/usr/bin/blender",
            &["blender", "-b"],
            "blender",
            Category::Creative,
        ),
        ("/usr/bin/krita", &["krita"], "krita", Category::Creative),
        (
            "/usr/bin/gimp-2.10",
            &["gimp-2.10"],
            "gimp",
            Category::Creative,
        ),
        (
            "/usr/bin/inkscape",
            &["inkscape"],
            "inkscape",
            Category::Creative,
        ),
        ("/usr/bin/godot", &["godot"], "godot", Category::Creative),
        (
            "/home/u/Unity/Editor/Unity",
            &["Unity", "-projectPath", "x"],
            "unity",
            Category::Creative,
        ),
        (
            "/opt/UnrealEngine/Engine/Binaries/Linux/UnrealEditor",
            &["UnrealEditor"],
            "unreal-editor",
            Category::Creative,
        ),
        (
            "/usr/bin/darktable",
            &["darktable"],
            "darktable",
            Category::Creative,
        ),
        (
            "/usr/bin/rawtherapee",
            &["rawtherapee"],
            "rawtherapee",
            Category::Creative,
        ),
        (
            "/usr/bin/freecad",
            &["freecad"],
            "freecad",
            Category::Creative,
        ),
        (
            "/opt/hfs/bin/houdini",
            &["houdini"],
            "houdini",
            Category::Creative,
        ),
        ("/opt/maya/bin/maya", &["maya"], "maya", Category::Creative),
        (
            "/home/u/Games/Adobe Substance 3D Painter",
            &["Adobe Substance 3D Painter"],
            "substance-painter",
            Category::Creative,
        ),
        (
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin", "--type=gpu-process"],
            "browser:vivaldi",
            Category::Browser,
        ),
        (
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin", "--type=renderer"],
            "browser:vivaldi",
            Category::Browser,
        ),
        (
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin"],
            "browser:vivaldi",
            Category::Browser,
        ),
        (
            "/usr/lib/chromium/chromium",
            &["chromium", "--type=gpu-process"],
            "browser:chromium",
            Category::Browser,
        ),
        (
            "/opt/google/chrome/chrome",
            &["chrome", "--type=gpu-process"],
            "browser:chrome",
            Category::Browser,
        ),
        (
            "/usr/lib/firefox/firefox",
            &["firefox", "-contentproc", "-childID", "1"],
            "browser:firefox",
            Category::Browser,
        ),
        (
            "/usr/lib/librewolf/librewolf",
            &["librewolf"],
            "browser:librewolf",
            Category::Browser,
        ),
        (
            "/usr/bin/brave",
            &["brave", "--type=gpu-process"],
            "browser:brave",
            Category::Browser,
        ),
        (
            "/usr/bin/microsoft-edge",
            &["microsoft-edge"],
            "browser:edge",
            Category::Browser,
        ),
        (
            "/usr/bin/opera",
            &["opera", "--type=renderer"],
            "browser:opera",
            Category::Browser,
        ),
        (
            "/usr/bin/zen-bin",
            &["zen-bin"],
            "browser:zen",
            Category::Browser,
        ),
        (
            "/opt/1Password/1password",
            &["1password", "--type=gpu-process"],
            "app:1password",
            Category::Other,
        ),
        (
            "/opt/Discord/Discord",
            &["Discord", "--type=gpu-process"],
            "app:discord",
            Category::Other,
        ),
        ("/usr/share/code/code", &["code"], "app:code", Category::Dev),
        (
            "/opt/obsidian/obsidian",
            &["obsidian"],
            "app:obsidian",
            Category::Other,
        ),
        (
            "/usr/lib/signal-desktop/signal-desktop",
            &["signal-desktop"],
            "app:signal-desktop",
            Category::Other,
        ),
        (
            "/opt/Spotify/spotify",
            &["spotify", "--type=utility"],
            "app:spotify",
            Category::Other,
        ),
        (
            "/opt/Foo Bar/foobar",
            &["foobar", "--type=gpu-process"],
            "app:foo bar",
            Category::Other,
        ),
        (
            "/usr/bin/somechromeapp",
            &["somechromeapp", "--type=gpu-process"],
            "app:somechromeapp",
            Category::Other,
        ),
        (
            "/opt/heroic/heroic",
            &["heroic"],
            "app:heroic",
            Category::Game,
        ),
        (
            "/opt/cuda/bin/ncu",
            &["ncu"],
            "nsight-compute",
            Category::Dev,
        ),
        (
            "/opt/cuda/bin/nsys",
            &["nsys", "profile"],
            "nsight-systems",
            Category::Dev,
        ),
        (
            "/opt/cuda/bin/cuda-gdb",
            &["cuda-gdb"],
            "cuda-gdb",
            Category::Dev,
        ),
        (
            "/opt/cuda/bin/compute-sanitizer",
            &["compute-sanitizer"],
            "compute-sanitizer",
            Category::Dev,
        ),
        ("/usr/bin/nvtop", &["nvtop"], "nvtop", Category::System),
        (
            "/usr/bin/nvidia-smi",
            &["nvidia-smi", "dmon"],
            "nvidia-smi",
            Category::System,
        ),
        (
            "/usr/bin/nvidia-persistenced",
            &["nvidia-persistenced"],
            "nvidia-persistenced",
            Category::System,
        ),
        (
            "/usr/bin/nvidia-modprobe",
            &["nvidia-modprobe"],
            "nvidia-modprobe",
            Category::System,
        ),
        (
            "/usr/bin/nvidia-powerd",
            &["nvidia-powerd"],
            "nvidia-powerd",
            Category::System,
        ),
        (
            "/usr/bin/nv-hostengine",
            &["nv-hostengine"],
            "dcgm",
            Category::System,
        ),
        (
            "/home/u/Dev/gpulens/target/release/gpulens",
            &["gpulens"],
            "gpulens",
            Category::System,
        ),
        (
            "/usr/lib/wine/wine64",
            &["wine64", "C:\\Games\\Foo\\foo.exe"],
            "wine:foo",
            Category::Other,
        ),
        (
            "/usr/bin/wineserver",
            &["wineserver", "-f"],
            "wine",
            Category::Other,
        ),
        ("/usr/bin/lutris", &["lutris"], "lutris", Category::Game),
        ("/usr/bin/bottles", &["bottles"], "bottles", Category::Game),
        (
            "/usr/bin/thunderbird",
            &["thunderbird"],
            "app:thunderbird",
            Category::Other,
        ),
        (
            "/usr/bin/emojipick",
            &["emojipick"],
            "app:emojipick",
            Category::Other,
        ),
        ("/usr/bin/handy", &["handy"], "app:handy", Category::Other),
        (
            "/usr/bin/tailscode",
            &["tailscode"],
            "app:tailscode",
            Category::Other,
        ),
        (
            "/usr/bin/flaccy",
            &["flaccy"],
            "app:flaccy",
            Category::Other,
        ),
    ];

    #[test]
    fn every_builtin_case_gets_its_key_and_category() {
        for &(exe, args, key, category) in CASES {
            let id = identify(&plain(exe, args)).unwrap_or_else(|| panic!("no identity for {exe}"));
            assert_eq!(id.key, key, "key for {exe}");
            assert_eq!(id.category, category, "category for {exe}");
            assert_eq!(id.source, IdentitySource::Builtin, "source for {exe}");
        }
    }

    #[test]
    fn display_names_are_human_readable() {
        assert_eq!(
            identify(&plain("/usr/bin/vllm-serve", &["vllm"])).map(|i| i.name),
            Some("vLLM".to_string())
        );
        assert_eq!(
            identify(&plain("/usr/bin/llama-server", &["llama-server"])).map(|i| i.name),
            Some("llama.cpp server".to_string())
        );
        assert_eq!(
            identify(&plain(
                "/usr/lib/wine/wine64",
                &["wine64", "C:\\Games\\Foo\\foo.exe"]
            ))
            .map(|i| i.name),
            Some("Wine app (foo)".to_string())
        );
        assert_eq!(
            identify(&plain("/opt/vivaldi/vivaldi-bin", &["vivaldi-bin"])).map(|i| i.name),
            Some("Vivaldi".to_string())
        );
        assert_eq!(
            identify(&plain("/opt/google/chrome/chrome", &["chrome"])).map(|i| i.name),
            Some("Google Chrome".to_string())
        );
        assert_eq!(
            identify(&plain("/usr/share/code/code", &["code"])).map(|i| i.name),
            Some("Visual Studio Code".to_string())
        );
        assert_eq!(
            identify(&plain(
                "/opt/1Password/1password",
                &["1password", "--type=gpu-process"]
            ))
            .map(|i| i.name),
            Some("1Password".to_string())
        );
        assert_eq!(
            identify(&plain(
                "/opt/Foo Bar/foobar",
                &["foobar", "--type=gpu-process"]
            ))
            .map(|i| i.name),
            Some("Foo Bar".to_string())
        );
    }

    #[test]
    fn descriptions_are_one_plain_sentence_under_ninety_characters() {
        let tables: [&[Rule]; 7] = [
            STEAM_RULES,
            DESKTOP_RULES,
            AI_RULES,
            MEDIA_RULES,
            CREATIVE_RULES,
            DEV_SYSTEM_RULES,
            LAUNCHER_AND_APP_RULES,
        ];
        for rule in tables.into_iter().flatten() {
            assert!(rule.description.len() <= 90, "too long: {}", rule.key);
            assert!(rule.description.ends_with('.'), "no period: {}", rule.key);
        }
        for description in [
            browser_description(&plain("/opt/x/x", &["x", "--type=gpu-process"])),
            WINE,
            OPT_PROGRAM,
        ] {
            assert!(
                description.len() <= 90 && description.ends_with('.'),
                "{description}"
            );
        }
    }

    #[test]
    fn every_pattern_is_lowercase() {
        fn lowercase(test: &Test) -> bool {
            match test {
                Test::Exe(p) | Test::Comm(p) | Test::Arg(p) | Test::Path(p) => {
                    *p == p.to_ascii_lowercase()
                }
                Test::All(inner) => inner.iter().all(lowercase),
            }
        }
        let tables: [&[Rule]; 7] = [
            STEAM_RULES,
            DESKTOP_RULES,
            AI_RULES,
            MEDIA_RULES,
            CREATIVE_RULES,
            DEV_SYSTEM_RULES,
            LAUNCHER_AND_APP_RULES,
        ];
        for rule in tables.into_iter().flatten() {
            assert!(rule.tests.iter().all(lowercase), "{}", rule.key);
        }
    }

    #[test]
    fn table_holds_at_least_ninety_patterns() {
        fn leaves(tests: &[Test]) -> usize {
            tests
                .iter()
                .map(|test| match test {
                    Test::All(inner) => leaves(inner),
                    _ => 1,
                })
                .sum()
        }
        let tables: [&[Rule]; 7] = [
            STEAM_RULES,
            DESKTOP_RULES,
            AI_RULES,
            MEDIA_RULES,
            CREATIVE_RULES,
            DEV_SYSTEM_RULES,
            LAUNCHER_AND_APP_RULES,
        ];
        let rule_patterns: usize = tables
            .iter()
            .flat_map(|table| table.iter())
            .map(|rule| leaves(rule.tests))
            .sum();
        let chromium_stems: usize = CHROMIUM_APPS.iter().map(|app| app.stems.len()).sum();
        let total = rule_patterns + BROWSERS.len() + chromium_stems + WINDOWS_MARKERS.len();
        assert!(total >= 90, "only {total} patterns");
    }

    #[test]
    fn same_program_keeps_one_key_across_processes() {
        let gpu = plain(
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin", "--type=gpu-process"],
        );
        let renderer = plain(
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin", "--type=renderer"],
        );
        assert_eq!(key_of(&gpu), key_of(&renderer));
        let mut second_server = plain("/usr/bin/llama-server", &["llama-server", "-m", "a.gguf"]);
        second_server.pid = 9001;
        assert_eq!(
            key_of(&second_server),
            key_of(&plain(
                "/usr/bin/llama-server",
                &["llama-server", "-m", "b.gguf"]
            ))
        );
        let mut worker = plain("/usr/bin/python3", &["python3"]);
        worker.comm = "VLLM::Worker_TP0".to_string();
        assert_eq!(key_of(&worker), "vllm");
    }

    #[test]
    fn containers_split_groups_except_desktop_and_browser() {
        let vllm = |id: &str| {
            build(
                "/usr/bin/python3",
                &["python3", "-m", "vllm.entrypoints"],
                Some(docker(id)),
                &[],
            )
        };
        assert_eq!(
            key_of(&vllm("3f2a9c1d4b7e8a6f0123456789abcdef")),
            "vllm@3f2a9c1d4b7e"
        );
        assert_eq!(
            key_of(&vllm("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
            "vllm@aaaaaaaaaaaa"
        );
        let kwin = build(
            "/usr/bin/kwin_wayland",
            &["kwin_wayland"],
            Some(docker("3f2a9c1d4b7e8a6f0123456789abcdef")),
            &[],
        );
        assert_eq!(key_of(&kwin), "desktop:kwin");
        let browser = build(
            "/opt/vivaldi/vivaldi-bin",
            &["vivaldi-bin"],
            Some(docker("3f2a9c1d4b7e8a6f0123456789abcdef")),
            &[],
        );
        assert_eq!(key_of(&browser), "browser:vivaldi");
        let flatpak = Container {
            runtime: ContainerRuntime::Flatpak,
            id: "org.mozilla.firefox".to_string(),
        };
        let app = build(
            "/usr/bin/llama-server",
            &["llama-server"],
            Some(flatpak),
            &[],
        );
        assert_eq!(key_of(&app), "llama-server@org.mozilla.firefox");
    }

    #[test]
    fn steam_client_helpers_and_games_are_told_apart() {
        let (_dir, library) = test_library();
        let resolve = |meta: &ProcMeta| classify(meta, &library).expect("identity");

        let client = resolve(&plain(
            "/home/u/.local/share/Steam/ubuntu12_32/steam",
            &["steam"],
        ));
        assert_eq!(
            (client.key.as_str(), client.name.as_str(), client.category),
            ("steam", "Steam", Category::Other)
        );

        let web = resolve(&plain(
            "/home/u/.local/share/Steam/ubuntu12_32/steamwebhelper",
            &["steamwebhelper"],
        ));
        assert_eq!(web.key, "steam");

        let game_path = "/home/u/.local/share/Steam/steamapps/common/Counter-Strike Global Offensive/game/bin/linuxsteamrt64/cs2";
        let helper = resolve(&plain(
            "/usr/lib/steam/reaper",
            &["reaper", "SteamLaunch", "AppId=730", "--", game_path],
        ));
        assert_eq!(
            (helper.key.as_str(), helper.category),
            ("steam-helper", Category::System)
        );

        let pressure = resolve(&plain(
            "/home/u/.local/share/Steam/steamapps/common/SteamLinuxRuntime_sniper/pressure-vessel/bin/pressure-vessel-wrap",
            &["pressure-vessel-wrap", "--", game_path],
        ));
        assert_eq!(pressure.key, "steam-helper");

        let gamescope = resolve(&plain(
            "/usr/bin/gamescope",
            &["gamescope", "--", game_path],
        ));
        assert_eq!(
            (gamescope.key.as_str(), gamescope.category),
            ("gamescope", Category::Game)
        );

        let native = resolve(&plain(game_path, &["cs2", "-steam"]));
        assert_eq!(native.key, "game:counter-strike-2");
        assert_eq!(native.name, "Counter-Strike 2");
        assert_eq!(native.category, Category::Game);
        assert_eq!(native.description, "A game running through Steam.");
        assert_eq!(native.source, IdentitySource::Builtin);

        let mut second_pid = plain(game_path, &["cs2"]);
        second_pid.pid = 77;
        assert_eq!(resolve(&second_pid).key, "game:counter-strike-2");

        let windows = resolve(&plain(
            "/usr/lib/wine/wine64",
            &[
                "wine64",
                r"Z:\home\u\second\steamapps\common\ELDEN RING\Game\eldenring.exe",
            ],
        ));
        assert_eq!(windows.key, "game:elden-ring");
        assert_eq!(
            windows.description,
            "A game running through Steam. Windows game via Proton."
        );

        let by_env = classify(
            &build(
                "/usr/bin/some-launcher",
                &["some-launcher"],
                None,
                &[("SteamAppId", "1245620")],
            ),
            &library,
        )
        .expect("identity");
        assert_eq!(by_env.key, "game:elden-ring");

        let scoped = classify(
            &build(
                game_path,
                &["cs2"],
                Some(docker("3f2a9c1d4b7e8a6f0123456789abcdef")),
                &[],
            ),
            &library,
        )
        .expect("identity");
        assert_eq!(scoped.key, "game:counter-strike-2@3f2a9c1d4b7e");

        let proton_only = resolve(&plain(
            "/home/u/.local/share/Steam/steamapps/common/Proton 9.0 (Beta)/files/bin/wine64",
            &["wine64"],
        ));
        assert_eq!(
            (proton_only.key.as_str(), proton_only.category),
            ("wine", Category::Other)
        );
    }

    #[test]
    fn unlisted_steam_folder_still_counts_as_a_game() {
        assert_eq!(
            key_of(&plain(
                "/x/steamapps/common/Some_Unlisted-Game/Game.exe",
                &["Game.exe"]
            )),
            "game:some-unlisted-game"
        );
    }

    #[test]
    fn windows_program_stems_handle_both_slashes() {
        assert_eq!(windows_program_stem(r"C:\Games\Foo\foo.exe"), Some("foo"));
        assert_eq!(windows_program_stem("/opt/game/Game.EXE"), Some("Game"));
        assert_eq!(windows_program_stem(".exe"), None);
        assert_eq!(windows_program_stem("notes.txt"), None);
        assert_eq!(windows_program_stem(""), None);
    }

    #[test]
    fn slugs_drop_punctuation_and_repeats() {
        assert_eq!(slug("Half-Life: Alyx"), "half-life-alyx");
        assert_eq!(slug("Counter-Strike 2"), "counter-strike-2");
        assert_eq!(slug("  ** "), "");
    }

    #[test]
    fn unknown_and_empty_processes_yield_none() {
        assert!(identify(&plain("/usr/bin/mystery-tool", &["mystery-tool", "--flag"])).is_none());
        assert!(
            identify(&plain(
                "/usr/bin/python3",
                &["python3", "/home/u/train_model.py"]
            ))
            .is_none()
        );
        assert!(identify(&ProcMeta::default()).is_none());
        assert!(identify(&plain("/opt/cuda/bin/unknown-tool", &["unknown-tool"])).is_none());
    }

    #[test]
    fn main_browser_process_and_helper_roles_describe_themselves() {
        assert_eq!(
            browser_description(&plain(
                "/x/vivaldi-bin",
                &["vivaldi-bin", "--type=gpu-process"]
            )),
            "Draws web pages with the GPU."
        );
        assert_eq!(
            browser_description(&plain(
                "/x/vivaldi-bin",
                &["vivaldi-bin", "--type=renderer"]
            )),
            "Renders a web page tab."
        );
        assert_eq!(
            browser_description(&plain("/x/vivaldi-bin", &["vivaldi-bin"])),
            "Web browser window and its tabs."
        );
        assert_eq!(
            browser_description(&plain("/x/vivaldi-bin", &["vivaldi-bin", "--type=utility"])),
            "Browser helper process for web pages and extensions."
        );
    }
}
