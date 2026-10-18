//! Friendly identities for processes: a hand-written user catalog and a learned cache.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::types::{Category, Identity, IdentitySource};

const APP_QUALIFIER: &str = "";
const APP_ORGANIZATION: &str = "";
const APP_NAME: &str = "gpulens";
const USER_FILE_NAME: &str = "catalog.toml";
const LEARNED_FILE_NAME: &str = "learned.json";
const LEARNED_VERSION: u32 = 1;
const NAME_LIMIT: usize = 40;
const DESCRIPTION_LIMIT: usize = 140;
const SECS_PER_DAY: u64 = 86_400;
/// A program the model could not identify is asked about again after this long (7 days).
const DECLINED_RETRY_SECS: u64 = 7 * SECS_PER_DAY;

/// Learned answers older than this are dropped by `prune_older_than` (90 days).
pub const LEARNED_MAX_AGE_SECS: u64 = 90 * SECS_PER_DAY;

/// The identities known from the user catalog and the learned cache, with any problems found while loading them.
#[derive(Debug)]
pub struct Catalog {
    user: Vec<UserEntry>,
    learned: BTreeMap<String, LearnedEntry>,
    learned_path: PathBuf,
    warnings: Vec<String>,
    dirty: bool,
}

#[derive(Debug, Deserialize)]
struct UserFile {
    #[serde(default)]
    app: Vec<RawUserEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUserEntry {
    #[serde(rename = "match")]
    pattern: String,
    name: String,
    category: String,
    #[serde(default)]
    description: String,
}

#[derive(Clone, Debug, PartialEq)]
struct UserEntry {
    pattern: String,
    prefix: bool,
    name: String,
    category: Category,
    description: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct LearnedFile {
    version: u32,
    entries: BTreeMap<String, LearnedEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LearnedEntry {
    name: String,
    category: Category,
    description: String,
    model: String,
    learned_at: u64,
    #[serde(default)]
    declined: bool,
}

impl Catalog {
    /// A catalog with nothing in it that never touches the disk until something is learned.
    #[cfg(test)]
    pub fn empty() -> Catalog {
        Catalog {
            user: Vec::new(),
            learned: BTreeMap::new(),
            learned_path: PathBuf::new(),
            warnings: Vec::new(),
            dirty: false,
        }
    }

    /// Never fails. Missing files are empty, a corrupt learned cache is moved aside, and every problem becomes a warning.
    pub fn load(user_path: &Path, learned_path: &Path) -> Catalog {
        let mut warnings = Vec::new();
        let user = load_user(user_path, &mut warnings);
        let learned = load_learned(learned_path, &mut warnings);
        Catalog {
            user,
            learned,
            learned_path: learned_path.to_path_buf(),
            warnings,
            dirty: false,
        }
    }

    /// `catalog.toml` in the platform config directory.
    pub fn default_user_path() -> PathBuf {
        project_dirs()
            .map(|dirs| dirs.config_dir().join(USER_FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(USER_FILE_NAME))
    }

    /// `learned.json` in the platform cache directory.
    pub fn default_learned_path() -> PathBuf {
        project_dirs()
            .map(|dirs| dirs.cache_dir().join(LEARNED_FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(LEARNED_FILE_NAME))
    }

    /// User entries win over learned answers. Both are keyed to a workload by the slug of the friendly name.
    pub fn lookup(&self, signature: &str) -> Option<Identity> {
        self.lookup_user(signature)
            .or_else(|| self.lookup_learned(signature))
    }

    /// Stores an answer from the local LLM. Ignored when the name has no letters or digits.
    pub fn record_learned(
        &mut self,
        signature: &str,
        name: &str,
        category: Category,
        description: &str,
        model: &str,
    ) {
        let name = clip(name.trim(), NAME_LIMIT);
        if slug(&name).is_empty() {
            return;
        }
        let entry = LearnedEntry {
            name,
            category,
            description: clip(description.trim(), DESCRIPTION_LIMIT),
            model: model.trim().to_string(),
            learned_at: now_secs(),
            declined: false,
        };
        self.learned.insert(signature.to_string(), entry);
        self.dirty = true;
    }

    /// Remembers that the model could not tell what a program is, so it is not asked again until `DECLINED_RETRY_SECS` has passed.
    pub fn record_declined(&mut self, signature: &str, model: &str) {
        if self.learned.get(signature).is_some_and(|e| !e.declined) {
            return;
        }
        let entry = LearnedEntry {
            name: String::new(),
            category: Category::Other,
            description: String::new(),
            model: model.trim().to_string(),
            learned_at: now_secs(),
            declined: true,
        };
        self.learned.insert(signature.to_string(), entry);
        self.dirty = true;
    }

    /// Whether the model recently gave up on this program.
    pub fn recently_declined(&self, signature: &str) -> bool {
        self.learned.get(signature).is_some_and(|entry| {
            entry.declined && now_secs().saturating_sub(entry.learned_at) < DECLINED_RETRY_SECS
        })
    }

    /// Writes the learned cache only when something changed since the last load or save.
    pub fn save_learned(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let file = LearnedFile {
            version: LEARNED_VERSION,
            entries: self.learned.clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
        write_atomically(&self.learned_path, &text)?;
        self.dirty = false;
        Ok(())
    }

    /// Drops learned answers whose `learned_at` is more than `max_age_secs` before `now_secs`.
    pub fn prune_older_than(&mut self, now_secs: u64, max_age_secs: u64) {
        let before = self.learned.len();
        self.learned
            .retain(|_, entry| now_secs.saturating_sub(entry.learned_at) <= max_age_secs);
        if self.learned.len() != before {
            self.dirty = true;
        }
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn learned_len(&self) -> usize {
        self.learned.len()
    }

    pub fn user_len(&self) -> usize {
        self.user.len()
    }

    fn lookup_user(&self, signature: &str) -> Option<Identity> {
        let lowered = signature.to_lowercase();
        self.user
            .iter()
            .find(|entry| entry.matches(&lowered))
            .map(|entry| Identity {
                key: app_key(&entry.name),
                name: entry.name.clone(),
                category: entry.category,
                description: entry.description.clone(),
                source: IdentitySource::Catalog,
            })
    }

    fn lookup_learned(&self, signature: &str) -> Option<Identity> {
        self.learned
            .get(signature)
            .filter(|entry| !entry.declined)
            .map(|entry| Identity {
                key: app_key(&entry.name),
                name: entry.name.clone(),
                category: entry.category,
                description: entry.description.clone(),
                source: IdentitySource::Learned,
            })
    }
}

impl UserEntry {
    /// `lowered_signature` must already be lowercased; the pattern was lowercased when it was parsed.
    fn matches(&self, lowered_signature: &str) -> bool {
        if self.prefix {
            lowered_signature.starts_with(&self.pattern)
        } else {
            lowered_signature == self.pattern
        }
    }
}

/// Lowercase words joined by single dashes, with every other character treated as a separator.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn app_key(name: &str) -> String {
    format!("app:{}", slug(name))
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Cuts on a character boundary, never inside a multi-byte character, and leaves no trailing space.
fn clip(text: &str, limit: usize) -> String {
    text.chars()
        .take(limit)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn load_user(path: &Path, warnings: &mut Vec<String>) -> Vec<UserEntry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            warnings.push(format!("cannot read {}: {err}", path.display()));
            return Vec::new();
        }
    };
    let file: UserFile = match toml::from_str(&text) {
        Ok(file) => file,
        Err(err) => {
            warnings.push(format!("invalid catalog {}: {err}", path.display()));
            return Vec::new();
        }
    };
    file.app
        .into_iter()
        .filter_map(|raw| match parse_user_entry(raw, path) {
            Ok(entry) => Some(entry),
            Err(warning) => {
                warnings.push(warning);
                None
            }
        })
        .collect()
}

/// Only the offending entry is skipped, and the warning names the file, the entry and the problem.
fn parse_user_entry(raw: RawUserEntry, path: &Path) -> Result<UserEntry, String> {
    let name = raw.name.trim().to_string();
    let skip = |reason: String| format!("{}: skipping \"{}\": {reason}", path.display(), name);
    let category = Category::from_label(&raw.category)
        .ok_or_else(|| skip(format!("unknown category \"{}\"", raw.category)))?;
    let (pattern, prefix) = split_match(&raw.pattern);
    if pattern.is_empty() {
        return Err(skip("match is empty".to_string()));
    }
    if slug(&name).is_empty() {
        return Err(skip("name has no letters or digits".to_string()));
    }
    Ok(UserEntry {
        pattern,
        prefix,
        name,
        category,
        description: raw.description.trim().to_string(),
    })
}

/// A trailing `*` makes the pattern a prefix match. Matching is case-insensitive, so both forms are lowercased here.
fn split_match(raw: &str) -> (String, bool) {
    let lowered = raw.trim().to_lowercase();
    match lowered.strip_suffix('*') {
        Some(prefix) => (prefix.to_string(), true),
        None => (lowered, false),
    }
}

fn load_learned(path: &Path, warnings: &mut Vec<String>) -> BTreeMap<String, LearnedEntry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => {
            warnings.push(format!("cannot read {}: {err}", path.display()));
            return BTreeMap::new();
        }
    };
    match parse_learned(&text) {
        Ok(entries) => entries,
        Err(reason) => {
            warnings.push(quarantine(path, &reason));
            BTreeMap::new()
        }
    }
}

fn parse_learned(text: &str) -> Result<BTreeMap<String, LearnedEntry>, String> {
    let file: LearnedFile = serde_json::from_str(text).map_err(|err| err.to_string())?;
    if file.version != LEARNED_VERSION {
        return Err(format!("unsupported version {}", file.version));
    }
    Ok(file.entries)
}

/// Moves an unreadable cache to `<name>.corrupt` so the next save starts clean without destroying the evidence.
fn quarantine(path: &Path, reason: &str) -> String {
    let target = corrupt_path(path);
    match fs::rename(path, &target) {
        Ok(()) => format!(
            "learned cache {} is unreadable ({reason}); moved it to {} and starting empty",
            path.display(),
            target.display()
        ),
        Err(err) => format!(
            "learned cache {} is unreadable ({reason}) and could not be moved aside ({err}); starting empty",
            path.display()
        ),
    }
}

fn corrupt_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(LEARNED_FILE_NAME);
    path.with_file_name(format!("{name}.corrupt"))
}

/// The temp name sits next to the target so the final rename never crosses filesystems.
fn write_atomically(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let temp = temp_path_for(path);
    let outcome = write_file(&temp, text).and_then(|()| fs::rename(&temp, path));
    if outcome.is_err() {
        let _ = fs::remove_file(&temp);
    }
    outcome
}

fn temp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(LEARNED_FILE_NAME);
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    const DAY: u64 = 86_400;
    const NOW: u64 = 2_000_000_000;

