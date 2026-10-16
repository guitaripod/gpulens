//! Per-process metadata read from a `/proc`-shaped tree.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use nix::unistd::{Uid, User};

use crate::types::{Container, ContainerRuntime, Pid, ProcMeta};

const ENVIRON_LIMIT: u64 = 256 * 1024;
const ENV_ALLOW_LIST: [&str; 8] = [
    "SteamAppId",
    "SteamGameId",
    "STEAM_COMPAT_APP_ID",
    "STEAM_COMPAT_DATA_PATH",
    "FLATPAK_ID",
    "SNAP_NAME",
    "APPIMAGE",
    "WINEPREFIX",
];
const DELETED_SUFFIX: &[u8] = b" (deleted)";
const CONTAINER_ID_LEN: usize = 64;
const HEX_ID_PREFIXES: [(&str, ContainerRuntime); 5] = [
    ("docker-", ContainerRuntime::Docker),
    ("libpod-", ContainerRuntime::Podman),
    ("cri-containerd-", ContainerRuntime::Containerd),
    ("containerd-", ContainerRuntime::Containerd),
    ("crio-", ContainerRuntime::Containerd),
];

/// Reads `/proc/<pid>` entries and caches the results per process start time.
pub struct ProcReader {
    root: PathBuf,
    cache: HashMap<Pid, ProcMeta>,
    users: HashMap<u32, Option<String>>,
}

impl ProcReader {
    /// A reader over the live `/proc` filesystem.
    pub fn new() -> Self {
        Self::with_root("/proc")
    }

    /// A reader over any directory laid out like `/proc`.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: HashMap::new(),
            users: HashMap::new(),
        }
    }

    /// Reads the metadata of `pid`. A process that vanished or whose stat cannot be read comes back with `accessible == false` and is not cached.
    pub fn read(&mut self, pid: Pid) -> ProcMeta {
        let pid_dir = self.root.join(pid.to_string());
        let Some(stat) = read_stat(&pid_dir) else {
            return inaccessible(pid);
        };
        if let Some(cached) = self.cached_if_same_start(pid, stat.start_ticks) {
            return cached;
        }
        let meta = self.build(pid, &pid_dir, &stat);
        self.cache.insert(pid, meta.clone());
        meta
    }

    /// Drops cached entries for every pid that is not in `live`.
    pub fn retain_only(&mut self, live: &HashSet<Pid>) {
        self.cache.retain(|pid, _| live.contains(pid));
    }

    /// A cached entry is reusable only while it is accessible and was taken from the same process start, so a reused pid number forces a rebuild.
    fn cached_if_same_start(&self, pid: Pid, start_ticks: u64) -> Option<ProcMeta> {
        self.cache
            .get(&pid)
            .filter(|cached| cached.accessible && cached.start_ticks == Some(start_ticks))
            .cloned()
    }

    fn build(&mut self, pid: Pid, pid_dir: &Path, stat: &Stat) -> ProcMeta {
        let uid = read_text(&pid_dir.join("status"))
            .as_deref()
            .and_then(parse_uid);
        let user = uid.and_then(|uid| self.user_name(uid));
        let cgroup = read_text(&pid_dir.join("cgroup")).unwrap_or_default();
        let (container, unit) = parse_cgroup(&cgroup);
        ProcMeta {
            pid,
            accessible: true,
            comm: read_comm(pid_dir, &stat.content),
            exe: read_exe(pid_dir),
            cmdline: read_cmdline(pid_dir),
            uid,
            user,
            ppid: Some(stat.ppid),
            start_ticks: Some(stat.start_ticks),
            container,
            unit,
            env_hints: read_env_hints(pid_dir),
        }
    }

    fn user_name(&mut self, uid: u32) -> Option<String> {
        self.users
            .entry(uid)
            .or_insert_with(|| lookup_user(uid))
            .clone()
    }
}

