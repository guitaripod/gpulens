//! Names Steam games from appmanifest files and the library folders that hold them.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const COMMON_MARKER: &str = "steamapps/common/";
const RUNTIME_TOOL_PREFIXES: [&str; 4] = [
    "proton",
    "steamlinuxruntime",
    "steam linux runtime",
    "steamworks common redistributables",
];

/// The Steam library folders to search and, lazily, the games installed in them.
#[derive(Debug, Default)]
pub struct SteamLibrary {
    roots: Vec<PathBuf>,
    manifests: OnceLock<Manifests>,
}

impl SteamLibrary {
    /// Uses the default Steam install folders plus every library folder they list.
    pub fn discover() -> Self {
        let mut roots = default_roots();
        let listed: Vec<PathBuf> = roots.iter().flat_map(|root| library_roots(root)).collect();
        roots.extend(listed);
        let mut seen = HashSet::new();
        roots.retain(|root| seen.insert(fs::canonicalize(root).unwrap_or_else(|_| root.clone())));
        Self::with_roots(roots)
    }

    /// How many Steam library folders were found.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// Each root is a `steamapps` directory.
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            manifests: OnceLock::new(),
        }
    }

    /// Finds the game in any path that contains `steamapps/common/<Dir>`, with `/` or `\` separators.
    pub fn name_for_path(&self, path: &str) -> Option<String> {
        let normalized = path.replace('\\', "/");
        let dir = game_dir_in(&normalized)?;
        let name = self
            .manifests()
            .names_by_install_dir
            .get(&dir.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| prettify(dir));
        (!is_runtime_tool(dir) && !is_runtime_tool(&name)).then_some(name)
    }

    /// The name of an installed app id, or `None` for Steam's runtime and Proton tools.
    pub fn name_for_app_id(&self, app_id: &str) -> Option<String> {
        let name = self.manifests().names_by_app_id.get(app_id.trim())?;
        (!is_runtime_tool(name)).then(|| name.clone())
    }

    fn manifests(&self) -> &Manifests {
        self.manifests.get_or_init(|| Manifests::scan(&self.roots))
    }
}

#[derive(Debug, Default)]
struct Manifests {
    names_by_install_dir: HashMap<String, String>,
    names_by_app_id: HashMap<String, String>,
}

impl Manifests {
    fn scan(roots: &[PathBuf]) -> Self {
        let mut manifests = Self::default();
        for path in manifest_files(roots) {
            if let Ok(text) = fs::read_to_string(&path) {
                manifests.add(&path, &text);
            }
        }
        manifests
    }

    fn add(&mut self, path: &Path, text: &str) {
        let fields = ManifestFields::parse(text);
        let Some(name) = fields.name.filter(|name| !name.trim().is_empty()) else {
            return;
        };
        if let Some(app_id) = fields.app_id.or_else(|| app_id_from_file_name(path)) {
            self.names_by_app_id
                .entry(app_id)
                .or_insert_with(|| name.clone());
        }
        if let Some(install_dir) = fields.install_dir {
            self.names_by_install_dir
                .entry(install_dir.to_ascii_lowercase())
                .or_insert(name);
        }
    }
}

#[derive(Debug, Default)]
struct ManifestFields {
    app_id: Option<String>,
    name: Option<String>,
    install_dir: Option<String>,
}

impl ManifestFields {
    /// Reads the top-level keys of an appmanifest, ignoring nested blocks such as `UserConfig`.
    fn parse(text: &str) -> Self {
        let mut fields = Self::default();
        let mut depth: usize = 0;
        for line in text.lines() {
            let tokens = quoted_tokens(line);
            if let (1, [key, value]) = (depth, tokens.as_slice()) {
                fields.record(key, value);
            }
            depth = brace_depth_after(depth, line);
        }
        fields
    }

    fn record(&mut self, key: &str, value: &str) {
        let slot = match key {
            "appid" => &mut self.app_id,
            "name" => &mut self.name,
            "installdir" => &mut self.install_dir,
            _ => return,
        };
        slot.get_or_insert_with(|| value.to_string());
    }
}

/// Builds the lookup tables for every root.
fn default_roots() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => {
            let home = PathBuf::from(home);
            vec![
                home.join(".local/share/Steam/steamapps"),
                home.join(".steam/steam/steamapps"),
            ]
        }
        None => Vec::new(),
    }
}

/// Every `steamapps` folder a `libraryfolders.vdf` in `steamapps` lists.
fn library_roots(steamapps: &Path) -> Vec<PathBuf> {
    fs::read_to_string(steamapps.join("libraryfolders.vdf"))
        .map(|text| library_paths(&text))
        .unwrap_or_default()
}