    const COMFY_CATALOG: &str = r#"
[[app]]
match = "python3 ComfyUI/main.py"
name = "ComfyUI"
category = "ai"
description = "Image generation."

[[app]]
match = "Steam*"
name = "Steam"
category = "game"
"#;

    fn sandbox() -> (TempDir, PathBuf, PathBuf) {
        let dir = tempdir().unwrap();
        let user = dir.path().join("catalog.toml");
        let learned = dir.path().join("cache").join("learned.json");
        (dir, user, learned)
    }

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn learned_json(name: &str, learned_at: u64) -> serde_json::Value {
        json!({"name": name, "category": "ai", "description": "", "model": "m", "learned_at": learned_at})
    }

    #[test]
    fn a_declined_program_is_not_asked_again_and_survives_a_restart() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_declined("mystery --serve", "m");
        catalog.save_learned().unwrap();
        let reloaded = Catalog::load(&user, &learned);
        assert!(reloaded.recently_declined("mystery --serve"));
        assert!(reloaded.lookup("mystery --serve").is_none());
        assert!(!reloaded.recently_declined("other"));
    }

    #[test]
    fn a_decline_is_retried_after_a_week_and_never_hides_a_real_answer() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned("known", "Known", Category::Ai, "Does things.", "m");
        catalog.record_declined("known", "m");
        assert_eq!(catalog.lookup("known").unwrap().name, "Known");
        catalog.record_declined("old", "m");
        catalog.learned.get_mut("old").unwrap().learned_at = now_secs() - DECLINED_RETRY_SECS - 1;
        assert!(!catalog.recently_declined("old"));
    }

    #[test]
    fn user_entries_match_exactly_case_insensitively_and_by_prefix() {
        let (_dir, user, learned) = sandbox();
        put(&user, COMFY_CATALOG);
        let catalog = Catalog::load(&user, &learned);
        let exact = catalog.lookup("python3 ComfyUI/main.py").unwrap();
        assert_eq!(exact.name, "ComfyUI");
        assert_eq!(exact.category, Category::Ai);
        assert_eq!(exact.description, "Image generation.");
        assert_eq!(exact.source, IdentitySource::Catalog);
        assert_eq!(exact.key, "app:comfyui");
        assert_eq!(
            catalog.lookup("PYTHON3 comfyui/MAIN.py").unwrap().name,
            "ComfyUI"
        );
        assert!(
            catalog
                .lookup("python3 ComfyUI/main.py --port 8188")
                .is_none()
        );
        assert_eq!(catalog.lookup("steamwebhelper").unwrap().name, "Steam");
        assert_eq!(
            catalog.lookup("STEAMWEBHELPER").unwrap().category,
            Category::Game
        );
        assert!(catalog.lookup("python3 ComfyUI/other.py").is_none());
        assert!(catalog.warnings().is_empty());
    }

    #[test]
    fn first_matching_user_entry_wins() {
        let (_dir, user, learned) = sandbox();
        put(
            &user,
            r#"
[[app]]
match = "python3*"
name = "Python"
category = "dev"

[[app]]
match = "python3 ComfyUI/main.py"
name = "ComfyUI"
category = "ai"
"#,
        );
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(
            catalog.lookup("python3 ComfyUI/main.py").unwrap().name,
            "Python"
        );
        assert_eq!(catalog.user_len(), 2);
    }

    #[test]
    fn user_entry_wins_over_learned_answer() {
        let (_dir, user, learned) = sandbox();
        put(&user, COMFY_CATALOG);
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned(
            "python3 ComfyUI/main.py",
            "Guess",
            Category::Dev,
            "",
            "qwen3-4b",
        );
        catalog.record_learned(
            "mystery-tool",
            "Mystery Tool",
            Category::Other,
            "Unknown.",
            "qwen3-4b",
        );
        let user_answer = catalog.lookup("python3 ComfyUI/main.py").unwrap();
        assert_eq!(user_answer.name, "ComfyUI");
        assert_eq!(user_answer.source, IdentitySource::Catalog);
        let learned_answer = catalog.lookup("mystery-tool").unwrap();
        assert_eq!(learned_answer.name, "Mystery Tool");
        assert_eq!(learned_answer.source, IdentitySource::Learned);
        assert_eq!(learned_answer.key, "app:mystery-tool");
    }

    #[test]
    fn unknown_category_is_skipped_and_reported() {
        let (_dir, user, learned) = sandbox();
        put(
            &user,
            r#"
[[app]]
match = "weird"
name = "Weird"
category = "spaceship"

[[app]]
match = "fine"
name = "Fine"
category = "Dev"
"#,
        );
        let catalog = Catalog::load(&user, &learned);
        assert!(catalog.lookup("weird").is_none());
        assert_eq!(catalog.lookup("fine").unwrap().category, Category::Dev);
        assert_eq!(catalog.user_len(), 1);
        assert_eq!(catalog.warnings().len(), 1);
        assert!(
            catalog.warnings()[0].contains("spaceship"),
            "{:?}",
            catalog.warnings()
        );
        assert!(
            catalog.warnings()[0].contains("Weird"),
            "{:?}",
            catalog.warnings()
        );
    }

    #[test]
    fn empty_match_pattern_is_skipped_so_a_bare_star_cannot_match_everything() {
        let (_dir, user, learned) = sandbox();
        put(
            &user,
            "[[app]]\nmatch = \"*\"\nname = \"Everything\"\ncategory = \"other\"\n",
        );
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.user_len(), 0);
        assert!(catalog.lookup("anything at all").is_none());
        assert!(catalog.warnings()[0].contains("match is empty"));
    }

    #[test]
    fn bad_user_toml_yields_empty_list_and_a_warning_with_the_parse_error() {
        let (_dir, user, learned) = sandbox();
        put(&user, "[[app\nmatch = ");
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.user_len(), 0);
        assert_eq!(catalog.warnings().len(), 1);
        assert!(
            catalog.warnings()[0].starts_with("invalid catalog"),
            "{:?}",
            catalog.warnings()
        );
    }

    #[test]
    fn missing_files_load_empty_without_warnings() {
        let (_dir, user, learned) = sandbox();
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.user_len(), 0);
        assert_eq!(catalog.learned_len(), 0);
        assert!(catalog.warnings().is_empty());
        assert!(catalog.lookup("anything").is_none());
    }

    #[test]
    fn corrupt_learned_json_is_quarantined_and_starts_empty() {
        let (_dir, user, learned) = sandbox();
        put(&learned, "{not json");
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.learned_len(), 0);
        assert!(!learned.exists());
        assert!(learned.with_file_name("learned.json.corrupt").exists());
        assert_eq!(catalog.warnings().len(), 1);
        assert!(
            catalog.warnings()[0].contains("moved it to"),
            "{:?}",
            catalog.warnings()
        );
    }

    #[test]
    fn learned_file_with_unsupported_version_is_quarantined() {
        let (_dir, user, learned) = sandbox();
        put(&learned, &json!({"version": 9, "entries": {}}).to_string());
        let catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.learned_len(), 0);
        assert!(learned.with_file_name("learned.json.corrupt").exists());
        assert!(
            catalog.warnings()[0].contains("unsupported version 9"),
            "{:?}",
            catalog.warnings()
        );
    }

    #[test]
    fn learned_answers_round_trip_through_save_and_load() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned(
            "ffmpeg-helper",
            " Video Encoder ",
            Category::Media,
            "  Transcodes clips.  ",
            "qwen3-4b",
        );
        catalog.save_learned().unwrap();
        let text = fs::read_to_string(&learned).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert!(text.contains("\"category\": \"media\""), "{text}");
        let reloaded = Catalog::load(&user, &learned);
        assert_eq!(reloaded.learned_len(), 1);
        let identity = reloaded.lookup("ffmpeg-helper").unwrap();
        assert_eq!(identity.name, "Video Encoder");
        assert_eq!(identity.description, "Transcodes clips.");
        assert_eq!(identity.category, Category::Media);
        assert_eq!(identity.source, IdentitySource::Learned);
        assert_eq!(identity.key, "app:video-encoder");
        assert!(reloaded.warnings().is_empty());
    }

    #[test]
    fn save_learned_writes_nothing_when_not_dirty() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.save_learned().unwrap();
        assert!(!learned.exists());
        catalog.record_learned("sig", "Name", Category::Dev, "", "m");
        catalog.save_learned().unwrap();
        assert!(learned.exists());
        fs::remove_file(&learned).unwrap();
        catalog.save_learned().unwrap();
        assert!(!learned.exists());
    }

    #[test]
    fn save_learned_creates_parent_directories() {
        let (dir, user, _) = sandbox();
        let learned = dir.path().join("x").join("y").join("learned.json");
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned("sig", "Name", Category::Dev, "", "m");
        catalog.save_learned().unwrap();
        assert!(learned.is_file());
    }

    #[test]
    fn slug_lowercases_and_collapses_separators() {
        assert_eq!(slug("Counter-Strike 2"), "counter-strike-2");
        assert_eq!(slug("  vLLM  (GPU) "), "vllm-gpu");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("ComfyUI"), "comfyui");
    }

    #[test]
    fn different_signatures_with_one_friendly_name_share_a_key() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned("chrome --type=gpu", "Chrome", Category::Browser, "", "m");
        catalog.record_learned(
            "chrome --type=renderer",
            "chrome",
            Category::Browser,
            "",
            "m",
        );
        assert_eq!(
            catalog.lookup("chrome --type=gpu").unwrap().key,
            catalog.lookup("chrome --type=renderer").unwrap().key
        );
    }

    #[test]
    fn name_and_description_are_clipped_on_character_boundaries() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned("sig", &"é".repeat(50), Category::Dev, &"ü".repeat(200), "m");
        let identity = catalog.lookup("sig").unwrap();
        assert_eq!(identity.name, "é".repeat(40));
        assert_eq!(identity.description, "ü".repeat(140));
    }

    #[test]
    fn record_learned_trims_before_clipping_and_ignores_blank_names() {
        let (_dir, user, learned) = sandbox();
        let mut catalog = Catalog::load(&user, &learned);
        catalog.record_learned("blank", "   ", Category::Dev, "x", "m");
        assert!(catalog.lookup("blank").is_none());
        assert_eq!(catalog.learned_len(), 0);
        catalog.record_learned(
            "padded",
            &format!("   {}   ", "a".repeat(38)),
            Category::Dev,
            "",
            "m",
        );
        assert_eq!(catalog.lookup("padded").unwrap().name, "a".repeat(38));
        catalog.record_learned("symbols", "!!!", Category::Dev, "", "m");
        assert!(catalog.lookup("symbols").is_none());
    }

    #[test]
    fn prune_drops_answers_older_than_the_limit() {
        let (_dir, user, learned) = sandbox();
        put(
            &learned,
            &json!({
                "version": 1,
                "entries": {
                    "old": learned_json("Old", NOW - 91 * DAY),
                    "edge": learned_json("Edge", NOW - 90 * DAY),
                    "fresh": learned_json("Fresh", NOW - 89 * DAY),
                    "future": learned_json("Future", NOW + DAY)
                }
            })
            .to_string(),
        );
        let mut catalog = Catalog::load(&user, &learned);
        assert_eq!(catalog.learned_len(), 4);
        catalog.prune_older_than(NOW, LEARNED_MAX_AGE_SECS);
        assert_eq!(catalog.learned_len(), 3);
        assert!(catalog.lookup("old").is_none());
        assert!(catalog.lookup("edge").is_some());
        assert!(catalog.lookup("fresh").is_some());
        assert!(catalog.lookup("future").is_some());
        catalog.save_learned().unwrap();
        assert_eq!(Catalog::load(&user, &learned).learned_len(), 3);
    }

    #[test]
    fn prune_leaves_the_cache_untouched_when_nothing_is_old() {
        let (_dir, user, learned) = sandbox();
        let text = json!({
            "version": 1,
            "entries": {
                "fresh": learned_json("Fresh", NOW - DAY)
            }
        })
        .to_string();
        put(&learned, &text);
        let mut catalog = Catalog::load(&user, &learned);
        catalog.prune_older_than(NOW, LEARNED_MAX_AGE_SECS);
        catalog.save_learned().unwrap();
        assert_eq!(fs::read_to_string(&learned).unwrap(), text);
    }

    #[test]
    fn default_paths_name_the_expected_files() {
        assert!(Catalog::default_user_path().ends_with("catalog.toml"));
        assert!(Catalog::default_learned_path().ends_with("learned.json"));
    }
}
