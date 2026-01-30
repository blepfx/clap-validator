//! The base of the validation framework. This contains utilities for setting up a test case in a
//! way that somewhat mimics a real host.

use crate::Verbosity;
use crate::commands::validate::{SingleTestSettings, ValidatorSettings};
use crate::plugin::library::{PluginLibrary, PluginMetadata};
use crate::tests::{PluginLibraryTestCase, PluginTestCase, SerializedTest, TestCase, TestResult, TestStatus};
use crate::util::{self, IteratorExt};
use anyhow::{Context, Result};
use clap::ValueEnum;
use clap_sys::version::clap_version_is_compatible;
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;
use strum::IntoEnumIterator;

/// The results of running the validation test suite on one or more plugins. Use the
/// [`tally()`][Self::tally()] method to compute the number of successful and failed tests.
///
/// Uses `BTreeMap`s purely so the order is stable.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ValidationResult {
    /// A map indexed by plugin library paths containing the results of running the per-plugin
    /// library tests on one or more plugin libraries. These tests mainly examine the plugin's
    /// scanning behavior.
    pub plugin_library_tests: BTreeMap<PathBuf, Vec<TestResult>>,
    /// A map indexed by plugin IDs containing the results of running the per-plugin tests on one or
    /// more plugins.
    pub plugin_tests: BTreeMap<String, Vec<TestResult>>,
}

/// Statistics for the validator.
pub struct ValidationTally {
    /// The number of passed test cases.
    pub num_passed: u32,
    /// The number of failed or crashed test cases.
    pub num_failed: u32,
    /// The number of skipped test cases.
    pub num_skipped: u32,
    /// The number of test cases resulting in a warning.
    pub num_warnings: u32,
}

