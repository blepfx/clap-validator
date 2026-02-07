//! Commands for listing information about the validator or installed plugins.

use crate::Verbosity;
use crate::commands::list::scan_out_of_process::ScanStatus;
use crate::plugin::index::{index_plugins, scan_library};
use crate::plugin::preset_discovery::PresetFile;
use crate::term::{Report, ReportItem};
use crate::util::IteratorExt;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;
use yansi::Paint;

/// Commands for listing tests and data realted to the installed plugins.
#[derive(Subcommand)]
pub enum ListCommand {
    /// Lists basic information about all installed CLAP plugins.
    Plugins {
        /// Print JSON instead of a human readable format.
        #[arg(short, long)]
        json: bool,
        /// Run the plugin indexing in-process instead of out-of-process.
        #[arg(long)]
        in_process: bool,
        /// Paths to one or more plugins that should be loaded and scanned, optional.
        ///
        /// All installed plugins are crawled if this value is missing.
        paths: Option<Vec<PathBuf>>,
    },
    /// Lists the available presets for one, more, or all installed CLAP plugins.
    Presets {
        /// Print JSON instead of a human readable format.
        #[arg(short, long)]
        json: bool,
        /// Run the plugin indexing in-process instead of out-of-process.
        #[arg(long)]
        in_process: bool,
        /// Paths to one or more plugins that should be indexed for presets, optional.
        ///
        /// All installed plugins are crawled if this value is missing.
        paths: Option<Vec<PathBuf>>,
    },
    /// Lists all available test cases.
    Tests {
        /// Print JSON instead of a human readable format.
        #[arg(short, long)]
        json: bool,
    },
}

pub fn list(verbosity: Verbosity, command: ListCommand) -> Result<ExitCode> {
    match command {
        ListCommand::Tests { json } => list_tests(json),
        ListCommand::Plugins {
            json,
            in_process,
            paths,
        } => list_plugins(json, in_process, verbosity, paths),
        ListCommand::Presets {
            json,
            in_process,
            paths,
        } => list_presets(json, in_process, verbosity, paths),
    }
}

