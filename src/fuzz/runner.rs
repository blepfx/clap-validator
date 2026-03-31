use anyhow::Result;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub enum FuzzResult {
    Success {
        /// Measures runtime in `wallclock_time / audio_time`. Lower is better.
        perf_ratio: f64,
    },

    Error {
        details: String,
    },

    Crashed {
        details: String,
    },
}

/// Runs a single fuzzer chunk for a given plugin.
///
/// Fully deterministic w.r.t. the seed.
pub fn run_fuzzer(library: &Path, plugin_id: &str, seed: u64) -> Result<FuzzResult> {
    log::info!(
        "Fuzzing plugin '{}' in library '{}' with seed '{}'",
        plugin_id,
        library.display(),
        seed
    );

    // simulate fuzzing for now
    std::thread::sleep(Duration::from_secs(10));

    Ok(FuzzResult::Success { perf_ratio: 1.0 })
}

/// Creates a new PRNG that is seeded with the current time.
pub fn new_seeded_prng() -> rand::rngs::Xoshiro128PlusPlus {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    rand::rngs::Xoshiro128PlusPlus::from_seed(time.to_le_bytes())
}
