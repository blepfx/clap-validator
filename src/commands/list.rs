//! Commands for listing information about the validator or installed plugins.

use super::{TextWrapper, println_wrapped, println_wrapped_no_indent};
use crate::Verbosity;
use crate::commands::list::scan_out_of_process::ScanStatus;
use crate::plugin::index::{index_plugins, scan_plugin};
use crate::plugin::preset_discovery::PresetFile;
use anyhow::{Context, Result};
use clap::Subcommand;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
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

pub fn list(verbosity: Verbosity, command: &ListCommand) -> Result<ExitCode> {
    match command {
        ListCommand::Tests { json } => list_tests(*json),
        ListCommand::Plugins { json, in_process } => list_plugins(*json, *in_process, verbosity),
        ListCommand::Presets {
            json,
            in_process,
            paths,
        } => list_presets(*json, *in_process, verbosity, paths.clone()),
    }
}

/// List presets for one, more, or all installed CLAP plugins.
fn list_presets(json: bool, in_process: bool, verbosity: Verbosity, paths: Option<Vec<PathBuf>>) -> Result<ExitCode> {
    let plugins = match paths {
        Some(paths) => paths,
        None => index_plugins().context("Error while crawling plugins")?,
    };

    let results = if in_process {
        plugins
            .into_iter()
            .map(|path| match scan_plugin(&path, true) {
                Ok(library) => (path, ScanStatus::Success { library }),
                Err(err) => (
                    path,
                    ScanStatus::Error {
                        details: format!("{err:#}"),
                    },
                ),
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        plugins
            .into_par_iter()
            .map(|path| {
                let result = scan_out_of_process::spawn(&path, true, verbosity)?;
                Ok((path, result))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).expect("Could not format JSON")
        );
    } else {
        let mut wrapper = TextWrapper::default();
        for (i, (plugin_path, status)) in results.into_iter().enumerate() {
            if i > 0 {
                println!();
            }

            match status {
                ScanStatus::Error { details } => {
                    println_wrapped!(wrapper, "{} - {}: {}", plugin_path.display(), "ERROR".red(), details);
                }

                ScanStatus::Crashed { details } => {
                    println_wrapped!(
                        wrapper,
                        "{} - {}: {}",
                        plugin_path.display(),
                        "CRASHED".red().bold(),
                        details
                    );
                }

                ScanStatus::Success { library } => {
                    println_wrapped!(
                        wrapper,
                        "{}: (contains {} {})",
                        plugin_path.display(),
                        library.preset_providers.len(),
                        if library.preset_providers.len() == 1 {
                            "preset provider"
                        } else {
                            "preset providers"
                        }
                    );
                    println!();

                    for (i, provider) in library.preset_providers.into_iter().enumerate() {
                        if i > 0 {
                            println!();
                        }

                        println_wrapped!(
                            wrapper,
                            " - {} ({}) (contains {} {}, {} {}):",
                            provider.provider_name,
                            provider.provider_vendor.as_deref().unwrap_or("unknown vendor"),
                            provider.soundpacks.len(),
                            if provider.soundpacks.len() == 1 {
                                "soundpack"
                            } else {
                                "soundpacks"
                            },
                            provider.presets.len(),
                            if provider.presets.len() == 1 {
                                "preset"
                            } else {
                                "presets"
                            },
                        );

                        if !provider.soundpacks.is_empty() {
                            println!();
                            println!("   Soundpacks:");

                            for soundpack in provider.soundpacks {
                                println!();
                                println_wrapped!(wrapper, "   - {} ({})", soundpack.name, soundpack.id);
                                if let Some(description) = soundpack.description {
                                    println_wrapped_no_indent!(wrapper, "     {}", description);
                                }
                                println!();
                                println_wrapped!(
                                    wrapper,
                                    "     vendor: {}",
                                    soundpack.vendor.as_deref().unwrap_or("(unknown)")
                                );
                                if let Some(homepage_url) = soundpack.homepage_url {
                                    println_wrapped!(wrapper, "     homepage url: {homepage_url}");
                                }
                                if let Some(image_path) = soundpack.image_path {
                                    println_wrapped!(wrapper, "     image path: {image_path}");
                                }
                                if let Some(release_timestamp) = soundpack.release_timestamp {
                                    println_wrapped!(wrapper, "     released: {release_timestamp}");
                                }
                                println_wrapped!(wrapper, "     flags: {}", soundpack.flags);
                            }
                        }

                        if !provider.presets.is_empty() {
                            println!();
                            println!("   Presets:");

                            for (preset_uri, preset_file) in provider.presets {
                                println!();
                                match preset_file {
                                    PresetFile::Single(preset) => {
                                        println_wrapped!(wrapper, "   - {}", preset_uri);

                                        println!();
                                        println_wrapped!(
                                            wrapper,
                                            "     {} ({})",
                                            preset.name,
                                            preset.plugin_ids_string()
                                        );
                                        if let Some(description) = preset.description {
                                            println_wrapped_no_indent!(wrapper, "     {}", description);
                                        }
                                        println!();
                                        if !preset.creators.is_empty() {
                                            println_wrapped!(
                                                wrapper,
                                                "     {}: {}",
                                                if preset.creators.len() == 1 {
                                                    "creator"
                                                } else {
                                                    "creators"
                                                },
                                                preset.creators.join(", ")
                                            );
                                        }
                                        if let Some(soundpack_id) = preset.soundpack_id {
                                            println_wrapped!(wrapper, "     soundpack: {soundpack_id}");
                                        }
                                        if let Some(creation_time) = preset.creation_time {
                                            println_wrapped!(wrapper, "     created: {creation_time}");
                                        }
                                        if let Some(modification_time) = preset.modification_time {
                                            println_wrapped!(wrapper, "     modified: {modification_time}");
                                        }
                                        println_wrapped!(wrapper, "     flags: {}", preset.flags);
                                        if !preset.features.is_empty() {
                                            println_wrapped!(
                                                wrapper,
                                                "     features: [{}]",
                                                preset.features.join(", ")
                                            );
                                        }
                                        if !preset.extra_info.is_empty() {
                                            println_wrapped!(wrapper, "     extra info: {:#?}", preset.extra_info);
                                        }
                                    }
                                    PresetFile::Container(presets) => {
                                        println_wrapped!(
                                            wrapper,
                                            "   - {} (contains {} {})",
                                            preset_uri,
                                            presets.len(),
                                            if presets.len() == 1 { "preset" } else { "presets" }
                                        );

                                        for (load_key, preset) in presets {
                                            println!();
                                            println_wrapped!(
                                                wrapper,
                                                "     - {} ({}, {})",
                                                preset.name,
                                                load_key,
                                                preset.plugin_ids_string()
                                            );
                                            if let Some(description) = preset.description {
                                                println_wrapped_no_indent!(wrapper, "       {}", description);
                                            }
                                            println!();
                                            if !preset.creators.is_empty() {
                                                println_wrapped!(
                                                    wrapper,
                                                    "       {}: {}",
                                                    if preset.creators.len() == 1 {
                                                        "creator"
                                                    } else {
                                                        "creators"
                                                    },
                                                    preset.creators.join(", ")
                                                );
                                            }
                                            if let Some(soundpack_id) = preset.soundpack_id {
                                                println_wrapped!(wrapper, "       soundpack: {soundpack_id}");
                                            }
                                            if let Some(creation_time) = preset.creation_time {
                                                println_wrapped!(wrapper, "       created: {creation_time}");
                                            }
                                            if let Some(modification_time) = preset.modification_time {
                                                println_wrapped!(wrapper, "       modified: {modification_time}");
                                            }
                                            println_wrapped!(wrapper, "       flags: {}", preset.flags);
                                            if !preset.features.is_empty() {
                                                println_wrapped!(
                                                    wrapper,
                                                    "       features: [{}]",
                                                    preset.features.join(", ")
                                                );
                                            }
                                            if !preset.extra_info.is_empty() {
                                                println_wrapped!(
                                                    wrapper,
                                                    "       extra info: {:#?}",
                                                    preset.extra_info
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Lists basic information about all installed CLAP plugins.
fn list_plugins(json: bool, in_process: bool, verbosity: Verbosity) -> Result<ExitCode> {
    let plugins = index_plugins().context("Error while crawling plugins")?;
    let results = if in_process {
        plugins
            .into_iter()
            .map(|path| match scan_plugin(&path, false) {
                Ok(library) => (path, ScanStatus::Success { library }),
                Err(err) => (
                    path,
                    ScanStatus::Error {
                        details: format!("{err:#}"),
                    },
                ),
            })
            .collect::<BTreeMap<_, _>>()
    } else {
        plugins
            .into_par_iter()
            .map(|path| {
                let result = scan_out_of_process::spawn(&path, false, verbosity)?;
                Ok((path, result))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&results).expect("Could not format JSON")
        );
    } else {
        let mut wrapper = TextWrapper::default();
        for (i, (plugin_path, status)) in results.into_iter().enumerate() {
            if i > 0 {
                println!();
            }

            match status {
                ScanStatus::Error { details } => {
                    println_wrapped!(wrapper, "{} - {}: {}", plugin_path.display(), "ERROR".red(), details);
                }

                ScanStatus::Crashed { details } => {
                    println_wrapped!(
                        wrapper,
                        "{} - {}: {}",
                        plugin_path.display(),
                        "CRASHED".red().bold(),
                        details
                    );
                }

                ScanStatus::Success { library } => {
                    println_wrapped!(
                        wrapper,
                        "{}: (CLAP {}.{}.{}, contains {} {})",
                        plugin_path.display(),
                        library.metadata.version.0,
                        library.metadata.version.1,
                        library.metadata.version.2,
                        library.metadata.plugins.len(),
                        if library.metadata.plugins.len() == 1 {
                            "plugin"
                        } else {
                            "plugins"
                        },
                    );

                    for plugin in library.metadata.plugins {
                        println!();
                        println_wrapped!(
                            wrapper,
                            " - {} {} ({})",
                            plugin.name,
                            plugin.version.as_deref().unwrap_or("(unknown version)"),
                            plugin.id
                        );

                        // Whether it makes sense to always show optional fields or not depends on
                        // the field
                        if let Some(description) = plugin.description {
                            println_wrapped_no_indent!(wrapper, "   {description}");
                        }
                        println!();
                        println_wrapped!(
                            wrapper,
                            "   vendor: {}",
                            plugin.vendor.as_deref().unwrap_or("(unknown)")
                        );
                        if let Some(manual_url) = plugin.manual_url {
                            println_wrapped!(wrapper, "   manual url: {manual_url}");
                        }
                        if let Some(support_url) = plugin.support_url {
                            println_wrapped!(wrapper, "   support url: {support_url}");
                        }
                        println_wrapped!(wrapper, "   features: [{}]", plugin.features.join(", "));
                    }
                }
            }
        }
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
        let mut wrapper = TextWrapper::default();
        let mut print_test = |test: &crate::tests::TestListItem| {
            if config.is_test_enabled(&test.name) {
                println_wrapped!(wrapper, "- {}: {}\n", test.name.bold(), test.description);
            } else {
                println_wrapped!(
                    wrapper,
                    "- {} {}: {}\n",
                    test.name.bold(),
                    "disabled".dim().italic(),
                    test.description
                );
            }
        };

        println!("Plugin library tests:");
        for test in list.plugin_library_tests {
            print_test(&test);
        }

        println!("\nPlugin tests:");
        for test in list.plugin_tests {
            print_test(&test);
        }
    }

    Ok(ExitCode::SUCCESS)
}

pub mod scan_out_of_process {
    use crate::Verbosity;
    use crate::plugin::index::{ScannedPlugin, scan_plugin};
    use anyhow::{Context, Result};
    use clap::{Args, ValueEnum};
    use serde::{Deserialize, Serialize};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::time::Duration;
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
    pub enum ScanStatus {
        Success { library: ScannedPlugin },
        Error { details: String },
        Crashed { details: String },
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
        let result = match scan_plugin(&settings.plugin_path, settings.scan_presets) {
            Ok(plugin) => ScanStatus::Success { library: plugin },
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