/// List presets for one, more, or all installed CLAP plugins.
fn list_presets(json: bool, in_process: bool, verbosity: Verbosity, paths: Option<Vec<PathBuf>>) -> Result<ExitCode> {
    let plugins = match paths {
        Some(paths) => paths,
        None => index_plugins().context("Error while crawling plugins")?,
    };

    let results = plugins
        .into_iter()
        .map_parallel(!in_process, |path| scan_single(path, in_process, true, verbosity))
        .collect::<Result<BTreeMap<_, _>>>()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).expect("Could not format JSON")
        );
    } else {
        for (plugin_path, status) in results.into_iter() {
            match status {
                ScanStatus::Error { details } => {
                    let report = Report {
                        header: plugin_path.display().to_string(),
                        items: vec![ReportItem::Text(details)],
                        footer: vec!["ERROR".red().to_string()],
                    };

                    println!("\n{}", report.print());
                }

                ScanStatus::Crashed { details } => {
                    let report = Report {
                        header: plugin_path.display().to_string(),
                        items: vec![ReportItem::Text(details)],
                        footer: vec!["CRASHED".red().bold().to_string()],
                    };

                    println!("\n{}", report.print());
                }

                ScanStatus::Success { library, duration } => {
                    if library.preset_providers.is_empty() {
                        continue;
                    }

                    let mut group = Report {
                        header: plugin_path.display().to_string(),

                        items: vec![ReportItem::Text(format!(
                            "CLAP {}.{}.{}",
                            library.version.0, library.version.1, library.version.2
                        ))],

                        footer: vec![
                            "OK".green().to_string(),
                            format!(
                                "{} {}",
                                library.preset_providers.len(),
                                if library.preset_providers.len() == 1 {
                                    "preset provider"
                                } else {
                                    "preset providers"
                                }
                            ),
                            format!("{}ms", duration.as_millis()).dim().to_string(),
                        ],
                    };

                    for provider in library.preset_providers {
                        let mut report = Report {
                            header: provider.provider_id,
                            ..Default::default()
                        };

                        report.items.push(ReportItem::Text(format!(
                            "{} {}.{}.{} ({})",
                            provider.provider_name,
                            provider.provider_version.0,
                            provider.provider_version.1,
                            provider.provider_version.2,
                            provider.provider_vendor.as_deref().unwrap_or("unknown vendor"),
                        )));

                        report.footer.push(format!(
                            "{} {}",
                            provider.soundpacks.len(),
                            if provider.soundpacks.len() == 1 {
                                "soundpack"
                            } else {
                                "soundpacks"
                            }
                        ));

                        report.footer.push(format!(
                            "{} {}",
                            provider.presets.len(),
                            if provider.presets.len() == 1 {
                                "preset"
                            } else {
                                "presets"
                            }
                        ));

                        group.items.push(ReportItem::Child(report));
                    }

                    println!("\n{}", group.print());
                }
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Lists basic information about all installed CLAP plugins.
fn list_plugins(json: bool, in_process: bool, verbosity: Verbosity, paths: Option<Vec<PathBuf>>) -> Result<ExitCode> {
    let plugins = match paths {
        Some(paths) => paths,
        None => index_plugins().context("Error while crawling plugins")?,
    };

    let results = plugins
        .into_iter()
        .map_parallel(!in_process, |path| scan_single(path, in_process, false, verbosity))
        .collect::<Result<BTreeMap<_, _>>>()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).expect("Could not format JSON")
        );
    } else {
        let mut num_errors = 0;
        let mut num_libraries = 0;
        let mut num_plugins = 0;

        for (plugin_path, status) in results.into_iter() {
            match status {
                ScanStatus::Error { details } => {
                    num_errors += 1;

                    let report = Report {
                        header: plugin_path.display().to_string(),
                        items: vec![ReportItem::Text(details)],
                        footer: vec!["ERROR".red().to_string()],
                    };

                    println!("\n{}", report.print());
                }

                ScanStatus::Crashed { details } => {
                    num_errors += 1;

                    let report = Report {
                        header: plugin_path.display().to_string(),
                        items: vec![ReportItem::Text(details)],
                        footer: vec!["CRASHED".red().bold().to_string()],
                    };

                    println!("\n{}", report.print());
                }

                ScanStatus::Success { library, duration } => {
                    num_libraries += 1;

                    let mut group = Report {
                        header: plugin_path.display().to_string(),

                        items: vec![ReportItem::Text(format!(
                            "CLAP {}.{}.{}",
                            library.version.0, library.version.1, library.version.2
                        ))],

                        footer: vec![
                            "OK".green().to_string(),
                            format!(
                                "{} {}",
                                library.plugins.len(),
                                if library.plugins.len() == 1 {
                                    "plugin"
                                } else {
                                    "plugins"
                                }
                            ),
                            format!("{}ms", duration.as_millis()).dim().to_string(),
                        ],
                    };

                    for plugin in library.plugins {
                        num_plugins += 1;

                        let mut report = Report {
                            header: plugin.id,
                            ..Default::default()
                        };

                        report.items.push(ReportItem::Text(format!(
                            "{} {} ({})",
                            plugin.name,
                            plugin.version.as_deref().unwrap_or("(unknown version)"),
                            plugin.vendor.as_deref().unwrap_or("unknown vendor"),
                        )));

                        if let Some(description) = plugin.description {
                            report.items.push(ReportItem::Text(description));
                        }

                        if let Some(manual_url) = plugin.manual_url {
                            report
                                .items
                                .push(ReportItem::Text(format!("manual url: {}", manual_url)));
                        }

                        if let Some(support_url) = plugin.support_url {
                            report
                                .items
                                .push(ReportItem::Text(format!("support url: {}", support_url)));
                        }

                        report
                            .items
                            .push(ReportItem::Text(format!("features: [{}]", plugin.features.join(", "))));

                        group.items.push(ReportItem::Child(report));
                    }

                    println!("\n{}", group.print());
                }
            }
        }

        println!(
            "{} {}, {} {}, {} {}",
            num_libraries,
            if num_libraries == 1 { "library" } else { "libraries" },
            num_plugins,
            if num_plugins == 1 { "plugin" } else { "plugins" },
            num_errors,
            if num_errors == 1 { "error" } else { "errors" }
        )
    }

    Ok(ExitCode::SUCCESS)
}

