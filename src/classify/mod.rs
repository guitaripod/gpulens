//! "What is this process?": the user's catalog first, then built-in rules, then anything the language model has learned.

pub mod rules;
pub mod steam;

use std::sync::{Arc, Mutex};

use crate::catalog::Catalog;
use crate::types::{Category, Identity, IdentitySource, ProcMeta};

pub struct Identifier {
    steam: steam::SteamLibrary,
    catalog: Arc<Mutex<Catalog>>,
}

impl Identifier {
    pub fn new(catalog: Arc<Mutex<Catalog>>) -> Self {
        Self {
            steam: steam::SteamLibrary::discover(),
            catalog,
        }
    }

    /// An identifier with an empty catalog and no Steam libraries, for tests and one-shot tools.
    #[cfg(test)]
    pub fn without_catalog() -> Self {
        Self {
            steam: steam::SteamLibrary::with_roots(Vec::new()),
            catalog: Arc::new(Mutex::new(Catalog::empty())),
        }
    }

    /// Never fails: the worst case is the program's own name.
    pub fn identify(&self, meta: &ProcMeta) -> Identity {
        if !meta.accessible || meta.exe_name().is_empty() {
            return hidden(meta);
        }
        let known = self
            .catalog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lookup(&meta.signature());
        if let Some(identity) = known
            .as_ref()
            .filter(|i| i.source == IdentitySource::Catalog)
        {
            return identity.clone();
        }
        if let Some(identity) = rules::classify(meta, &self.steam) {
            return name_served_model(identity, meta);
        }
        known.unwrap_or_else(|| Identity::fallback(meta))
    }
}

/// Several llama.cpp servers usually run side by side, so each is named after the model file it serves.
fn name_served_model(identity: Identity, meta: &ProcMeta) -> Identity {
    if !identity.key.starts_with("llama-server") {
        return identity;
    }
    let Some(model) = served_model_name(&meta.cmdline) else {
        return identity;
    };
    Identity {
        key: format!("{}:{}", identity.key, model.to_lowercase()),
        name: format!("{} · {model}", identity.name),
        ..identity
    }
}

/// The file stem of the `-m` or `--model` argument, without a `.gguf` extension and capped in length.
fn served_model_name(cmdline: &[String]) -> Option<String> {
    let position = cmdline.iter().position(|a| a == "-m" || a == "--model")?;
    let path = cmdline.get(position + 1)?;
    let file = path.rsplit('/').next()?;
    let stem = strip_quantisation(file.strip_suffix(".gguf").unwrap_or(file));
    (!stem.is_empty()).then(|| stem.chars().take(28).collect())
}

/// Drops a trailing quantisation tag such as `-Q4_K_M`, `-q8_0` or `-f16`, which says how the model was compressed rather than what it is.
fn strip_quantisation(stem: &str) -> &str {
    let lower = stem.to_ascii_lowercase();
    let tag_start = lower
        .rmatch_indices(['-', '_', '.'])
        .find_map(|(index, _)| {
            let tag = &lower[index + 1..];
            let quant = tag.starts_with('q') && tag[1..].starts_with(|c: char| c.is_ascii_digit())
                || tag.starts_with("iq") && tag[2..].starts_with(|c: char| c.is_ascii_digit())
                || matches!(tag, "f16" | "f32" | "bf16" | "fp16");
            quant.then_some(index)
        });
    match tag_start {
        Some(index) if index > 0 => &stem[..index],
        _ => stem,
    }
}

