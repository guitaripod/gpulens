//! Command-line surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

#[derive(Debug, Parser)]
#[command(
    name = "gpulens",
    version,
    about = "See which apps are using your GPU, in plain language",
    long_about = "gpulens shows what is using your GPU right now and why, grouped by application and written in plain language. It reads NVIDIA GPUs through NVML and can use a small local language model to name unfamiliar programs and explain the situation.",
    after_help = "Run `gpulens setup` once to connect a local language model. Press ? inside the interface for help."
)]
pub struct Cli {
    #[command(flatten)]
    pub common: Common,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args, Clone)]
pub struct Common {
    /// Sampling interval in milliseconds (250 to 10000)
    #[arg(short, long, global = true, value_name = "MS")]
    pub interval: Option<u64>,

    /// Do not use the language-model helper
    #[arg(long, global = true)]
    pub no_ai: bool,

    /// Use a scripted fake GPU instead of real hardware
    #[arg(long, global = true)]
    pub demo: bool,

    /// Draw with plain ASCII instead of Unicode block characters
    #[arg(long, global = true)]
    pub ascii: bool,

    /// Draw without colour (also honoured: the NO_COLOR variable)
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Use this config file instead of the default location
    #[arg(long, global = true, value_name = "PATH", env = "GPULENS_CONFIG")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the interactive interface (the default)
    Tui,
    /// Print one report and exit; the default when output is not a terminal
    Snapshot(SnapshotArgs),
    /// Find a local language model and configure gpulens to use it
    Setup(SetupArgs),
    /// Check that everything gpulens relies on works
    Doctor,
    /// Print a shell completion script
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,

    /// How many readings to take before reporting (utilisation needs at least two)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=60))]
    pub samples: u32,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Use this model instead of the best small one found
    #[arg(long)]
    pub model: Option<String>,

    /// Use this server instead of searching (for example http://127.0.0.1:8081)
    #[arg(long, value_name = "URL")]
    pub url: Option<String>,

    /// Accept the choice without asking, even if the model is not loaded yet
    #[arg(short, long)]
    pub yes: bool,

    /// Show what would be configured without writing anything
    #[arg(long)]
    pub dry_run: bool,
}