/// The `steamapps` folder under every `"path"` entry of a `libraryfolders.vdf`.
fn library_paths(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| match quoted_tokens(line).as_slice() {
            [key, value] if key.as_str() == "path" => Some(PathBuf::from(value).join("steamapps")),
            _ => None,
        })
        .collect()
}

fn manifest_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| fs::read_dir(root).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_manifest_name(path))
        .collect();
    files.sort();
    files
}

fn is_manifest_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("appmanifest_") && name.ends_with(".acf"))
}

fn app_id_from_file_name(path: &Path) -> Option<String> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("appmanifest_")
        .map(str::to_string)
}

/// Splits one line of Valve KeyValues text into its quoted strings, unescaping `\"` and `\\`.
fn quoted_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match current.as_mut() {
            None if c == '"' => current = Some(String::new()),
            None => {}
            Some(_) if c == '"' => tokens.extend(current.take()),
            Some(buf) if c == '\\' => buf.extend(chars.next()),
            Some(buf) => buf.push(c),
        }
    }
    tokens
}

fn brace_depth_after(depth: usize, line: &str) -> usize {
    let opened = line.matches('{').count();
    let closed = line.matches('}').count();
    (depth + opened).saturating_sub(closed)
}

/// The directory name after the first `steamapps/common/` in a normalized path.
fn game_dir_in(normalized: &str) -> Option<&str> {
    let lower = normalized.to_ascii_lowercase();
    let at = lower
        .match_indices(COMMON_MARKER)
        .map(|(at, _)| at)
        .find(|&at| is_path_start(&lower, at))?;
    let start = at + COMMON_MARKER.len();
    normalized
        .get(start..)?
        .split('/')
        .next()
        .filter(|dir| !dir.is_empty())
}

fn is_path_start(path: &str, at: usize) -> bool {
    at == 0 || path.as_bytes()[at - 1] == b'/'
}

