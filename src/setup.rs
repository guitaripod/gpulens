//! `gpulens setup`: find a small local model, prove it works, let it name the programs on this GPU, and save the choice.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::ai::{
    AiSettings, ClassifyRequest, JobError, classify_with, resolve, scrub_command_line, scrub_path,
};
use crate::catalog::Catalog;
use crate::classify::Identifier;
use crate::cli::SetupArgs;
use crate::config::Config;
use crate::gpu::GpuBackend;
use crate::llm::client::ChatClient;
use crate::llm::discover::{self, Choice, Endpoint};
use crate::sampler::Sampler;
use crate::types::{IdentitySource, ProcMeta};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_PRELEARN: usize = 12;

pub fn run(
    args: &SetupArgs,
    config_path: &Path,
    mut config: Config,
    backend: Box<dyn GpuBackend>,
) -> Result<()> {
    println!("Looking for language-model servers on this machine…");
    let endpoints = match &args.url {
        Some(url) => discover::probe(url, DISCOVERY_TIMEOUT)
            .into_iter()
            .collect(),
        None => discover::discover(DISCOVERY_TIMEOUT),
    };
    if endpoints.is_empty() {
        bail!(
            "no language-model server answered. Start one (llama-server, llama-swap, Ollama or LM Studio) or pass --url. gpulens works without one; it just uses fixed wording."
        );
    }
    print_endpoints(&endpoints);
    let choice = discover::pick_model(&endpoints, args.model.as_deref())
        .context("none of the models found is suitable (embedding, reranking and thinking models are skipped)")?;
    announce(&choice);
    if !approved(&choice, args.yes)? {
        bail!("stopped without changing anything");
    }

    let client = ChatClient::new(&choice.base_url, &choice.model, None, PROBE_TIMEOUT);
    let latency = self_test(&client)?;
    println!(
        "  works: answered in {:.1} s with valid JSON",
        latency.as_secs_f64()
    );

    config.llm.enabled = true;
    config.llm.base_url = Some(choice.base_url.clone());
    config.llm.model = Some(choice.model.clone());
    let learned = prelearn(&client, &config, backend);
    report_learned(&learned);

    if args.dry_run {
        println!(
            "Dry run: nothing written. Would save to {}",
            config_path.display()
        );
        return Ok(());
    }
    config.save(config_path)?;
    println!("Saved {}", config_path.display());
    println!("Done. Run `gpulens` to see it working.");
    Ok(())
}

fn print_endpoints(endpoints: &[Endpoint]) {
    for endpoint in endpoints {
        println!("  {} ({:?})", endpoint.base_url, endpoint.flavor);
        for model in endpoint.models.iter().take(12) {
            let size = model.params_b.map_or(String::new(), |b| format!("{b}B"));
            let state = if model.loaded { "loaded" } else { "not loaded" };
            println!("    {:<40} {:>6}  {state}", model.id, size);
        }
        if endpoint.models.len() > 12 {
            println!("    … and {} more", endpoint.models.len() - 12);
        }
    }
}

fn announce(choice: &Choice) {
    println!("Chosen: {} on {}", choice.model, choice.base_url);
    if let Some(warning) = &choice.warning {
        println!("  note: {warning}");
    }
}

/// Unloaded models need a human decision because starting one can push other models off the GPU.
fn approved(choice: &Choice, assume_yes: bool) -> Result<bool> {
    if choice.loaded || assume_yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!(
            "{} is not loaded; rerun with --yes to allow starting it",
            choice.model
        );
    }
    print!("Start and use it? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// A tiny real request, so a server that cannot produce structured JSON is caught now rather than silently later. A cautious "I am not sure" answer still proves the plumbing works.
fn self_test(client: &ChatClient) -> Result<Duration> {
    let request = ClassifyRequest {
        signature: "llama-server".into(),
        executable: "/usr/bin/llama-server".into(),
        command_line: "llama-server -m model.gguf --port 8080".into(),
        container: None,
        unit: None,
    };
    let started = Instant::now();
    match classify_with(client, &request) {
        Ok(_) | Err(JobError::Rejected(_)) => Ok(started.elapsed()),
        Err(JobError::Failed(reason)) => bail!("the model did not give a usable answer: {reason}"),
    }
}

/// Names the unfamiliar programs on the GPU right now and remembers the answers.
fn prelearn(
    client: &ChatClient,
    config: &Config,
    backend: Box<dyn GpuBackend>,
) -> Vec<(String, String)> {
    let catalog = Arc::new(Mutex::new(Catalog::load(
        &Catalog::default_user_path(),
        &Catalog::default_learned_path(),
    )));
    let mut sampler = Sampler::new(backend, Identifier::new(Arc::clone(&catalog)), None);
    let Ok(observation) = sampler.poll() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let unknown: Vec<ProcMeta> = observation
        .processes
        .iter()
        .filter(|p| p.identity.source == IdentitySource::Fallback && p.meta.accessible)
        .filter(|p| seen.insert(p.meta.signature()))
        .map(|p| p.meta.clone())
        .take(MAX_PRELEARN)
        .collect();
    if unknown.is_empty() || !config.llm.classify {
        return Vec::new();
    }
    println!(
        "Asking the model about {} unfamiliar program(s) on your GPU…",
        unknown.len()
    );
    let mut learned = Vec::new();
    for meta in unknown {
        let request = ClassifyRequest {
            signature: meta.signature(),
            executable: scrub_path(
                &meta
                    .exe
                    .as_ref()
                    .map_or_else(|| meta.comm.clone(), |p| p.display().to_string()),
            ),
            command_line: scrub_command_line(&meta.cmdline),
            container: meta
                .container
                .as_ref()
                .map(|c| c.runtime.label().to_string()),
            unit: meta.unit.clone(),
        };
        if let Ok(identity) = classify_with(client, &request) {
            let mut guard = catalog.lock().unwrap_or_else(|p| p.into_inner());
            guard.record_learned(
                &request.signature,
                &identity.name,
                identity.category,
                &identity.description,
                client.model(),
            );
            learned.push((
                request.signature,
                format!(
                    "{} ({}): {}",
                    identity.name,
                    identity.category.label(),
                    identity.description
                ),
            ));
        }
    }
    let _ = catalog
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .save_learned();
    learned
}

fn report_learned(learned: &[(String, String)]) {
    for (signature, description) in learned {
        println!("  learned: {signature} → {description}");
    }
}

/// Checks the configured helper end to end, for `gpulens doctor`.
pub fn check_helper(settings: &AiSettings) -> (String, bool) {
    match resolve(settings) {
        Err(reason) => (reason, false),
        Ok((client, choice)) => match self_test(&client) {
            Ok(latency) => (
                format!(
                    "{} on {} answered in {:.1} s",
                    choice.model,
                    choice.base_url,
                    latency.as_secs_f64()
                ),
                true,
            ),
            Err(error) => (
                format!(
                    "{} on {} is reachable but failed the test: {error}",
                    choice.model, choice.base_url
                ),
                false,
            ),
        },
    }
}
