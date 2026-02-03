//! Commands for validating plugins.

use super::{TextWrapper, println_wrapped};
use crate::tests::{TestResult, TestStatus};
use crate::{Verbosity, validator};
use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use std::path::PathBuf;
use std::process::ExitCode;

/// Options for the validator.
#[derive(Debug, Args)]
pub struct ValidatorSettings {
    /// Paths to one or more plugins that should be validated.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Only validate plugins with this ID.
    ///
    /// If the plugin library contains multiple plugins, then you can pass a single plugin's ID
    /// to this option to only validate that plugin. Otherwise all plugins in the library are
    /// validated.
    #[arg(short = 'i', long)]
    pub plugin_id: Option<String>,
    /// Print the test output as JSON instead of human readable text.
    #[arg(long)]
    pub json: bool,
    /// Only run the tests that match this case-insensitive regular expression.
    #[arg(short = 'f', long)]
    pub test_filter: Option<String>,
    /// Changes the behavior of -f/--test-filter to skip matching tests instead.
    #[arg(short = 'v', long)]
    pub invert_filter: bool,
    /// When running the validation out-of-process, hide the plugin's output.
    ///
    /// This can be useful for validating noisy plugins.
    #[arg(long)]
    pub hide_output: bool,
    /// Only show failed tests.
    ///
    /// This affects both the human readable and the JSON output.
    #[arg(long)]
    pub only_failed: bool,
    /// Run the tests within this process.
    ///
    /// Tests are normally run in separate processes in case the plugin crashes. Another benefit
    /// of the out-of-process validation is that the test always starts from a clean state.
    /// Using this option will remove those protections, but in turn the tests may run faster.
    #[arg(long)]
    pub in_process: bool,
    /// Don't run tests in parallel.
    ///
    /// This will cause the out-of-process tests to be run sequentially. Implied when the
    /// --in-process option is used. Can be useful for keeping plugin output in the correct order.
    #[arg(long, conflicts_with = "in_process")]
    pub no_parallel: bool,
}

/// Options for running a single test. This is used for the out-of-process testing method. This
/// option is hidden from the CLI as it's merely an implementation detail.
#[derive(Debug, Args)]
pub struct SingleTestSettings {
    /// The type of test (plugin library or plugin) to run.
    pub test_type: String,
    /// The name of the test to run.
    pub test_name: String,
    /// The serialized test data as JSON.
    pub test_data: String,

    /// The name of the file to write the test's JSON result to. This is not done through STDIO
    /// because the hosted plugin may also write things there.
    #[arg(long)]
    pub output_file: PathBuf,
}

/// The main validator command. This will validate one or more plugins and print the results.
pub fn validate(verbosity: Verbosity, settings: &ValidatorSettings) -> Result<ExitCode> {
    let mut result = validator::validate(verbosity, settings).context("Could not run the validator")?;
    let tally = result.tally();

    // Filtering out tests should be done after we did the tally for consistency's sake
    if settings.only_failed {
        // The `.drain_filter()` methods have not been stabilized yet, so to make things
        // easy for us we'll just inefficiently rebuild the data structures
        result.plugin_library_tests = result
            .plugin_library_tests
            .into_iter()
            .filter_map(|(library_path, tests)| {
                let tests: Vec<_> = tests
                    .into_iter()
                    .filter(|test| test.status.failed_or_warning())
                    .collect();
                if tests.is_empty() {
                    None
                } else {
                    Some((library_path, tests))
                }
            })
            .collect();

        result.plugin_tests = result
            .plugin_tests
            .into_iter()
            .filter_map(|(plugin_id, tests)| {
                let tests: Vec<_> = tests
                    .into_iter()
                    .filter(|test| test.status.failed_or_warning())
                    .collect();
                if tests.is_empty() {
                    None
                } else {
                    Some((plugin_id, tests))
                }
            })
            .collect();
    }

    if settings.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("Could not format JSON")
        );
    } else {
        fn print_test(wrapper: &mut TextWrapper, test: &TestResult) {
            println_wrapped!(
                wrapper,
                "   - {} {}: {}",
                test.name,
                format!("({}ms)", test.duration.as_millis()).black().bold(),
                test.description
            );

            let status_text = match test.status {
                TestStatus::Success { .. } => "PASSED".green(),
                TestStatus::Skipped { .. } => "SKIPPED".dimmed(),
                TestStatus::Warning { .. } => "WARNING".yellow(),
                TestStatus::Failed { .. } => "FAILED".red(),
                TestStatus::Crashed { .. } => "CRASHED".red().bold(),
            };
            let test_result = match test.status.details() {
                Some(reason) => format!("     {status_text}: {reason}"),
                None => format!("     {status_text}"),
            };
            wrapper.print_auto(test_result);
        }

        let mut wrapper = TextWrapper::default();
        if !result.plugin_library_tests.is_empty() {
            println!("Plugin library tests:");
            for (library_path, tests) in result.plugin_library_tests {
                println!();
                println_wrapped!(wrapper, " - {}", library_path.display());

                for test in tests {
                    println!();
                    print_test(&mut wrapper, &test);
                }
            }

            println!();
        }

        if !result.plugin_tests.is_empty() {
            println!("Plugin tests:");
            for (plugin_id, tests) in result.plugin_tests {
                println!();
                println_wrapped!(wrapper, " - {plugin_id}");

                for test in tests {
                    println!();
                    print_test(&mut wrapper, &test);
                }
            }

            println!();
        }

        let num_tests = tally.total();
        println_wrapped!(
            wrapper,
            "{} {} run, {} passed, {} failed, {} skipped, {} warnings",
            num_tests,
            if num_tests == 1 { "test" } else { "tests" },
            tally.num_passed,
            tally.num_failed,
            tally.num_skipped,
            tally.num_warnings
        );
    }

    // If any of the tests failed, this process should exit with a failure code
    if tally.num_failed == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Run a single test and write the output to a file. This command is a hidden implementation detail
/// used by the validator to run tests in a different process.
pub fn run_single(settings: &SingleTestSettings) -> Result<ExitCode> {
    validator::run_single_test(settings)?;
    Ok(ExitCode::SUCCESS)
}
