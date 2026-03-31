mod runner;

use crate::cli::sandbox::{SandboxConfig, SandboxOperation};
use crate::commands::Verbosity;
use crate::commands::fuzz::FuzzSettings;
use crate::plugin::util::IteratorExt;
use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Instant;

pub trait FuzzerCli {
    fn verbosity(&self) -> Verbosity;
    fn settings(&self) -> &FuzzSettings;
}

pub fn fuzz(cli: impl FuzzerCli) -> Result<()> {
    let settings = cli.settings();
    let verbosity = cli.verbosity();

    let plugins = discover(&settings.paths, settings.plugin_id.as_deref())?;

    if plugins.is_empty() {
        anyhow::bail!("No plugins selected");
    }

    if let Some(seed) = settings.reproduce {
        if plugins.len() > 1 {
            anyhow::bail!("Choose a single plugin when using --reproduce");
        }

        let (library, plugin_id) = &plugins[0];
        runner::run_fuzzer(library, plugin_id, seed)?;
        return Ok(());
    }

    // round robin over the plugins until we run out of time
    let start = Instant::now();
    let mut prng = runner::new_seeded_prng();

    std::iter::repeat(plugins)
        .flatten()
        .map(|(library, plugin_id)| (library, plugin_id, prng.next_u64())) // this is also kinda goofy
        .take_while(|_| settings.duration.is_none_or(|duration| start.elapsed() < duration))
        .parallelize(settings.jobs, |(library, plugin_id, seed)| {
            SandboxedFuzzerChunk {
                library: library.clone(),
                plugin_id: plugin_id.clone(),
                seed,
            }
            .invoke(Some(SandboxConfig {
                verbosity,
                hide_output: settings.hide_output,
                timeout: None,
            }))
        });

    Ok(())
}

/// Scan the paths for plugins and return the paths and plugin IDs of the plugins that should be fuzzed.
fn discover(paths: &[PathBuf], plugin_id: Option<&str>) -> Result<Vec<(PathBuf, String)>> {
    let mut result = Vec::new();

    for path in paths {
        let library = crate::plugin::library::PluginLibrary::load(path)
            .with_context(|| format!("Could not load the plugin library at '{}'", path.display()))?;

        let metadata = library
            .metadata()
            .with_context(|| format!("Could not get the plugin metadata for library '{}'", path.display()))?;

        for plugin in metadata.plugins {
            if plugin_id.as_ref().is_none_or(|id| id == &plugin.id) {
                result.push((path.clone(), plugin.id));
            }
        }
    }

    Ok(result)
}

#[derive(Serialize, Deserialize)]
pub struct SandboxedFuzzerChunk {
    library: PathBuf,
    plugin_id: String,
    seed: u64,
}

impl SandboxOperation for SandboxedFuzzerChunk {
    const ID: &'static str = "fuzz";
    type Result = runner::FuzzResult;

    fn run(&self) -> Self::Result {
        match runner::run_fuzzer(&self.library, &self.plugin_id, self.seed) {
            Ok(result) => result,
            Err(err) => runner::FuzzResult::Error {
                details: format!("{:#}", err),
            },
        }
    }
}
