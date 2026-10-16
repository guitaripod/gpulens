mod actions;
mod ai;
mod app;
mod catalog;
mod classify;
mod cli;
mod config;
mod doctor;
mod event;
mod format;
mod gpu;
mod llm;
mod model;
mod narrate;
mod procinfo;
mod sampler;
mod setup;
mod smooth;
mod snapshot;
mod terminal;
mod types;
mod ui;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use crate::ai::AiSettings;
use crate::app::{App, Command as AppCommand};
use crate::catalog::Catalog;
use crate::classify::Identifier;
use crate::cli::{Cli, Command, Common};
use crate::config::Config;
use crate::event::{Event, Events};
use crate::model::{Model, Tuning};
use crate::sampler::Sampler;
use crate::ui::theme::Theme;

const FRAME_WAIT: Duration = Duration::from_millis(250);
const ANIMATION_WAIT: Duration = Duration::from_millis(33);
const EVENTS_PER_FRAME: usize = 256;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("gpulens: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let config_path = cli
        .common
        .config
        .clone()
        .unwrap_or_else(Config::default_path);
    let mut config = Config::load(&config_path)?;
    apply_overrides(&mut config, &cli.common)?;
    match cli.command {
        Some(Command::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "gpulens",
                &mut std::io::stdout(),
            );
        }
        Some(Command::Doctor) => {
            let healthy = doctor::run(&config_path, &config, cli.common.demo, cli.common.no_ai);
            return Ok(if healthy {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        Some(Command::Setup(args)) => {
            let backend = gpu::open(cli.common.demo)?;
            setup::run(&args, &config_path, config, backend)?;
        }
        Some(Command::Snapshot(args)) => {
            print_snapshot(&cli.common, &config, args.samples, args.json)?
        }
        Some(Command::Tui) | None => {
            if std::io::stdout().is_terminal() {
                run_tui(&cli.common, config)?;
            } else {
                print_snapshot(&cli.common, &config, 2, false)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn apply_overrides(config: &mut Config, common: &Common) -> Result<()> {
    if let Some(ms) = common.interval {
        config.interval_ms = ms;
    }
    if common.ascii {
        config.ascii = true;
    }
    if common.no_ai {
        config.llm.enabled = false;
    }
    config.validate()?;
    Ok(())
}

fn shared_catalog() -> Arc<Mutex<Catalog>> {
    let mut loaded = Catalog::load(
        &Catalog::default_user_path(),
        &Catalog::default_learned_path(),
    );
    loaded.prune_older_than(catalog::now_secs(), catalog::LEARNED_MAX_AGE_SECS);
    Arc::new(Mutex::new(loaded))
}

fn print_snapshot(common: &Common, config: &Config, samples: u32, json: bool) -> Result<()> {
    let backend = gpu::open(common.demo)?;
    let infos = backend.devices().to_vec();
    let mut sampler = Sampler::new(backend, Identifier::new(shared_catalog()), None);
    let interval = config.interval().max(Duration::from_millis(500));
    let model = snapshot::collect(&mut sampler, infos, samples, interval)?;
    if json {
        println!("{}", snapshot::render_json(&model)?);
    } else {
        print!("{}", snapshot::render_text(&model));
    }
    Ok(())
}

fn run_tui(common: &Common, config: Config) -> Result<()> {
    let backend = gpu::open(common.demo)?;
    let infos = backend.devices().to_vec();
    let events = Events::new();
    let catalog = shared_catalog();
    let catalog_warnings = catalog
        .lock()
        .map(|c| c.warnings().to_vec())
        .unwrap_or_default();

    let ai_handle = config.llm.enabled.then(|| {
        let sender = events.sender();
        ai::spawn(
            AiSettings::from_config(&config),
            Arc::clone(&catalog),
            Box::new(move |outcome| {
                let _ = sender.send(Event::Ai(outcome));
            }),
        )
    });

    let identifier = Identifier::new(Arc::clone(&catalog));
    let mut sampler = Sampler::new(backend, identifier, ai_handle.clone());
    let mut model = Model::new(infos, config.interval(), config.history())
        .with_tuning(Tuning::for_interface(config.calm, config.animate));
    if common.demo {
        prefill_demo(&mut model, &mut sampler, &config)?;
    }
    let sampler_handle = sampler::spawn(sampler, config.interval(), events.sender());
    let theme = Theme::detect(config.ascii, common.no_color);
    let mut app = App::new(model, theme, config, ai_handle);
    if let Some(warning) = catalog_warnings.first() {
        app.notify(
            format!("Catalog: {warning}"),
            narrate::Level::Warn,
            Instant::now(),
        );
    }

    let (mut terminal, _guard) = terminal::enter().context("starting the terminal interface")?;
    events.spawn_input();
    events.spawn_signals();

    while !app.quit {
        app.now = Instant::now();
        let animating = app.model.is_animating(app.now);
        if animating {
            app.dirty = true;
        }
        if app.dirty {
            terminal::draw_synchronized(&mut terminal, |frame| ui::draw(frame, &mut app))
                .context("drawing the interface")?;
            app.dirty = false;
        }
        let wait = if animating {
            ANIMATION_WAIT
        } else {
            FRAME_WAIT
        };
        if let Some(event) = events.next(wait) {
            app.handle(event, Instant::now());
            for _ in 0..EVENTS_PER_FRAME {
                match events.try_next() {
                    Some(next) => app.handle(next, Instant::now()),
                    None => break,
                }
            }
        }
        app.tick(Instant::now());
        for command in app.commands.drain(..) {
            match command {
                AppCommand::Refresh => sampler_handle.refresh(),
                AppCommand::SetInterval(interval) => sampler_handle.set_interval(interval),
            }
        }
    }
    events.shutdown();
    sampler_handle.stop();
    Ok(())
}

/// Gives the scripted GPU a past, so the history chart is full the moment the demo opens.
fn prefill_demo(model: &mut Model, sampler: &mut Sampler, config: &Config) -> Result<()> {
    let ticks = (config.window().as_secs_f64() / config.interval().as_secs_f64()).round() as usize;
    for _ in 0..ticks {
        model.ingest(sampler.poll()?);
    }
    Ok(())
}
