//! The base of the validation framework. This contains utilities for setting up a test case in a
//! way that somewhat mimics a real host.

use crate::Verbosity;
use crate::cli::sandbox::{SandboxConfig, SandboxOperation};
use crate::cli::{Config, panic_message};
use crate::commands::validate::ValidatorSettings;
use crate::plugin::library::{PluginLibrary, PluginMetadata};
use crate::tests::{PluginLibraryTestCase, PluginTestCase, TestCase, TestResult, TestStatus};
use crate::util::IteratorExt;
use anyhow::{Context, Result};
use clap_sys::version::clap_version_is_compatible;
use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};
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
    pub num_passed: usize,
    /// The number of failed or crashed test cases.
    pub num_failed: usize,
    /// The number of skipped test cases.
    pub num_skipped: usize,
    /// The number of test cases resulting in a warning.
    pub num_warnings: usize,
}

/// Run the validator using the specified settings. Returns an error if any of the plugin paths
/// could not loaded, or if the plugin ID filter did not match any plugins.
pub fn validate(verbosity: Verbosity, settings: &ValidatorSettings, config: &Config) -> Result<ValidationResult> {
    let filter_test = {
        let test_filter_regexes = settings
            .include
            .iter()
            .map(|x| {
                Regex::new(x).with_context(|| format!("Could not parse the test filter regular expression '{}'", x))
            })
            .collect::<Result<Vec<_>>>()?;

        let test_exclude_regexes = settings
            .exclude
            .iter()
            .map(|x| {
                Regex::new(x).with_context(|| format!("Could not parse the test exclude regular expression '{}'", x))
            })
            .collect::<Result<Vec<_>>>()?;

        move |id: &str| {
            let config_enabled = config.is_test_enabled(id);
            let filter_matches = test_filter_regexes.is_empty() || test_filter_regexes.iter().any(|f| f.is_match(id));
            let exclude_matches = test_exclude_regexes.iter().any(|f| f.is_match(id));

            config_enabled && filter_matches && !exclude_matches
        }
    };

    let filter_plugin = |metadata: &PluginMetadata| {
        settings
            .plugin_id
            .as_ref()
            .is_none_or(|plugin_id| &metadata.id == plugin_id)
    };

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
                    .filter(|test| filter_test(&test.to_string()))
                    .map_parallel(parallel, |test| {
                        run_test(
                            verbosity,
                            settings,
                            SandboxedValidation::PluginLibrary {
                                test,
                                path: library_path.clone(),
                            },
                        )
                    })
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
                .filter(|plugin_metadata| filter_plugin(plugin_metadata))
                .map_parallel(parallel, |plugin_metadata| {
                    let tests = PluginTestCase::iter()
                        .filter(|test| filter_test(&test.to_string()))
                        .map_parallel(parallel, |test| {
                            run_test(
                                verbosity,
                                settings,
                                SandboxedValidation::Plugin {
                                    test,
                                    path: library_path.clone(),
                                    plugin_id: plugin_metadata.id.clone(),
                                },
                            )
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

fn run_test(verbosity: Verbosity, settings: &ValidatorSettings, request: SandboxedValidation) -> Result<TestResult> {
    let start = Instant::now();
    let (status, duration) = request
        .invoke(if settings.in_process {
            None
        } else {
            Some(SandboxConfig {
                verbosity,
                hide_output: settings.hide_output,
                timeout: Some(Duration::from_secs(30)),
            })
        })
        .unwrap_or_else(|err| {
            (
                TestStatus::Crashed {
                    details: err.to_string(),
                },
                start.elapsed(),
            )
        });

    let (name, description) = match &request {
        SandboxedValidation::PluginLibrary { test, .. } => (test.to_string(), test.description()),
        SandboxedValidation::Plugin { test, .. } => (test.to_string(), test.description()),
    };

    match &status {
        TestStatus::Success { details: None } => {
            log::info!("Test {} completed", name)
        }
        TestStatus::Success { details: Some(details) } => {
            log::info!("Test {} completed: {}", name, details)
        }
        TestStatus::Warning { details: None } => {
            log::warn!("Test {} completed with a warning", name)
        }
        TestStatus::Warning { details: Some(details) } => {
            log::warn!("Test {} completed with a warning: {}", name, details)
        }
        TestStatus::Failed { details: None } => {
            log::error!("Test {} failed", name)
        }
        TestStatus::Failed { details: Some(details) } => {
            log::error!("Test {} failed: {}", name, details)
        }
        TestStatus::Crashed { details } => {
            log::error!("Test {} crashed: {}", name, details)
        }
        _ => {}
    }

    Ok(TestResult {
        name,
        description,
        duration,
        status,
    })
}

#[derive(Serialize, Deserialize)]
pub enum SandboxedValidation {
    PluginLibrary {
        test: PluginLibraryTestCase,
        path: PathBuf,
    },
    Plugin {
        test: PluginTestCase,
        path: PathBuf,
        plugin_id: String,
    },
}

impl SandboxOperation for SandboxedValidation {
    const ID: &'static str = "validate";

    type Result = (TestStatus, Duration);

    fn run(&self) -> Self::Result {
        let start = Instant::now();

        let closure = || match self {
            SandboxedValidation::PluginLibrary { test, path } => test.run(path),
            SandboxedValidation::Plugin { test, path, plugin_id } => test.run((path, plugin_id)),
        };

        let status = match catch_unwind(AssertUnwindSafe(closure)) {
            Ok(Ok(test_status)) => test_status,
            Ok(Err(err)) => TestStatus::Failed {
                details: Some(format!("{err:#}")),
            },
            Err(panic) => TestStatus::Crashed {
                details: panic_message(&*panic),
            },
        };

        (status, start.elapsed())
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

    /// Filter the test results using the specified filter function.
    pub fn filter(mut self, mut f: impl FnMut(&TestResult) -> bool) -> Self {
        self.plugin_tests.values_mut().for_each(|tests| tests.retain(&mut f));
        self.plugin_library_tests
            .values_mut()
            .for_each(|tests| tests.retain(&mut f));
        self
    }
}

impl ValidationTally {
    /// Get the total number of tests run.
    pub fn total(&self) -> usize {
        self.num_passed + self.num_failed + self.num_skipped + self.num_warnings
    }
}
