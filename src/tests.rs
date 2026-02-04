//! All tests in the validation test suite.
//!
//! Tests are split up in tests for the entire plugin library, and tests for individual plugins
//! within the library. The former group of tests exists mostly to ensure good plugin scanning
//! behavior.
//!
//! The results for the tests need to be serializable as JSON, and there also needs to be some way
//! to refer to a single test in a cli invocation (in order to be able to run tests out-of-process).
//! To facilitate this, the test cases are all identified by variants in an enum, and that enum can
//! be converted to and from a string representation.

use crate::util;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::any::TypeId;
use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use strum::IntoEnumIterator;

mod plugin;
mod plugin_library;
mod rng;

pub use plugin::PluginTestCase;
pub use plugin_library::PluginLibraryTestCase;

/// A test case for testing the behavior of a plugin. This `Test` object contains the result of a
/// test, which is serialized to and from JSON so the test can be run in another process.
#[derive(Debug, Deserialize, Serialize)]
pub struct TestResult {
    /// The name of this test.
    pub name: String,
    /// A description of what this test case has tested.
    pub description: String,
    /// The outcome of the test.
    pub status: TestStatus,
    /// How much time it took
    pub duration: Duration,
}

/// The result of running a test. Skipped and failed test may optionally include an explanation for
/// why this happened.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
#[serde(tag = "code")]
pub enum TestStatus {
    /// The test passed successfully.
    Success { details: Option<String> },
    /// The plugin segfaulted, SIGABRT'd, or otherwise crashed while running the test. This is only
    /// caught for out-of-process validation, for obvious reasons.
    Crashed { details: String },
    /// The test failed.
    Failed { details: Option<String> },
    /// Preconditions for running the test were not met, so the test has been skipped.
    Skipped { details: Option<String> },
    /// The test did not succeed, but this should not be treated as a hard failure. This is reserved
    /// for tests involving runtime performance that might otherwise yield different results
    /// depending on the target system.
    Warning { details: Option<String> },
}

/// Stores all of the available tests and their descriptions. Used solely for pretty printing
/// purposes in `clap-validator list tests`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TestList {
    pub plugin_library_tests: Vec<TestListItem>,
    pub plugin_tests: Vec<TestListItem>,
}

/// A single item in the test list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TestListItem {
    pub name: String,
    pub description: String,
}

/// An abstraction for a test case. This mostly exists because we need two separate kinds of tests
/// (per library and per plugin), and it's good to keep the interface uniform.
pub trait TestCase<'a>: Display + FromStr + Sized + 'static {
    /// The type of the arguments the test cases are parameterized over. This can be an instance of
    /// the plugin library and a plugin ID, or just the file path to the plugin library.
    type TestArgs: Serialize + Deserialize<'a>;

    /// Get the textual description for a test case. This description won't contain any line breaks,
    /// but it may consist of multiple sentences.
    fn description(&self) -> String;

    /// Run a test case for a specified arguments in the current, returning the result. If the test
    /// cuases the plugin to segfault, then this will obviously not return. See
    /// [`run_out_of_process()`][Self::run_out_of_process()] for a generic way to run test cases in
    /// a separate process.
    ///
    /// In the event that this is called for a plugin ID that does not exist within the plugin
    /// library, then the test will also be marked as failed.
    fn run(&self, args: Self::TestArgs) -> Result<TestStatus>;

    /// Get a writable temporary file handle for this test case. The file will be located at
    /// `$TMP_DIR/clap-validator/$plugin_id/$test_name/$file_name`. The temporary files directory is
    /// cleared on a new validator run, but the files will persist until then.
    fn temporary_file(&self, plugin_id: &str, name: &str) -> Result<(PathBuf, fs::File)> {
        let path = util::validator_temp_dir()
            .join(plugin_id)
            .join(self.to_string())
            .join(name);

        if path.exists() {
            panic!(
                "Tried to create a temporary file at '{}', but this file already exists",
                path.display()
            )
        }

        fs::create_dir_all(path.parent().unwrap())
            .expect("Could not create the directory for the test's temporary files");
        let file = fs::File::create(&path).expect("Could not create a temporary file for the test");

        Ok((path, file))
    }
}

impl TestStatus {
    /// Returns `true` if tests with this status should be shown when running the validator with the
    /// `--only-failed` option.
    pub fn failed_or_warning(&self) -> bool {
        match self {
            TestStatus::Success { .. } | TestStatus::Skipped { .. } => false,
            TestStatus::Warning { .. } | TestStatus::Crashed { .. } | TestStatus::Failed { .. } => true,
        }
    }

    /// Get the textual explanation for the test status, if this is available.
    pub fn details(&self) -> Option<&str> {
        match self {
            TestStatus::Success { details }
            | TestStatus::Failed { details }
            | TestStatus::Skipped { details }
            | TestStatus::Warning { details } => details.as_deref(),
            TestStatus::Crashed { details } => Some(details),
        }
    }
}

impl TestListItem {
    pub fn from<'a, T: TestCase<'a>>(test_case: &T) -> Self {
        Self {
            name: test_case.to_string(),
            description: test_case.description(),
        }
    }
}

impl Default for TestList {
    fn default() -> Self {
        Self {
            plugin_library_tests: PluginLibraryTestCase::iter().map(|c| TestListItem::from(&c)).collect(),
            plugin_tests: PluginTestCase::iter().map(|c| TestListItem::from(&c)).collect(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SerializedTest {
    pub test_type: String,
    pub test_name: String,
    pub data: String,
}

impl SerializedTest {
    pub fn from_test<'a, T: TestCase<'a>>(test: &T, data: &T::TestArgs) -> Result<Self> {
        if TypeId::of::<T>() == TypeId::of::<PluginTestCase>() {
            Ok(Self {
                test_type: "plugin".into(),
                test_name: test.to_string(),
                data: serde_json::to_string(&data)?,
            })
        } else if TypeId::of::<T>() == TypeId::of::<PluginLibraryTestCase>() {
            Ok(Self {
                test_type: "plugin_library".into(),
                test_name: test.to_string(),
                data: serde_json::to_string(&data)?,
            })
        } else {
            panic!("Unsupported test case type for serialization.");
        }
    }

    pub fn run(&self) -> Result<TestStatus> {
        if self.test_type == "plugin" {
            let test: PluginTestCase = self.test_name.parse()?;
            test.run(serde_json::from_str(&self.data)?)
        } else if self.test_type == "plugin_library" {
            let test: PluginLibraryTestCase = self.test_name.parse()?;
            test.run(serde_json::from_str(&self.data)?)
        } else {
            panic!("Unsupported test type '{}'", self.test_type);
        }
    }
}
