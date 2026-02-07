//! Tests surrounding plugin features.

use crate::plugin::library::PluginLibrary;
use crate::tests::TestStatus;
use anyhow::{Context, Result};
use clap_sys::plugin_features::*;
use std::collections::HashSet;
use std::ffi::CStr;

/// Verifies that the descriptor stored in the factory and the descriptor stored on the plugin
/// object are equivalent.
pub fn test_consistency(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let metadata = library.metadata().with_context(|| {
        format!(
            "Could not fetch plugin metadata for '{}'",
            library.plugin_path().display()
        )
    })?;
    let factory_descriptor = metadata
        .plugins
        .into_iter()
        .find(|plugin_meta| plugin_meta.id == plugin_id)
        .expect("Incorrect plugin ID for metadata query");

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    let plugin_descriptor = plugin.descriptor()?;

    if plugin_descriptor == factory_descriptor {
        Ok(TestStatus::Success { details: None })
    } else {
        Ok(TestStatus::Failed {
            details: Some(format!(
                "The 'clap_plugin_descriptor' stored on '{plugin_id}'s 'clap_plugin' object contains different values \
                 than the one returned by the factory."
            )),
        })
    }
}

/// Check whether the plugin's categories are consistent. Currently this just makes sure that the
/// plugin has one of the four main plugin category features.
pub fn test_features_categories(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let features = plugin_features(library, plugin_id)?;

    // These are stored in the bindings as C-compatible null terminated strings, but we'll need them
    // as regular string slices so we can compare them to
    let instrument_feature = CLAP_PLUGIN_FEATURE_INSTRUMENT.to_str().unwrap();
    let audio_effect_feature = CLAP_PLUGIN_FEATURE_AUDIO_EFFECT.to_str().unwrap();
    let note_detector_feature = CLAP_PLUGIN_FEATURE_NOTE_DETECTOR.to_str().unwrap();
    let note_effect_feature = CLAP_PLUGIN_FEATURE_NOTE_EFFECT.to_str().unwrap();
    let analyzer_feature = CLAP_PLUGIN_FEATURE_ANALYZER.to_str().unwrap();

    let has_main_category = features.iter().any(|feature| -> bool {
        feature == instrument_feature
            || feature == audio_effect_feature
            || feature == note_detector_feature
            || feature == note_effect_feature
            || feature == analyzer_feature
    });

    if !has_main_category {
        anyhow::bail!(
            "The plugin needs to have at least one of the following plugin category features: \
             \"{instrument_feature}\", \"{audio_effect_feature}\", \"{note_effect_feature}\", or \
             \"{analyzer_feature}\"."
        );
    }

    Ok(TestStatus::Success { details: None })
}

const STANDARD_FEATURES: &[&CStr] = &[
    CLAP_PLUGIN_FEATURE_INSTRUMENT,
    CLAP_PLUGIN_FEATURE_AUDIO_EFFECT,
    CLAP_PLUGIN_FEATURE_NOTE_DETECTOR,
    CLAP_PLUGIN_FEATURE_NOTE_EFFECT,
    CLAP_PLUGIN_FEATURE_ANALYZER,
    CLAP_PLUGIN_FEATURE_SYNTHESIZER,
    CLAP_PLUGIN_FEATURE_SAMPLER,
    CLAP_PLUGIN_FEATURE_DRUM,
    CLAP_PLUGIN_FEATURE_DRUM_MACHINE,
    CLAP_PLUGIN_FEATURE_FILTER,
    CLAP_PLUGIN_FEATURE_PHASER,
    CLAP_PLUGIN_FEATURE_EQUALIZER,
    CLAP_PLUGIN_FEATURE_DEESSER,
    CLAP_PLUGIN_FEATURE_PHASE_VOCODER,
    CLAP_PLUGIN_FEATURE_GRANULAR,
    CLAP_PLUGIN_FEATURE_FREQUENCY_SHIFTER,
    CLAP_PLUGIN_FEATURE_PITCH_SHIFTER,
    CLAP_PLUGIN_FEATURE_DISTORTION,
    CLAP_PLUGIN_FEATURE_TRANSIENT_SHAPER,
    CLAP_PLUGIN_FEATURE_COMPRESSOR,
    CLAP_PLUGIN_FEATURE_EXPANDER,
    CLAP_PLUGIN_FEATURE_GATE,
    CLAP_PLUGIN_FEATURE_LIMITER,
    CLAP_PLUGIN_FEATURE_FLANGER,
    CLAP_PLUGIN_FEATURE_CHORUS,
    CLAP_PLUGIN_FEATURE_DELAY,
    CLAP_PLUGIN_FEATURE_REVERB,
    CLAP_PLUGIN_FEATURE_TREMOLO,
    CLAP_PLUGIN_FEATURE_GLITCH,
    CLAP_PLUGIN_FEATURE_UTILITY,
    CLAP_PLUGIN_FEATURE_PITCH_CORRECTION,
    CLAP_PLUGIN_FEATURE_RESTORATION,
    CLAP_PLUGIN_FEATURE_MULTI_EFFECTS,
    CLAP_PLUGIN_FEATURE_MIXING,
    CLAP_PLUGIN_FEATURE_MASTERING,
    CLAP_PLUGIN_FEATURE_MONO,
    CLAP_PLUGIN_FEATURE_STEREO,
    CLAP_PLUGIN_FEATURE_SURROUND,
    CLAP_PLUGIN_FEATURE_AMBISONIC,
];

/// Check whether the plugin has any non-standard features without namespaces.
/// This is not necessarily a problem, but it can be a sign of a typo or a misunderstanding of how features work.
pub fn test_features_standard(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let features = plugin_features(library, plugin_id)?;

    let invalid_features: Vec<String> = features
        .iter()
        .filter(|feature| !feature.contains(':')) // We can only compare ASCII features, so we'll ignore non-ASCII ones
        .filter(|feature| {
            STANDARD_FEATURES
                .iter()
                .all(|standard| feature != &standard.to_str().unwrap())
        })
        .cloned()
        .collect();

    if invalid_features.is_empty() {
        Ok(TestStatus::Success { details: None })
    } else {
        anyhow::bail!(
            "The plugin has the following non-standard features: {invalid_features:?}. Please make sure that all \
             features used by the plugin are listed in 'clap_sys::plugin_features'."
        );
    }
}

/// Confirm that the plugin does not have any duplicate features.
pub fn test_features_duplicates(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut features = plugin_features(library, plugin_id)?;
    let unique_features: HashSet<&str> = features.iter().map(|feature| feature.as_str()).collect();

    if unique_features.len() == features.len() {
        Ok(TestStatus::Success { details: None })
    } else {
        // We'll sort the features first to make spotting the duplicates easier
        features.sort_unstable();

        anyhow::bail!("The plugin has duplicate features: {features:?}")
    }
}

/// Get the feature vector for a plugin in the library. Returns `None` if the plugin ID does not
/// exist in the library.
fn plugin_features(library: &PluginLibrary, plugin_id: &str) -> Result<Vec<String>> {
    library
        .metadata()
        .with_context(|| {
            format!(
                "Could not fetch plugin metadata for '{}'",
                library.plugin_path().display()
            )
        })
        .map(|metadata| {
            metadata
                .plugins
                .into_iter()
                .find(|plugin_meta| plugin_meta.id == plugin_id)
                .expect("Incorrect plugin ID for metadata query")
        })
        .map(|metadata| metadata.features)
}