/// Turns a folder name such as `Some_Unlisted-Game` into `Some Unlisted Game`.
fn prettify(dir: &str) -> String {
    dir.replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_runtime_tool(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    RUNTIME_TOOL_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// A two-manifest Steam library plus a second library folder, for tests.
#[cfg(test)]
pub(crate) fn test_library() -> (tempfile::TempDir, SteamLibrary) {
    let dir = tempfile::tempdir().expect("temp dir");
    let main = dir.path().join("main").join("steamapps");
    let second = dir.path().join("second").join("steamapps");
    write_manifest(
        &main,
        "appmanifest_730.acf",
        &appmanifest("730", "Counter-Strike 2", "Counter-Strike Global Offensive"),
    );
    write_manifest(
        &main,
        "appmanifest_2805730.acf",
        &appmanifest("2805730", "Proton 9.0 (Beta)", "Proton 9.0 (Beta)"),
    );
    write_manifest(
        &second,
        "appmanifest_1245620.acf",
        &appmanifest("1245620", "ELDEN RING", "ELDEN RING"),
    );
    (dir, SteamLibrary::with_roots(vec![main, second]))
}

#[cfg(test)]
fn write_manifest(steamapps: &Path, file: &str, text: &str) {
    fs::create_dir_all(steamapps).expect("steamapps dir");
    fs::write(steamapps.join(file), text).expect("manifest");
}

#[cfg(test)]
fn appmanifest(app_id: &str, name: &str, install_dir: &str) -> String {
    format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{app_id}\"\n\t\"name\"\t\t\"{name}\"\n\t\"installdir\"\t\t\"{install_dir}\"\n\t\"UserConfig\"\n\t{{\n\t\t\"name\"\t\t\"ignored\"\n\t}}\n}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_native_game_by_install_folder() {
        let (_dir, library) = test_library();
        assert_eq!(
            library.name_for_path("/home/u/.local/share/Steam/steamapps/common/Counter-Strike Global Offensive/game/cs2"),
            Some("Counter-Strike 2".to_string())
        );
    }

    #[test]
    fn resolves_windows_path_from_second_library_root() {
        let (_dir, library) = test_library();
        assert_eq!(
            library
                .name_for_path(r"Z:\home\u\second\steamapps\common\ELDEN RING\Game\eldenring.exe"),
            Some("ELDEN RING".to_string())
        );
    }

    #[test]
    fn steamapps_and_folder_lookups_ignore_case() {
        let (_dir, library) = test_library();
        assert_eq!(
            library.name_for_path("/x/STEAMAPPS/Common/elden ring/eldenring.exe"),
            Some("ELDEN RING".to_string())
        );
        assert_eq!(
            library.name_for_path("/x/steamapps/common/counter-strike global offensive/cs2"),
            Some("Counter-Strike 2".to_string())
        );
    }

    #[test]
    fn unlisted_folder_is_prettified() {
        let (_dir, library) = test_library();
        assert_eq!(
            library.name_for_path("/x/steamapps/common/Some_Unlisted-Game/Game.exe"),
            Some("Some Unlisted Game".to_string())
        );
    }

    #[test]
    fn proton_and_runtime_folders_are_not_games() {
        let (_dir, library) = test_library();
        assert_eq!(
            library.name_for_path("/x/steamapps/common/Proton 9.0 (Beta)/files/bin/wine64"),
            None
        );
        assert_eq!(
            library.name_for_path("/x/steamapps/common/Proton Experimental/proton"),
            None
        );
        assert_eq!(
            library.name_for_path("/x/steamapps/common/SteamLinuxRuntime_sniper/pressure-vessel"),
            None
        );
        assert_eq!(
            library.name_for_path("/x/steamapps/common/Steam Linux Runtime 3.0/run"),
            None
        );
        assert_eq!(
            library.name_for_path("/x/steamapps/common/Steamworks Common Redistributables/bin"),
            None
        );
    }

    #[test]
    fn paths_outside_common_resolve_to_nothing() {
        let (_dir, library) = test_library();
        assert_eq!(library.name_for_path("/usr/bin/vlc"), None);
        assert_eq!(
            library.name_for_path("/x/mysteamapps/common/Game/Game.exe"),
            None
        );
        assert_eq!(library.name_for_path("/x/steamapps/common/"), None);
    }

    #[test]
    fn app_ids_resolve_from_both_roots_and_skip_tools() {
        let (_dir, library) = test_library();
        assert_eq!(
            library.name_for_app_id("730"),
            Some("Counter-Strike 2".to_string())
        );
        assert_eq!(
            library.name_for_app_id(" 1245620 "),
            Some("ELDEN RING".to_string())
        );
        assert_eq!(library.name_for_app_id("2805730"), None);
        assert_eq!(library.name_for_app_id("999999"), None);
    }

    #[test]
    fn nested_user_config_name_is_not_used() {
        let fields = ManifestFields::parse(&appmanifest("730", "Real Name", "Dir"));
        assert_eq!(fields.name.as_deref(), Some("Real Name"));
        assert_eq!(fields.install_dir.as_deref(), Some("Dir"));
    }

    #[test]
    fn library_folders_list_steamapps_under_each_path() {
        let text = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/u/.local/share/Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/mnt/games/SteamLibrary\"\n\t}\n}\n";
        assert_eq!(
            library_paths(text),
            vec![
                PathBuf::from("/home/u/.local/share/Steam/steamapps"),
                PathBuf::from("/mnt/games/SteamLibrary/steamapps"),
            ]
        );
    }

    #[test]
    fn library_roots_reads_libraryfolders_vdf_from_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let steamapps = dir.path().join("steamapps");
        write_manifest(
            &steamapps,
            "libraryfolders.vdf",
            "\"libraryfolders\"\n{\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/mnt/games\"\n\t}\n}\n",
        );
        assert_eq!(
            library_roots(&steamapps),
            vec![PathBuf::from("/mnt/games/steamapps")]
        );
        assert!(library_roots(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn quoted_tokens_unescape_backslashes_and_quotes() {
        assert_eq!(
            quoted_tokens("\t\"path\"\t\t\"D:\\\\Lib\""),
            vec!["path".to_string(), "D:\\Lib".to_string()]
        );
        assert_eq!(
            quoted_tokens(r#""say \"hi\"""#),
            vec![r#"say "hi""#.to_string()]
        );
        assert!(quoted_tokens("{").is_empty());
        assert_eq!(quoted_tokens("\"unterminated"), Vec::<String>::new());
    }

    #[test]
    fn appmanifest_file_name_supplies_missing_app_id() {
        assert_eq!(
            app_id_from_file_name(Path::new("/a/appmanifest_440.acf")),
            Some("440".to_string())
        );
        assert_eq!(app_id_from_file_name(Path::new("/a/config.vdf")), None);
    }

    #[test]
    fn prettify_replaces_separators_only() {
        assert_eq!(
            prettify("Half-Life_2  Deathmatch"),
            "Half Life 2 Deathmatch"
        );
    }

    #[test]
    fn runtime_tool_prefixes_match_any_case() {
        assert!(is_runtime_tool("proton 9.0"));
        assert!(is_runtime_tool("STEAMLINUXRUNTIME_soldier"));
        assert!(!is_runtime_tool("Half-Life 2"));
    }

    #[test]
    fn discover_never_panics() {
        let _ = SteamLibrary::discover();
    }
}