/// Run the validator using the specified settings. Returns an error if any of the plugin paths
/// could not loaded, or if the plugin ID filter did not match any plugins.
pub fn validate(verbosity: Verbosity, settings: &ValidatorSettings) -> Result<ValidationResult> {
    // Before doing anything, we need to make sure any temporary artifact files from the previous
    // run are cleaned up. These are used for things like state dumps when one of the state tests
    // fail. This is allowed to fail since the directory may not exist and even if it does and we
    // cannot remove it, then that may not be a problem.
    let _ = std::fs::remove_dir_all(util::validator_temp_dir());
    let test_filter_re = settings
        .test_filter
        .as_deref()
        .map(|filter| {
            RegexBuilder::new(filter)
                .case_insensitive(true)
                .build()
                .context("The test filter is not a valid regular expression")
        })
        .transpose()?;

    // The tests can optionally be run in parallel. This is not the default since some plugins may
    // not handle it correctly, event when the plugins are loaded in different processes. It's also
    // incompatible with the in-process mode.
    let parallel = !settings.no_parallel && !settings.in_process;
    let mut results = settings
        .paths
        .iter()
        .map_parallel(parallel, |library_path| {
            // We distinguish between two separate classes of tests: tests for an entire plugin
            // library, and tests for a single plugin contained witin that library. The former
            // group of tests are run first and they only receive the path to the plugin library
            // as their argument, while the second class of tests receive an already loaded
            // plugin library and a plugin ID as their arugmetns. We'll start with the tests for
            // entire plugin libraries so the in-process mode makes a bit more sense. Otherwise
            // we would be measuring plugin scanning time on libraries that may still be loaded
            // in the process.
            let mut plugin_library_tests = BTreeMap::new();
            plugin_library_tests.insert(
                library_path.clone(),
                PluginLibraryTestCase::iter()
                    .filter(|test| test_filter(test, settings, &test_filter_re))
                    .map_parallel(parallel, |test| run_test(&test, verbosity, settings, library_path))
                    .collect::<Result<Vec<TestResult>>>()?,
            );

            // And these are the per-plugin instance tests
            let plugin_library = PluginLibrary::load(library_path)
                .with_context(|| format!("Could not load '{}'", library_path.display()))?;

            let plugin_metadata = plugin_library
                .metadata()
                .with_context(|| format!("Could not fetch plugin metadata for '{}'", library_path.display()))?;

            if !clap_version_is_compatible(plugin_metadata.clap_version()) {
                log::debug!(
                    "'{}' uses an unsupported CLAP version ({}.{}.{}), skipping...",
                    library_path.display(),
                    plugin_metadata.version.0,
                    plugin_metadata.version.1,
                    plugin_metadata.version.2
                );

                // Since this is a map-reduce, this acts like a continue statement in a loop. We
                // could use `.filter_map()` instead but that would only make things more
                // complicated
                return Ok(ValidationResult::default());
            }

            let plugin_tests = plugin_metadata
                .plugins
                .iter()
                .filter(|plugin_metadata| plugin_filter(plugin_metadata, settings))
                .map_parallel(parallel, |plugin_metadata| {
                    let tests = PluginTestCase::iter()
                        .filter(|test| test_filter(test, settings, &test_filter_re))
                        .map_parallel(parallel, |test| {
                            run_test(&test, verbosity, settings, (library_path, &plugin_metadata.id))
                        });

                    Ok((plugin_metadata.id.clone(), tests.collect::<Result<Vec<TestResult>>>()?))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;

            Ok(ValidationResult {
                plugin_library_tests,
                plugin_tests,
            })
        })
        .reduce(|a, b| {
            let (a, b) = (a?, b?);

            // In the serial version this could be done when iterating over the plugins, but
            // when using iterators you can't do that. But it's still essential to make sure we
            // don't test two versionsq of the same plugin.
            if a.intersects(&b) {
                anyhow::bail!(
                    "Duplicate plugin ID in validation results. Maybe multiple versions of the same plugin are being \
                     validated."
                );
            }

            Ok(ValidationResult::union(a, b))
        })
        .unwrap_or_else(|| Ok(ValidationResult::default()))?;

    // The parallel iterators don't preserve order, so this needs to be sorted to make sure the test
    // results are always reported in the same order
    for tests in results
        .plugin_tests
        .values_mut()
        .chain(results.plugin_library_tests.values_mut())
    {
        tests.sort_by(|a, b| Ord::cmp(&a.name, &b.name));
    }

    if let Some(plugin_id) = &settings.plugin_id
        && results.plugin_tests.is_empty()
    {
        anyhow::bail!("No plugins matched the plugin ID '{plugin_id}'.");
    }

    Ok(results)
}

pub fn run_single_test(settings: &SingleTestSettings) -> Result<()> {
    let test = SerializedTest {
        test_type: settings.test_type.clone(),
        test_name: settings.test_name.clone(),
        data: settings.test_data.clone(),
    };

    let result = run_test_in_process(|| test.run());

    std::fs::write(
        &settings.output_file,
        serde_json::to_string(&result).context("Could not serialize the test result to JSON")?,
    )
    .with_context(|| {
        format!(
            "Could not write the test result to '{}'",
            settings.output_file.display()
        )
    })?;

    Ok(())
}

/// The filter function for determining whether or not a test should be run based on the validator's
/// settings settings.
fn test_filter<'a, T: TestCase<'a>>(test: &T, settings: &ValidatorSettings, test_filter_re: &Option<Regex>) -> bool {
    let test_name = test.to_string();
    match (&test_filter_re, settings.invert_filter) {
        (Some(test_filter_re), false) if !test_filter_re.is_match(&test_name) => false,
        (Some(test_filter_re), true) if test_filter_re.is_match(&test_name) => false,
        _ => true,
    }
}

/// The filter function for determining whether or not tests should be run for a particular plugin.
fn plugin_filter(plugin_metadata: &PluginMetadata, settings: &ValidatorSettings) -> bool {
    // It's possible to filter by plugin ID in case you want to validate a single plugin
    // from a plugin library containing multiple plugins
    #[allow(clippy::match_like_matches_macro)]
    match &settings.plugin_id {
        Some(plugin_id) if &plugin_metadata.id != plugin_id => false,
        _ => true,
    }
}

/// The filter function for determining whether or not a test should be run based on the validator's
/// settings settings.
fn run_test<'a, T: TestCase<'a>>(
    test: &T,
    verbosity: Verbosity,
    settings: &ValidatorSettings,
    args: T::TestArgs,
) -> Result<TestResult> {
    let start = Instant::now();
    let status = if settings.in_process {
        run_test_in_process(|| test.run(args))
    } else {
        run_test_out_of_process(test, args, verbosity, settings.hide_output)?
    };

    Ok(TestResult {
        name: test.to_string(),
        description: test.description(),
        duration: start.elapsed(),
        status,
    })
}

fn run_test_out_of_process<'a, T: TestCase<'a>>(
    test: &T,
    args: T::TestArgs,
    verbosity: Verbosity,
    hide_output: bool,
) -> Result<TestStatus> {
    let test = SerializedTest::from_test(test, &args)?;

    // The idea here is that we'll invoke the same clap-validator binary with a special hidden command
    // that runs a single test. This is the reason why test cases must be convertible to and
    // from strings. If everything goes correctly, then the child process will write the results
    // as JSON to the specified file path. This is intentionaly not done through STDIO since the
    // hosted plugin may also write things there, and doing STDIO redirection within the child
    // process is more complicated than just writing the result to a temporary file.

    // This temporary file will automatically be removed when this function exits
    let output_file_path = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .context("Could not create a temporary file path")?
        .into_temp_path();

    let mut command =
        Command::new(std::env::current_exe().context("Could not find the path to the current executable")?);

    command
        .arg("--verbosity")
        .arg(verbosity.to_possible_value().unwrap().get_name())
        .arg("run-single-test")
        .args([OsStr::new("--output-file"), output_file_path.as_os_str()])
        .arg(test.test_type)
        .arg(test.test_name)
        .arg(test.data);

    if hide_output {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }

    let exit_status = command
        .spawn()
        .context("Could not call clap-validator for out-of-process validation")?
        // The docs make it seem like this can only fail if the process isn't running, but if
        // spawn succeeds then this can never fail:
        .wait()
        .context("Error while waiting on clap-validator to finish running the test")?;

    if !exit_status.success() {
        return Ok(TestStatus::Crashed {
            details: exit_status.to_string(),
        });
    }

    // At this point, the child process _should_ have written its output to `output_file_path`,
    // and we can just parse it from there
    let result = serde_json::from_str(&fs::read_to_string(&output_file_path).with_context(|| {
        format!(
            "Could not read the child process output from '{}'",
            output_file_path.display()
        )
    })?)
    .context("Could not parse the child process output to JSON")?;

    Ok(result)
}

fn run_test_in_process(test: impl FnOnce() -> Result<TestStatus>) -> TestStatus {
    match catch_unwind(AssertUnwindSafe(test)) {
        Ok(Ok(test_status)) => test_status,
        Ok(Err(err)) => TestStatus::Failed {
            details: Some(format!("{err:#}")),
        },
        Err(panic) => {
            let message = if let Some(s) = panic.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else {
                "A panic occurred".to_string()
            };

            TestStatus::Crashed {
                details: format!("{message}. This is a bug in clap-validator"),
            }
        }
    }
}

impl ValidationResult {
    /// Count the number of passing, failing, and skipped tests.
    pub fn tally(&self) -> ValidationTally {
        let mut num_passed = 0;
        let mut num_failed = 0;
        let mut num_skipped = 0;
        let mut num_warnings = 0;
        for test in self
            .plugin_library_tests
            .values()
            .chain(self.plugin_tests.values())
            .flatten()
        {
            match test.status {
                TestStatus::Success { .. } => num_passed += 1,
                TestStatus::Crashed { .. } | TestStatus::Failed { .. } => num_failed += 1,
                TestStatus::Skipped { .. } => num_skipped += 1,
                TestStatus::Warning { .. } => num_warnings += 1,
            }
        }

        ValidationTally {
            num_passed,
            num_failed,
            num_skipped,
            num_warnings,
        }
    }

    // Check whether the maps in the object intersect. Useful to ensure that a plugin ID only occurs
    // once in the outputs before merging them.
    pub fn intersects(&self, other: &Self) -> bool {
        for key in other.plugin_library_tests.keys() {
            if self.plugin_library_tests.contains_key(key) {
                return true;
            }
        }

        for key in other.plugin_tests.keys() {
            if self.plugin_tests.contains_key(key) {
                return true;
            }
        }

        false
    }

    /// Merge the results from two validation result objects. If `other` contains a key that also
    /// exists in this object, then the version from `other` is used.
    pub fn union(mut self, other: Self) -> Self {
        self.plugin_library_tests.extend(other.plugin_library_tests);
        self.plugin_tests.extend(other.plugin_tests);
        self
    }
}

impl ValidationTally {
    /// Get the total number of tests run.
    pub fn total(&self) -> u32 {
        self.num_passed + self.num_failed + self.num_skipped + self.num_warnings
    }
}
