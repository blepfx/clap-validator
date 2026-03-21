use crate::commands::Verbosity;
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn parse_duration(str: &str) -> Result<Duration, String> {
    let suffixes = [("ms", 0.001), ("s", 1.0), ("m", 60.0), ("h", 3600.0)];

    for (suffix, multiplier) in suffixes {
        if let Some(num_str) = str.strip_suffix(suffix)
            && let Ok(num) = num_str.parse::<f64>()
        {
            return Ok(Duration::from_secs_f64(num * multiplier));
        }
    }

    Err(format!("Invalid duration format: {}", str))
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum FuzzerTag {
    ProcessOutOfPlace,
    ProcessInPlace,
    Process32,
    Process64,
}

/// Options for the fuzzer.
#[derive(Debug, Args)]
pub struct FuzzerSettings {
    /// Paths to one or more plugins that should be fuzzed.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Only fuzz plugins with this ID.
    ///
    /// If the plugin library contains multiple plugins, then you can pass a single plugin's ID
    /// to this option to only fuzz that plugin. Otherwise all plugins in the library are
    /// fuzzed.
    #[arg(short = 'p', long)]
    pub plugin_id: Option<String>,

    /// Print the test output as JSON instead of human readable text.
    #[arg(long)]
    pub json: bool,

    /// Run fuzzing for this long before stopping.
    /// The duration can be specified with a suffix of "ms" for milliseconds, "s" for seconds, "m" for minutes, or "h" for hours.
    #[arg(long, short = 'd', default_value = "60s", value_parser = parse_duration)]
    pub duration: Duration,

    /// The number of fuzzers to run in parallel.
    #[arg(long, short = 'j', default_value = "1")]
    pub jobs: usize,

    /// The tags to apply to the fuzzing process.
    #[arg(long, short = 't')]
    pub tags: Vec<FuzzerTag>,
}

pub fn fuzz(verbosity: Verbosity, settings: &FuzzerSettings) -> Result<ExitCode> {
    todo!()
}