/// Lists all available test cases.
fn list_tests(json: bool) -> Result<ExitCode> {
    let list = crate::tests::TestList::default();
    let config = crate::config::Config::from_current()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&list).expect("Could not format JSON")
        );
    } else {
        let report_test = |test: &crate::tests::TestListItem| {
            let mut report = Report {
                header: test.name.clone(),
                items: vec![ReportItem::Text(test.description.clone())],
                footer: vec![],
            };

            if !config.is_test_enabled(&test.name) {
                report.footer.push("disabled".dim().italic().to_string());
            }

            report
        };

        let mut library = Report {
            header: "Plugin Library".to_string(),
            items: vec![ReportItem::Text(
                "Tests for plugin factories, preset providers and plugin libraries (files) in general".to_string(),
            )],
            footer: vec![format!("{} tests", list.plugin_library_tests.len())],
        };

        let mut plugin = Report {
            header: "Plugin".to_string(),
            items: vec![ReportItem::Text(
                "Tests for specific plugins within libraries, including their behavior during initialization, \
                 deinitialization, audio processing and callback handling."
                    .to_string(),
            )],
            footer: vec![format!("{} tests", list.plugin_tests.len())],
        };

        for test in &list.plugin_library_tests {
            library.items.push(ReportItem::Child(report_test(test)));
        }

        for test in &list.plugin_tests {
            plugin.items.push(ReportItem::Child(report_test(test)));
        }

        println!("\n{}", library.print());
        println!("\n{}", plugin.print());
    }

    Ok(ExitCode::SUCCESS)
}

/// Scan a single plugin library for its basic information and optionally its presets,
/// either in-process or out-of-process depending on the `in_process` argument.
fn scan_single(
    path: PathBuf,
    in_process: bool,
    scan_presets: bool,
    verbosity: Verbosity,
) -> Result<(PathBuf, ScanStatus)> {
    if in_process {
        let start = Instant::now();
        let status = match scan_library(&path, scan_presets) {
            Ok(library) => ScanStatus::Success {
                library,
                duration: start.elapsed(),
            },
            Err(err) => ScanStatus::Error {
                details: format!("{err:#}"),
            },
        };

        Ok((path, status))
    } else {
        let status = scan_out_of_process::spawn(&path, scan_presets, verbosity)?;
        Ok((path, status))
    }
}

pub mod scan_out_of_process {
    use crate::Verbosity;
    use crate::plugin::index::{ScannedLibrary, scan_library};
    use anyhow::{Context, Result};
    use clap::{Args, ValueEnum};
    use serde::{Deserialize, Serialize};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::{Duration, Instant};
    use wait_timeout::ChildExt;

    #[derive(Debug, Args)]
    pub struct Settings {
        #[arg(long)]
        pub plugin_path: PathBuf,
        #[arg(long)]
        pub output_file: PathBuf,
        #[arg(long)]
        pub scan_presets: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    #[serde(tag = "status")]
    pub enum ScanStatus {
        Success {
            #[serde(flatten)]
            library: ScannedLibrary,
            duration: Duration,
        },
        Error {
            details: String,
        },
        Crashed {
            details: String,
        },
    }

    pub fn spawn(plugin_path: &Path, scan_presets: bool, verbosity: Verbosity) -> Result<ScanStatus> {
        const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

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
            .arg("scan-out-of-process")
            .args([OsStr::new("--output-file"), output_file_path.as_os_str()])
            .args([OsStr::new("--plugin-path"), plugin_path.as_os_str()]);

        if scan_presets {
            command.arg("--scan-presets");
        }

        let status = command
            .spawn()
            .context("Could not call clap-validator for out-of-process scanning")?
            .wait_timeout(WAIT_TIMEOUT)
            .context("Error while waiting on clap-validator to finish running the scan")?;

        match status {
            None => Ok(ScanStatus::Crashed {
                details: format!("Timed out after {} seconds", WAIT_TIMEOUT.as_secs()),
            }),

            Some(status) if !status.success() => Ok(ScanStatus::Crashed {
                details: status.to_string(),
            }),

            _ => {
                // At this point, the child process _should_ have written its output to `output_file_path`,
                // and we can just parse it from there
                let result = serde_json::from_str(&std::fs::read_to_string(&output_file_path).with_context(|| {
                    format!(
                        "Could not read the child process output from '{}'",
                        output_file_path.display()
                    )
                })?)
                .context("Could not parse the child process output to JSON")?;

                Ok(result)
            }
        }
    }

    pub fn run(settings: &Settings) -> Result<ExitCode> {
        let start = Instant::now();
        let result = match scan_library(&settings.plugin_path, settings.scan_presets) {
            Ok(library) => ScanStatus::Success {
                library,
                duration: start.elapsed(),
            },
            Err(err) => ScanStatus::Error {
                details: format!("{err:#}"),
            },
        };

        std::fs::write(
            &settings.output_file,
            serde_json::to_string(&result).context("Could not serialize the test result to JSON")?,
        )
        .with_context(|| {
            format!(
                "Could not write the scan result to '{}'",
                settings.output_file.display()
            )
        })?;

        Ok(ExitCode::SUCCESS)
    }
}