impl Default for ProcReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses `/proc/<pid>/stat` into `(ppid, starttime ticks)`. The command name may contain spaces and parentheses, so fields are counted from the last `)`.
pub fn parse_stat(content: &str) -> Option<(Pid, u64)> {
    let after_comm = content.get(content.rfind(')')? + 1..)?;
    let mut fields = after_comm.split_whitespace();
    let ppid = fields.nth(1)?.parse().ok()?;
    let start_ticks = fields.nth(17)?.parse().ok()?;
    Some((ppid, start_ticks))
}

/// Derives the container and, outside containers, the systemd unit from `/proc/<pid>/cgroup`. The cgroup v2 line is preferred over v1 lines.
pub fn parse_cgroup(content: &str) -> (Option<Container>, Option<String>) {
    let Some(path) = preferred_cgroup_path(content) else {
        return (None, None);
    };
    match detect_container(path) {
        Some(container) => (Some(container), None),
        None => (None, unit_of(path)),
    }
}

struct Stat {
    content: String,
    ppid: Pid,
    start_ticks: u64,
}

fn inaccessible(pid: Pid) -> ProcMeta {
    ProcMeta {
        pid,
        accessible: false,
        ..ProcMeta::default()
    }
}

fn read_text(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn read_stat(pid_dir: &Path) -> Option<Stat> {
    let content = read_text(&pid_dir.join("stat"))?;
    let (ppid, start_ticks) = parse_stat(&content)?;
    Some(Stat {
        content,
        ppid,
        start_ticks,
    })
}

fn read_comm(pid_dir: &Path, stat: &str) -> String {
    read_text(&pid_dir.join("comm"))
        .map(|comm| comm.trim_end_matches('\n').to_string())
        .or_else(|| comm_from_stat(stat))
        .unwrap_or_default()
}

/// The command name between the first `(` and the last `)` of a stat line.
fn comm_from_stat(stat: &str) -> Option<String> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    stat.get(open + 1..close).map(str::to_string)
}

fn read_exe(pid_dir: &Path) -> Option<PathBuf> {
    fs::read_link(pid_dir.join("exe"))
        .ok()
        .map(strip_deleted_suffix)
}

/// Removes the ` (deleted)` marker the kernel appends to the link of a replaced executable.
fn strip_deleted_suffix(link: PathBuf) -> PathBuf {
    let bytes = link.as_os_str().as_encoded_bytes();
    match bytes.strip_suffix(DELETED_SUFFIX) {
        Some(trimmed) => PathBuf::from(OsString::from_vec(trimmed.to_vec())),
        None => link,
    }
}

fn read_cmdline(pid_dir: &Path) -> Vec<String> {
    match fs::read(pid_dir.join("cmdline")) {
        Ok(bytes) => split_cmdline(&bytes),
        Err(_) => Vec::new(),
    }
}

/// Splits NUL-terminated argv, dropping the single empty item left by the final terminator.
fn split_cmdline(bytes: &[u8]) -> Vec<String> {
    let mut args: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    if args.last().is_some_and(String::is_empty) {
        args.pop();
    }
    args
}

fn parse_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|real| real.parse().ok())
}

fn lookup_user(uid: u32) -> Option<String> {
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

fn read_env_hints(pid_dir: &Path) -> BTreeMap<String, String> {
    read_environ(pid_dir)
        .split(|byte| *byte == 0)
        .filter_map(parse_env_pair)
        .collect()
}

/// Reads at most `ENVIRON_LIMIT` bytes of `environ`; permission-denied or missing files yield nothing.
fn read_environ(pid_dir: &Path) -> Vec<u8> {
    let Ok(file) = File::open(pid_dir.join("environ")) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = file.take(ENVIRON_LIMIT).read_to_end(&mut buf);
    buf
}

/// Keeps a `KEY=VALUE` entry only when its key is on the allow-list.
fn parse_env_pair(entry: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(entry);
    let (key, value) = text.split_once('=')?;
    ENV_ALLOW_LIST
        .contains(&key)
        .then(|| (key.to_string(), value.to_string()))
}

/// The unified (v2, hierarchy `0`) path when present, otherwise the first line's path.
fn preferred_cgroup_path(content: &str) -> Option<&str> {
    let entries: Vec<(&str, &str)> = content.lines().filter_map(split_cgroup_line).collect();
    entries
        .iter()
        .find(|(hierarchy, _)| *hierarchy == "0")
        .or_else(|| entries.first())
        .map(|(_, path)| *path)
}

/// Splits `hierarchy:controllers:path`, keeping any colons inside the path.
fn split_cgroup_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ':');
    let hierarchy = parts.next()?;
    parts.next()?;
    let path = parts.next()?;
    Some((hierarchy, path))
}