/// A process whose `/proc` entry cannot be read, usually because another user or a container owns it.
fn hidden(meta: &ProcMeta) -> Identity {
    Identity {
        key: format!("pid:{}", meta.pid),
        name: format!("process {}", meta.pid),
        category: Category::Other,
        description: "Details are hidden: it belongs to another user or container.".into(),
        source: IdentitySource::Fallback,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn meta(exe: &str, args: &[&str]) -> ProcMeta {
        ProcMeta {
            pid: 7,
            accessible: true,
            comm: exe.into(),
            exe: Some(PathBuf::from(format!("/usr/bin/{exe}"))),
            cmdline: args.iter().map(|s| s.to_string()).collect(),
            ..ProcMeta::default()
        }
    }

    #[test]
    fn rules_name_known_programs() {
        let identity =
            Identifier::without_catalog().identify(&meta("llama-server", &["llama-server"]));
        assert_eq!(identity.category, Category::Ai);
        assert_eq!(identity.source, IdentitySource::Builtin);
    }

    #[test]
    fn llama_servers_are_told_apart_by_their_model_file() {
        let identifier = Identifier::without_catalog();
        let a = identifier.identify(&meta(
            "llama-server",
            &[
                "llama-server",
                "-m",
                "/models/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
                "--port",
                "1",
            ],
        ));
        let b = identifier.identify(&meta(
            "llama-server",
            &["llama-server", "--model", "/models/other.gguf"],
        ));
        assert_eq!(a.name, "llama.cpp server · Qwen3-4B-Instruct-2507");
        assert_eq!(b.name, "llama.cpp server · other");
        assert_ne!(a.key, b.key);
        assert_eq!(a.category, Category::Ai);
    }

    #[test]
    fn quantisation_tags_are_dropped_from_model_names() {
        assert_eq!(
            strip_quantisation("Qwen3-4B-Instruct-2507-Q4_K_M"),
            "Qwen3-4B-Instruct-2507"
        );
        assert_eq!(strip_quantisation("unrager-4b-q8_0"), "unrager-4b");
        assert_eq!(strip_quantisation("llama-3.1-8b-f16"), "llama-3.1-8b");
        assert_eq!(strip_quantisation("mistral-7b-iq4_xs"), "mistral-7b");
        assert_eq!(strip_quantisation("plain-model"), "plain-model");
        assert_eq!(strip_quantisation("q4-experiment"), "q4-experiment");
    }

    #[test]
    fn llama_server_without_a_model_argument_keeps_the_plain_name() {
        let identity =
            Identifier::without_catalog().identify(&meta("llama-server", &["llama-server"]));
        assert_eq!(identity.name, "llama.cpp server");
    }

    #[test]
    fn unknown_programs_fall_back_to_their_name() {
        let identity =
            Identifier::without_catalog().identify(&meta("mystery-tool", &["mystery-tool"]));
        assert_eq!(identity.name, "mystery-tool");
        assert_eq!(identity.source, IdentitySource::Fallback);
    }

    #[test]
    fn inaccessible_processes_get_a_pid_identity() {
        let hidden_meta = ProcMeta {
            pid: 99,
            ..ProcMeta::default()
        };
        let identity = Identifier::without_catalog().identify(&hidden_meta);
        assert_eq!(identity.key, "pid:99");
    }

    #[test]
    fn learned_names_fill_gaps_but_never_override_rules() {
        let catalog = Arc::new(Mutex::new(Catalog::empty()));
        catalog.lock().unwrap().record_learned(
            "mystery-tool",
            "Mystery Tool",
            Category::Creative,
            "Does art.",
            "m",
        );
        catalog.lock().unwrap().record_learned(
            "llama-server",
            "Wrong",
            Category::Game,
            "Wrong.",
            "m",
        );
        let identifier = Identifier {
            steam: steam::SteamLibrary::with_roots(Vec::new()),
            catalog,
        };
        let learned = identifier.identify(&meta("mystery-tool", &["mystery-tool"]));
        assert_eq!(
            (learned.name.as_str(), learned.source),
            ("Mystery Tool", IdentitySource::Learned)
        );
        let ruled = identifier.identify(&meta("llama-server", &["llama-server"]));
        assert_eq!(ruled.source, IdentitySource::Builtin);
        assert_ne!(ruled.name, "Wrong");
    }
}
