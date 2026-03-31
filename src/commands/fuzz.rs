use crate::commands::Verbosity;
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn parse_duration(mut str: &str) -> Result<Duration, &'static str> {
    if str.is_empty() {
        return Err("no duration provided");
    }

    let mut duration = Duration::from_secs(0);
    while !str.is_empty() {
        str = str.trim_ascii_start();

        let (num, rest) = str.split_at(str.find(|c: char| !c.is_ascii_digit()).unwrap_or(str.len()));
        let (unit, rest) = rest
            .trim_ascii_start()
            .split_at(rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len()));

        let num: u64 = num.parse::<u64>().map_err(|_| "invalid duration format")?;
        let unit = match unit {
            "ms" | "millis" => Duration::from_millis(num),
            "s" | "sec" | "seconds" => Duration::from_secs(num),
            "m" | "min" | "minutes" => Duration::from_secs(num * 60),
            "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs(num * 60 * 60),
            _ => return Err("invalid duration format"),
        };

        duration += unit;
        str = rest;
    }

    Ok(duration)
}

/// Options for the fuzzer.
#[derive(Debug, Args)]
pub struct FuzzSettings {
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
    /// When running the validation out-of-process, hide the plugin's output.
    ///
    /// This can be useful for validating noisy plugins.
    #[arg(long)]
    pub hide_output: bool,

    /// Run the fuzzer for this long before stopping.
    /// By default it will run until stopped manually via Ctrl+C.
    #[arg(long, short = 'd', value_parser = parse_duration)]
    pub duration: Option<Duration>,

    /// When running the fuzzer out-of-process, this many fuzzing chunks will be run in parallel.
    ///
    /// By default this is set to the number of logical CPU cores.
    #[arg(long, short = 'j')]
    pub jobs: Option<usize>,

    /// Run the fuzzer with this random seed _in-process_.
    ///
    /// This will run a single deterministic fuzzing chunk that will execute the same sequence of calls every time.
    /// Useful for reproducing an error/crash produced by the fuzzer.
    #[arg(long, conflicts_with = "jobs", conflicts_with = "duration")]
    pub reproduce: Option<u64>,

    /// When running the validation in-process, emit a JSON trace file that can be viewed with
    /// Chrome's tracing viewer or <https://ui.perfetto.dev>.
    ///
    /// This has a non-negligible performance impact.
    #[arg(long, requires = "reproduce")]
    pub trace: bool,
}

/// The main fuzzer command. This will fuzz one or more plugins and print the results.
pub fn fuzz(verbosity: Verbosity, settings: FuzzSettings) -> Result<ExitCode> {
    crate::fuzz::fuzz(FuzzerCli { verbosity, settings })?;

    Ok(ExitCode::SUCCESS)
}

struct FuzzerCli {
    verbosity: Verbosity,
    settings: FuzzSettings,
}

impl crate::fuzz::FuzzerCli for FuzzerCli {
    fn verbosity(&self) -> Verbosity {
        self.verbosity
    }

    fn settings(&self) -> &FuzzSettings {
        &self.settings
    }
}