fn detect_container(path: &str) -> Option<Container> {
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    components
        .iter()
        .enumerate()
        .find_map(|(index, component)| {
            container_in_component(component)
                .or_else(|| container_in_pair(component, components.get(index + 1).copied()))
        })
}

fn container_in_component(component: &str) -> Option<Container> {
    let name = component.strip_suffix(".scope").unwrap_or(component);
    hex_id_container(name)
        .or_else(|| flatpak_app_id(name).map(|id| container(ContainerRuntime::Flatpak, id)))
        .or_else(|| snap_name(name).map(|id| container(ContainerRuntime::Snap, id)))
        .or_else(|| lxc_name(name).map(|id| container(ContainerRuntime::Lxc, id)))
}

/// Container runtimes whose cgroup component is `<prefix><64 hex>`.
fn hex_id_container(name: &str) -> Option<Container> {
    HEX_ID_PREFIXES.iter().find_map(|(prefix, runtime)| {
        name.strip_prefix(prefix)
            .filter(|id| is_container_id(id))
            .map(|id| container(*runtime, id))
    })
}

/// Two-component layouts such as `/docker/<id>` and `/lxc/<name>`.
fn container_in_pair(component: &str, next: Option<&str>) -> Option<Container> {
    match (component, next?) {
        ("docker", id) if is_container_id(id) => Some(container(ContainerRuntime::Docker, id)),
        ("lxc", name) => Some(container(ContainerRuntime::Lxc, name)),
        _ => None,
    }
}

fn container(runtime: ContainerRuntime, id: &str) -> Container {
    Container {
        runtime,
        id: id.to_string(),
    }
}

fn is_container_id(id: &str) -> bool {
    id.len() == CONTAINER_ID_LEN && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The application id in `app-flatpak-<appid>-<digits>`; the id may itself contain dashes.
fn flatpak_app_id(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("app-flatpak-")?;
    let (app_id, instance) = rest.rsplit_once('-')?;
    let instance_is_digits =
        !instance.is_empty() && instance.bytes().all(|byte| byte.is_ascii_digit());
    (!app_id.is_empty() && instance_is_digits).then_some(app_id)
}

/// The snap name in `snap.<name>.<app>-<uuid>`.
fn snap_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("snap.")?;
    let (snap, app) = rest.split_once('.')?;
    (!snap.is_empty() && !app.is_empty()).then_some(snap)
}

fn lxc_name(name: &str) -> Option<&str> {
    name.strip_prefix("lxc.payload.")
        .filter(|rest| !rest.is_empty())
}

