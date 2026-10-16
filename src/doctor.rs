//! `gpulens doctor`: a plain checklist of everything gpulens depends on.

use std::path::Path;

use crate::ai::AiSettings;
use crate::catalog::Catalog;
use crate::classify::steam::SteamLibrary;
use crate::config::Config;
use crate::gpu;
use crate::procinfo::ProcReader;
use crate::setup::check_helper;

struct Report {
    problems: usize,
}

impl Report {
    fn ok(&self, text: String) {
        println!("[ ok ] {text}");
    }

    fn warn(&mut self, text: String) {
        self.problems += 1;
        println!("[warn] {text}");
    }

    fn info(&self, text: String) {
        println!("[ .. ] {text}");
    }
}

pub fn run(config_path: &Path, config: &Config, demo: bool, skip_ai: bool) -> bool {
    let mut report = Report { problems: 0 };
    println!("gpulens {}", env!("CARGO_PKG_VERSION"));
    check_gpu(&mut report, demo);
    check_config(&mut report, config_path, config);
    check_catalog(&mut report);
    report.info(format!(
        "Steam libraries found: {}",
        SteamLibrary::discover().root_count()
    ));
    if skip_ai || !config.llm.enabled {
        report.info("AI helper: switched off".into());
    } else {
        let settings = AiSettings::from_config(config);
        let (text, healthy) = check_helper(&settings);
        if settings.model.is_none() {
            report.info(format!("AI helper: {text}"));
        } else if healthy {
            report.ok(format!("AI helper: {text}"));
            preview_narration(&mut report, &settings, demo);
        } else {
            report.warn(format!("AI helper: {text}. gpulens still works with fixed wording; `gpulens setup` can fix this."));
        }
    }
    println!();
    if report.problems == 0 {
        println!("Everything looks good.");
    } else {
        println!("{} thing(s) need attention.", report.problems);
    }
    report.problems == 0
}

fn check_gpu(report: &mut Report, demo: bool) {
    let mut backend = match gpu::open(demo) {
        Ok(backend) => backend,
        Err(error) => {
            report.warn(format!("GPU access: {error}"));
            return;
        }
    };
    for info in backend.devices() {
        report.ok(format!(
            "GPU {}: {} · driver {} · {} of video memory",
            info.index,
            info.name,
            info.driver_version,
            crate::format::bytes(info.memory_total)
        ));
    }
    match backend.sample() {
        Err(error) => report.warn(format!("Reading the GPU failed: {error}")),
        Ok(sample) => check_processes(
            report,
            &sample.processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
            demo,
        ),
    }
}

fn check_processes(report: &mut Report, pids: &[u32], demo: bool) {
    if demo {
        report.info(format!("{} demo processes", pids.len()));
        return;
    }
    let mut reader = ProcReader::new();
    let hidden = pids
        .iter()
        .filter(|pid| !reader.read(**pid).accessible)
        .count();
    if pids.is_empty() {
        report.info("No process is using the GPU right now".into());
    } else if hidden == 0 {
        report.ok(format!(
            "All {} GPU processes are visible in /proc",
            pids.len()
        ));
    } else {
        report.warn(format!(
            "{hidden} of {} GPU processes are not visible in /proc (another container or PID namespace; try --pid=host)",
            pids.len()
        ));
    }
}

fn check_config(report: &mut Report, path: &Path, config: &Config) {
    let state = if path.exists() {
        "found"
    } else {
        "not created yet, using defaults"
    };
    report.ok(format!(
        "Config {}: {state}; sampling every {} ms",
        path.display(),
        config.interval_ms
    ));
}

fn check_catalog(report: &mut Report) {
    let catalog = Catalog::load(
        &Catalog::default_user_path(),
        &Catalog::default_learned_path(),
    );
    report.info(format!(
        "Catalog: {} of your own entries, {} learned by the AI helper",
        catalog.user_len(),
        catalog.learned_len()
    ));
    for warning in catalog.warnings() {
        report.warn(format!("Catalog: {warning}"));
    }
}

/// Shows what the helper would say about the GPU right now, so a bad rewrite is visible without opening the interface.
fn preview_narration(report: &mut Report, settings: &AiSettings, demo: bool) {
    let Ok((client, _)) = crate::ai::resolve(settings) else {
        return;
    };
    let Ok(backend) = gpu::open(demo) else { return };
    let infos = backend.devices().to_vec();
    let identifier = crate::classify::Identifier::new(std::sync::Arc::new(std::sync::Mutex::new(
        Catalog::load(
            &Catalog::default_user_path(),
            &Catalog::default_learned_path(),
        ),
    )));
    let mut sampler = crate::sampler::Sampler::new(backend, identifier, None);
    let Ok(model) =
        crate::snapshot::collect(&mut sampler, infos, 2, std::time::Duration::from_secs(1))
    else {
        return;
    };
    let device = &model.devices[0];
    let story = crate::narrate::story(device, model.interval);
    let Some(facts) = crate::narrate::facts(device, &story) else {
        return;
    };
    match crate::ai::narrate_with(&client, &facts) {
        Ok((headline, tip)) => {
            report.ok(format!("AI summary: {headline}"));
            if let Some(tip) = tip {
                report.info(format!("AI tip: {tip}"));
            }
        }
        Err(error) => report.warn(format!("AI summary was not usable: {error}")),
    }
}
