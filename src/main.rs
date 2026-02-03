use clap::{Parser, Subcommand, ValueEnum};
use std::process::ExitCode;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use yansi::Paint;

mod commands;
mod debug;
mod index;
mod plugin;
mod tests;
mod util;
mod validator;

#[derive(Parser)]
#[command(author, version, about, long_about = None, propagate_version = true)]
struct Cli {
    /// clap-validator's own logging verbosity.
    ///
    /// This can be used to silence all non-essential output, or to enable more in depth tracing.
    #[arg(short, long, default_value = "debug")]
    verbosity: Verbosity,

    #[command(subcommand)]
    command: Command,
}

/// The verbosity level. Set to `Debug` by default. `Trace` can be used to get more information on
/// what the validator is actually doing.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Verbosity {
    /// Suppress all logging output from the validator itself.
    Quiet,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// The validator's subcommands.
#[derive(Subcommand)]
enum Command {
    /// Validate one or more plugins.
    Validate(commands::validate::ValidatorSettings),
    /// Run a single test.
    ///
    /// This is used for the out-of-process testing. Since it's merely an implementation detail, the
    /// option is not shown in the CLI.
    #[command(hide = true)]
    RunSingleTest(commands::validate::SingleTestSettings),

    #[command(subcommand)]
    List(commands::list::ListCommand),
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Before doing anything, we need to make sure any temporary artifact files from the previous
    // run are cleaned up. These are used for things like state dumps when one of the state tests
    // fail. This is allowed to fail since the directory may not exist and even if it does and we
    // cannot remove it, then that may not be a problem.
    let _ = std::fs::remove_dir_all(util::validator_temp_dir());
    let _ = std::fs::create_dir_all(util::validator_temp_dir());

    let trace_path = util::validator_temp_dir().join("trace.json");
    let trace_enabled = match &cli.command {
        Command::Validate(settings) => settings.trace,
        _ => false,
    };

    let log_level = match cli.verbosity {
        Verbosity::Quiet => LevelFilter::OFF,
        Verbosity::Error => LevelFilter::ERROR,
        Verbosity::Warn => LevelFilter::WARN,
        Verbosity::Info => LevelFilter::INFO,
        Verbosity::Debug => LevelFilter::DEBUG,
        Verbosity::Trace => LevelFilter::TRACE,
    };

    tracing_subscriber::registry()
        .with(debug::LogStderrLayer::new().with_filter(log_level))
        .with(trace_enabled.then(|| debug::ChromeJsonLayer::new(&trace_path)))
        .init();

    // Install the panic hook to log panics instead of printing them to stderr.
    debug::install_panic_hook();

    // Mark the main thread as such for plugin instance creation checks.
    unsafe {
        plugin::library::mark_current_thread_as_os_main_thread();
    }

    let result = match cli.command {
        Command::Validate(settings) => commands::validate::validate(cli.verbosity, &settings),
        Command::RunSingleTest(settings) => commands::validate::run_single(&settings),
        Command::List(command) => commands::list::list(&command),
    };

    let status = match &result {
        Ok(code) => *code,
        Err(err) => {
            tracing::error!("{err:#}");
            ExitCode::FAILURE
        }
    };

    if trace_enabled {
        eprintln!(
            "{}",
            format!(
                "Trace written to '{}'. Use 'https://ui.perfetto.dev/ to view it.",
                trace_path.display()
            )
            .dim()
            .italic()
        );
    }

    status
}