/// The last `.service` or `.scope` component of a cgroup path.
fn unit_of(path: &str) -> Option<String> {
    path.split('/')
        .rev()
        .find(|component| component.ends_with(".service") || component.ends_with(".scope"))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const DOCKER_ID: &str = "3f2a9c1d4b7e8a6f0123456789abcdef3f2a9c1d4b7e8a6f0123456789abcdef";
    const PODMAN_ID: &str = "aa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233";
    const CONTAINERD_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

    struct FakeProc {
        pid: Pid,
        ppid: Pid,
        comm: String,
        start: u64,
        uid: u32,
        cmdline: Vec<u8>,
        cgroup: String,
        environ: Vec<u8>,
    }

    impl FakeProc {
        fn new(pid: Pid, comm: &str) -> Self {
            Self {
                pid,
                ppid: 1,
                comm: comm.to_string(),
                start: 1000 + u64::from(pid),
                uid: 1000,
                cmdline: format!("{comm}\0--flag\0").into_bytes(),
                cgroup: "0::/\n".to_string(),
                environ: Vec::new(),
            }
        }

        fn write(&self, root: &Path) {
            let dir = root.join(self.pid.to_string());
            fs::create_dir_all(&dir).unwrap();
            let stat = format!(
                "{} ({}) S {} 1 1 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 {} 0 0\n",
                self.pid, self.comm, self.ppid, self.start
            );
            fs::write(dir.join("stat"), stat).unwrap();
            fs::write(dir.join("comm"), format!("{}\n", self.comm)).unwrap();
            fs::write(dir.join("cmdline"), &self.cmdline).unwrap();
            fs::write(dir.join("cgroup"), &self.cgroup).unwrap();
            fs::write(dir.join("environ"), &self.environ).unwrap();
            let status = format!(
                "Name:\t{}\nUid:\t{u}\t{u}\t{u}\t{u}\n",
                self.comm,
                u = self.uid
            );
            fs::write(dir.join("status"), status).unwrap();
        }
    }

    fn cgroup_of(content: &str) -> (Option<Container>, Option<String>) {
        parse_cgroup(content)
    }

    fn docker_scope(id: &str) -> String {
        format!("docker-{id}.scope")
    }

    #[test]
    fn stat_parses_comm_with_spaces_and_parentheses() {
        let line =
            "42 (my (weird) name) S 7 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
        assert_eq!(parse_stat(line), Some((7, 987654)));
    }

    #[test]
    fn stat_rejects_malformed_lines() {
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S"), None);
        assert_eq!(
            parse_stat("1 (x) S 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 nope"),
            None
        );
    }

    #[test]
    fn comm_fallback_reads_between_outer_parentheses() {
        assert_eq!(comm_from_stat("9 (a (b) c) S"), Some("a (b) c".to_string()));
        assert_eq!(comm_from_stat("no parens"), None);
    }

    #[test]
    fn read_fills_every_field_from_fixture() {
        let root = tempfile::tempdir().unwrap();
        FakeProc::new(42, "firefox").write(root.path());
        let mut reader = ProcReader::with_root(root.path());
        let meta = reader.read(42);
        assert!(meta.accessible);
        assert_eq!(meta.pid, 42);
        assert_eq!(meta.comm, "firefox");
        assert_eq!(
            meta.cmdline,
            vec!["firefox".to_string(), "--flag".to_string()]
        );
        assert_eq!(meta.uid, Some(1000));
        assert_eq!(meta.ppid, Some(1));
        assert_eq!(meta.start_ticks, Some(1042));
        assert_eq!(meta.exe, None);
        assert_eq!(meta.container, None);
        assert_eq!(meta.unit, None);
        assert!(meta.env_hints.is_empty());
    }

    #[test]
    fn kernel_thread_has_empty_cmdline_and_no_exe() {
        let root = tempfile::tempdir().unwrap();
        let mut kthread = FakeProc::new(2, "kthreadd");
        kthread.ppid = 0;
        kthread.uid = 0;
        kthread.cmdline = Vec::new();
        kthread.write(root.path());
        let mut reader = ProcReader::with_root(root.path());
        let meta = reader.read(2);
        assert!(meta.accessible);
        assert_eq!(meta.comm, "kthreadd");
        assert!(meta.cmdline.is_empty());
        assert_eq!(meta.exe, None);
        assert_eq!(meta.uid, Some(0));
    }

    #[test]
    fn exe_link_has_deleted_marker_stripped() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeProc::new(7, "foo");
        fake.write(root.path());
        symlink("/usr/bin/foo (deleted)", root.path().join("7/exe")).unwrap();
        let mut reader = ProcReader::with_root(root.path());
        assert_eq!(reader.read(7).exe, Some(PathBuf::from("/usr/bin/foo")));
    }

    #[test]
    fn cgroup_v2_docker_scope_yields_full_id() {
        let line = format!("0::/system.slice/{}\n", docker_scope(DOCKER_ID));
        let (container, unit) = cgroup_of(&line);
        assert_eq!(
            container,
            Some(container_of(ContainerRuntime::Docker, DOCKER_ID))
        );
        assert_eq!(unit, None);
    }

    #[test]
    fn cgroup_v1_docker_path_yields_full_id() {
        let content = format!("11:devices:/docker/{DOCKER_ID}\n10:cpu:/docker/{DOCKER_ID}\n");
        let (container, unit) = cgroup_of(&content);
        assert_eq!(
            container,
            Some(container_of(ContainerRuntime::Docker, DOCKER_ID))
        );
        assert_eq!(unit, None);
    }

    #[test]
    fn cgroup_podman_scope_and_bare_component() {
        let scope = format!("0::/machine.slice/libpod-{PODMAN_ID}.scope\n");
        assert_eq!(
            cgroup_of(&scope).0,
            Some(container_of(ContainerRuntime::Podman, PODMAN_ID))
        );
        let bare = format!("12:memory:/libpod-{PODMAN_ID}/init\n");
        assert_eq!(
            cgroup_of(&bare).0,
            Some(container_of(ContainerRuntime::Podman, PODMAN_ID))
        );
    }

    #[test]
    fn cgroup_containerd_and_crio_are_containerd() {
        let cri = format!("0::/kubepods.slice/cri-containerd-{CONTAINERD_ID}.scope\n");
        assert_eq!(
            cgroup_of(&cri).0,
            Some(container_of(ContainerRuntime::Containerd, CONTAINERD_ID))
        );
        let plain = format!("0::/system.slice/containerd-{CONTAINERD_ID}.scope\n");
        assert_eq!(
            cgroup_of(&plain).0,
            Some(container_of(ContainerRuntime::Containerd, CONTAINERD_ID))
        );
        let crio = format!("0::/kubepods/crio-{CONTAINERD_ID}.scope\n");
        assert_eq!(
            cgroup_of(&crio).0,
            Some(container_of(ContainerRuntime::Containerd, CONTAINERD_ID))
        );
    }

    #[test]
    fn cgroup_lxc_payload_and_legacy_lxc_path() {
        let v2 = "0::/lxc.payload.web1/system.slice/sshd.service\n";
        assert_eq!(
            cgroup_of(v2).0,
            Some(container_of(ContainerRuntime::Lxc, "web1"))
        );
        let v1 = "5:cpu:/lxc/web2\n";
        assert_eq!(
            cgroup_of(v1).0,
            Some(container_of(ContainerRuntime::Lxc, "web2"))
        );
    }

    #[test]
    fn cgroup_flatpak_id_keeps_dashes_before_instance_digits() {
        let line = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.mozilla.firefox-4321.scope\n";
        assert_eq!(
            cgroup_of(line).0,
            Some(container_of(
                ContainerRuntime::Flatpak,
                "org.mozilla.firefox"
            ))
        );
        let dashed = "0::/app.slice/app-flatpak-com.example-my-app-99.scope\n";
        assert_eq!(
            cgroup_of(dashed).0,
            Some(container_of(
                ContainerRuntime::Flatpak,
                "com.example-my-app"
            ))
        );
    }

    #[test]
    fn cgroup_snap_yields_snap_name() {
        let line =
            "0::/system.slice/snap.firefox.firefox-0c1b2a3d-4e5f-6071-8293-a4b5c6d7e8f9.scope\n";
        assert_eq!(
            cgroup_of(line).0,
            Some(container_of(ContainerRuntime::Snap, "firefox"))
        );
    }

    #[test]
    fn cgroup_systemd_service_under_user_manager_is_unit() {
        let line =
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steam@123.service\n";
        assert_eq!(
            cgroup_of(line),
            (None, Some("app-steam@123.service".to_string()))
        );
    }

    #[test]
    fn cgroup_session_scope_is_unit() {
        let line = "0::/user.slice/user-1000.slice/session-2.scope\n";
        assert_eq!(cgroup_of(line), (None, Some("session-2.scope".to_string())));
    }

    #[test]
    fn cgroup_slice_or_root_has_no_unit() {
        assert_eq!(cgroup_of("0::/user.slice/user-1000.slice\n"), (None, None));
        assert_eq!(cgroup_of("0::/\n"), (None, None));
        assert_eq!(cgroup_of(""), (None, None));
    }

    #[test]
    fn cgroup_container_suppresses_unit() {
        let line = format!("0::/system.slice/docker-{DOCKER_ID}.scope/init.scope\n");
        assert_eq!(
            cgroup_of(&line),
            (
                Some(container_of(ContainerRuntime::Docker, DOCKER_ID)),
                None
            )
        );
    }

    #[test]
    fn cgroup_prefers_unified_line_over_legacy() {
        let content = format!("12:memory:/docker/{DOCKER_ID}\n0::/user.slice/session-5.scope\n");
        assert_eq!(
            cgroup_of(&content),
            (None, Some("session-5.scope".to_string()))
        );
    }

    #[test]
    fn env_hints_keep_only_allow_listed_keys() {
        let root = tempfile::tempdir().unwrap();
        let mut fake = FakeProc::new(30, "game.exe");
        fake.environ = b"PATH=/usr/bin\0AWS_SECRET_ACCESS_KEY=supersecret\0SteamAppId=730\0FLATPAK_ID=org.example.App\0WINEPREFIX=/home/u/pfx=x\0".to_vec();
        fake.write(root.path());
        let mut reader = ProcReader::with_root(root.path());
        let hints = reader.read(30).env_hints;
        let keys: Vec<&str> = hints.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["FLATPAK_ID", "SteamAppId", "WINEPREFIX"]);
        assert_eq!(hints["WINEPREFIX"], "/home/u/pfx=x");
        assert!(!hints.values().any(|value| value.contains("supersecret")));
    }

    #[test]
    fn cache_hit_ignores_disk_until_start_ticks_change() {
        let root = tempfile::tempdir().unwrap();
        let mut fake = FakeProc::new(10, "a");
        fake.start = 100;
        fake.cmdline = b"a\0".to_vec();
        fake.write(root.path());
        let mut reader = ProcReader::with_root(root.path());
        assert_eq!(reader.read(10).cmdline, vec!["a".to_string()]);
        assert!(reader.cache.contains_key(&10));

        fake.cmdline = b"b\0".to_vec();
        fake.write(root.path());
        assert_eq!(reader.read(10).cmdline, vec!["a".to_string()]);

        fake.start = 200;
        fake.cmdline = b"c\0".to_vec();
        fake.write(root.path());
        let reused = reader.read(10);
        assert_eq!(reused.cmdline, vec!["c".to_string()]);
        assert_eq!(reused.start_ticks, Some(200));
    }

    #[test]
    fn missing_pid_is_inaccessible_and_not_cached() {
        let root = tempfile::tempdir().unwrap();
        let mut reader = ProcReader::with_root(root.path());
        let meta = reader.read(999);
        assert!(!meta.accessible);
        assert_eq!(meta.pid, 999);
        assert!(meta.comm.is_empty());
        assert!(!reader.cache.contains_key(&999));
    }

    #[test]
    fn unreadable_stat_is_inaccessible() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("55")).unwrap();
        let mut reader = ProcReader::with_root(root.path());
        assert!(!reader.read(55).accessible);
        assert!(!reader.cache.contains_key(&55));
    }

    #[test]
    fn retain_only_drops_dead_pids() {
        let root = tempfile::tempdir().unwrap();
        FakeProc::new(10, "a").write(root.path());
        FakeProc::new(11, "b").write(root.path());
        let mut reader = ProcReader::with_root(root.path());
        reader.read(10);
        reader.read(11);
        let live: HashSet<Pid> = [10].into_iter().collect();
        reader.retain_only(&live);
        let mut remaining: Vec<Pid> = reader.cache.keys().copied().collect();
        remaining.sort_unstable();
        assert_eq!(remaining, vec![10]);
    }

    #[test]
    fn reads_own_process_from_real_proc() {
        let meta = ProcReader::new().read(std::process::id());
        assert!(meta.accessible);
        assert!(!meta.cmdline.is_empty());
        assert!(meta.uid.is_some());
    }

    fn container_of(runtime: ContainerRuntime, id: &str) -> Container {
        Container {
            runtime,
            id: id.to_string(),
        }
    }
}
