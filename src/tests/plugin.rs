//! Tests for individual plugin instances.

use super::TestCase;
use crate::plugin::library::PluginLibrary;
use crate::tests::TestStatus;
use anyhow::{Context, Result};
use std::path::Path;

mod descriptor;
mod layout;
mod params;
pub mod processing;
mod state;

/// The tests for individual CLAP plugins. See the module's heading for more information, and the
/// `description` function below for a description of each test case.
#[derive(strum_macros::Display, strum_macros::EnumString, strum_macros::EnumIter)]
pub enum PluginTestCase {
    #[strum(serialize = "descriptor-consistency")]
    DescriptorConsistency,
    #[strum(serialize = "features-categories")]
    FeaturesCategories,
    #[strum(serialize = "features-duplicates")]
    FeaturesDuplicates,
    #[strum(serialize = "layout-audio-ports-config")]
    LayoutAudioPortsConfig,
    #[strum(serialize = "layout-configurable-audio-ports")]
    LayoutConfigurableAudioPorts,
    #[strum(serialize = "process-audio-out-of-place-basic")]
    ProcessAudioOutOfPlaceBasic,
    #[strum(serialize = "process-audio-in-place-basic")]
    ProcessAudioInPlaceBasic,
    #[strum(serialize = "process-audio-out-of-place-double")]
    ProcessAudioOutOfPlaceDouble,
    #[strum(serialize = "process-audio-in-place-double")]
    ProcessAudioInPlaceDouble,
    #[strum(serialize = "process-audio-constant-mask")]
    ProcessAudioConstantMask,
    #[strum(serialize = "process-audio-reset-determinism")]
    ProcessAudioResetDeterminism,
    #[strum(serialize = "process-note-out-of-place-basic")]
    ProcessNoteOutOfPlaceBasic,
    #[strum(serialize = "process-note-inconsistent")]
    ProcessNoteInconsistent,
    #[strum(serialize = "process-varying-sample-rates")]
    ProcessVaryingSampleRates,
    #[strum(serialize = "process-varying-block-sizes")]
    ProcessVaryingBlockSizes,
    #[strum(serialize = "process-random-block-sizes")]
    ProcessRandomBlockSizes,
    #[strum(serialize = "param-conversions")]
    ParamConversions,
    #[strum(serialize = "param-fuzz-basic")]
    ParamFuzzBasic,
    #[strum(serialize = "param-fuzz-bounds")]
    ParamFuzzBounds,
    #[strum(serialize = "param-fuzz-sample-accurate")]
    ParamFuzzSampleAccurate,
    #[strum(serialize = "param-fuzz-modulation")]
    ParamFuzzModulation,
    #[strum(serialize = "param-set-wrong-namespace")]
    ParamSetWrongNamespace,
    #[strum(serialize = "param-default-values")]
    ParamDefaultValues,
    #[strum(serialize = "state-invalid-empty")]
    StateInvalidEmpty,
    #[strum(serialize = "state-invalid-random")]
    StateInvalidRandom,
    #[strum(serialize = "state-reproducibility-basic")]
    StateReproducibilityBasic,
    #[strum(serialize = "state-reproducibility-null-cookies")]
    StateReproducibilityNullCookies,
    #[strum(serialize = "state-reproducibility-flush")]
    StateReproducibilityFlush,
    #[strum(serialize = "state-buffered-streams")]
    StateBufferedStreams,
}

impl<'a> TestCase<'a> for PluginTestCase {
    /// A loaded CLAP plugin library and the ID of the plugin contained within that library that
    /// should be tested.
    type TestArgs = (&'a Path, &'a str);

    fn description(&self) -> String {
        match self {
            PluginTestCase::DescriptorConsistency => String::from(
                "The plugin descriptor returned from the plugin factory and the plugin descriptor stored on the \
                 'clap_plugin object should be equivalent.",
            ),
            PluginTestCase::FeaturesCategories => {
                String::from("The plugin needs to have at least one of the main CLAP category features.")
            }
            PluginTestCase::FeaturesDuplicates => {
                String::from("The plugin's features array should not contain any duplicates.")
            }
            PluginTestCase::ProcessAudioOutOfPlaceBasic => String::from(
                "Processes random audio through the plugin with its default parameter values and tests whether the \
                 output does not contain any non-finite or subnormal values. Uses out-of-place audio processing.",
            ),
            PluginTestCase::ProcessAudioInPlaceBasic => String::from(
                "Processes random audio through the plugin with its default parameter values and tests whether the \
                 output does not contain any non-finite or subnormal values. Uses in-place audio processing for buses \
                 that support it.",
            ),
            PluginTestCase::ProcessAudioOutOfPlaceDouble => format!(
                "Same as {}, but uses 64-bit floating point audio buffers instead of 32-bit ones for ports that \
                 support it.",
                PluginTestCase::ProcessAudioOutOfPlaceBasic,
            ),
            PluginTestCase::ProcessAudioInPlaceDouble => format!(
                "Same as {}, but uses 64-bit floating point audio buffers instead of 32-bit ones for ports that \
                 support it.",
                PluginTestCase::ProcessAudioInPlaceBasic,
            ),
            PluginTestCase::LayoutConfigurableAudioPorts => format!(
                "Performs the same test as {}, but this time it tries random configurations exposed via the \
                 'configurable-audio-ports' extension.",
                PluginTestCase::ProcessAudioOutOfPlaceBasic,
            ),
            PluginTestCase::LayoutAudioPortsConfig => format!(
                "Performs the same test as {}, but this time it tries all available port configurations exposed via \
                 the 'audio-ports-config' extension.",
                PluginTestCase::ProcessAudioInPlaceBasic,
            ),
            PluginTestCase::ProcessAudioConstantMask => String::from(
                "Processes random audio through the plugin with its default parameter values while setting the \
                 constant mask on silent blocks, and tests whether the output does not contain any non-finite or \
                 subnormal values and that the plugin sets the constant mask correctly. Uses out-of-place audio \
                 processing.",
            ),
            PluginTestCase::ProcessNoteOutOfPlaceBasic => String::from(
                "Sends audio and random note and MIDI events to the plugin with its default parameter values and \
                 tests the output for consistency. Uses out-of-place audio processing.",
            ),
            PluginTestCase::ProcessNoteInconsistent => String::from(
                "Sends intentionally inconsistent and mismatching note and MIDI events to the plugin with its default \
                 parameter values and tests the output for consistency. Uses out-of-place audio processing.",
            ),
            PluginTestCase::ProcessVaryingSampleRates => String::from(
                "Processes random audio and random note events through the plugin with its default parameter values \
                 while trying different sample rates ranging from 1kHz to 768kHz, including fractional rates, and \
                 tests whether the output does not contain any non-finite or subnormal values. Uses out-of-place \
                 audio processing.",
            ),
            PluginTestCase::ProcessVaryingBlockSizes => String::from(
                "Processes random audio and random note events through the plugin with its default parameter values \
                 while trying different maximum block sizes ranging from 1 to 32768, including non-power-of-two ones, \
                 and tests whether the output does not contain any non-finite or subnormal values. Uses out-of-place \
                 audio processing.",
            ),
            PluginTestCase::ProcessRandomBlockSizes => String::from(
                "Processes random audio and random note events through the plugin with maximum block size of 2048 \
                 while randomizing block sizes for each process call, and tests whether the output does not contain \
                 any non-finite or subnormal values. Uses out-of-place audio processing.",
            ),
            PluginTestCase::ProcessAudioResetDeterminism => String::from(
                "Asserts that resetting the plugin via 'clap_plugin::reset()' and via re-activation results in \
                 deterministic output when processing the same audio and events again.",
            ),
            PluginTestCase::ParamConversions => String::from(
                "Asserts that value to string and string to value conversions are supported for ether all or none of \
                 the plugin's parameters, and that conversions between values and strings roundtrip consistently.",
            ),
            PluginTestCase::ParamFuzzBasic => format!(
                "Generates {} sets of random parameter values, sets those on the plugin, and has the plugin process \
                 {} buffers of random audio and note events. The plugin passes the test if it doesn't produce any \
                 infinite or NaN values, and doesn't crash.",
                params::FUZZ_NUM_PERMUTATIONS,
                params::FUZZ_RUNS_PER_PERMUTATION
            ),
            PluginTestCase::ParamFuzzBounds => format!(
                "The exact same test as {}, but this time the parameter values are snapped to the minimum and maximum \
                 values.",
                PluginTestCase::ParamFuzzBasic
            ),
            PluginTestCase::ParamFuzzSampleAccurate => String::from(
                "Sets parameter values in a sample-accurate fashion while processing audio, generating them at fixed \
                 intervals (1, 100, 1000 samples). The plugin passes the test if it doesn't produce any infinite or \
                 NaN values, and doesn't crash.",
            ),
            PluginTestCase::ParamFuzzModulation => String::from(
                "Sends parameter change events, including monophonic modulation and polyphonic automation/modulation \
                 events at random irregular unsynchronized intervals, and have the plugin process them. The plugin \
                 passes the test if it doesn't produce any infinite or NaN values, and doesn't crash.",
            ),
            PluginTestCase::ParamSetWrongNamespace => String::from(
                "Sends events to the plugin with the 'CLAP_EVENT_PARAM_VALUE' event type but with a mismatching \
                 namespace ID. Asserts that the plugin's parameter values don't change.",
            ),
            PluginTestCase::ParamDefaultValues => String::from(
                "Asserts that the values for all parameters are set correctly to their default values when the plugin \
                 is initialized.",
            ),
            PluginTestCase::StateInvalidEmpty => String::from(
                "The plugin should return false when 'clap_plugin_state::load()' is called with an empty state.",
            ),
            PluginTestCase::StateInvalidRandom => String::from(
                "Loads 3x1MB chunks of random bytes via 'clap_plugin_state::load()' and asserts that the plugin \
                 doesn't crash.",
            ),
            PluginTestCase::StateReproducibilityBasic => String::from(
                "Randomizes a plugin's parameters, saves its state, recreates the plugin instance, reloads the state, \
                 and then checks whether the parameter values are the same and whether saving the state once more \
                 results in the same state file as before. The parameter values are updated using the process \
                 function.",
            ),
            PluginTestCase::StateReproducibilityNullCookies => format!(
                "The exact same test as {}, but with all cookies in the parameter events set to null pointers. The \
                 plugin should handle this in the same way as the other test case.",
                PluginTestCase::StateReproducibilityBasic
            ),
            PluginTestCase::StateReproducibilityFlush => String::from(
                "Randomizes a plugin's parameters, saves its state, recreates the plugin instance, sets the same \
                 parameters as before, saves the state again, and then asserts that the two states are identical. The \
                 parameter values are set updated using the process function to create the first state, and using the \
                 flush function to create the second state.",
            ),
            PluginTestCase::StateBufferedStreams => format!(
                "Performs the same state and parameter reproducibility check as in '{}', but this time the plugin is \
                 only allowed to read a small prime number of bytes at a time when reloading and resaving the state.",
                PluginTestCase::StateReproducibilityBasic
            ),
        }
    }

    fn run(&self, (library_path, plugin_id): Self::TestArgs) -> Result<TestStatus> {
        // SAFETY: This is called on the main thread.
        let library = &PluginLibrary::load(library_path)
            .with_context(|| format!("Could not load '{}'", library_path.display()))?;

        match self {
            PluginTestCase::DescriptorConsistency => descriptor::test_consistency(library, plugin_id),
            PluginTestCase::FeaturesCategories => descriptor::test_features_categories(library, plugin_id),
            PluginTestCase::FeaturesDuplicates => descriptor::test_features_duplicates(library, plugin_id),
            PluginTestCase::LayoutAudioPortsConfig => layout::test_layout_audio_ports_config(library, plugin_id),
            PluginTestCase::LayoutConfigurableAudioPorts => {
                layout::test_layout_configurable_audio_ports(library, plugin_id)
            }
            PluginTestCase::ProcessAudioOutOfPlaceBasic => {
                processing::test_process_audio_basic(library, plugin_id, false)
            }
            PluginTestCase::ProcessAudioInPlaceBasic => processing::test_process_audio_basic(library, plugin_id, true),
            PluginTestCase::ProcessAudioOutOfPlaceDouble => {
                processing::test_process_audio_double(library, plugin_id, false)
            }
            PluginTestCase::ProcessAudioInPlaceDouble => {
                processing::test_process_audio_double(library, plugin_id, true)
            }
            PluginTestCase::ProcessAudioConstantMask => {
                processing::test_process_audio_constant_mask(library, plugin_id)
            }
            PluginTestCase::ProcessAudioResetDeterminism => {
                processing::test_process_audio_reset_determinism(library, plugin_id)
            }
            PluginTestCase::ProcessNoteOutOfPlaceBasic => {
                processing::test_process_note_out_of_place(library, plugin_id, true)
            }
            PluginTestCase::ProcessNoteInconsistent => {
                processing::test_process_note_out_of_place(library, plugin_id, false)
            }
            PluginTestCase::ProcessVaryingSampleRates => {
                processing::test_process_varying_sample_rates(library, plugin_id)
            }
            PluginTestCase::ProcessVaryingBlockSizes => {
                processing::test_process_varying_block_sizes(library, plugin_id)
            }
            PluginTestCase::ProcessRandomBlockSizes => processing::test_process_random_block_sizes(library, plugin_id),
            PluginTestCase::ParamConversions => params::test_param_conversions(library, plugin_id),
            PluginTestCase::ParamFuzzBasic => params::test_param_fuzz_basic(library, plugin_id, false),
            PluginTestCase::ParamFuzzBounds => params::test_param_fuzz_basic(library, plugin_id, true),
            PluginTestCase::ParamFuzzSampleAccurate => params::test_param_fuzz_sample_accurate(library, plugin_id),
            PluginTestCase::ParamFuzzModulation => params::test_param_fuzz_modulation(library, plugin_id),
            PluginTestCase::ParamSetWrongNamespace => params::test_param_set_wrong_namespace(library, plugin_id),
            PluginTestCase::ParamDefaultValues => params::test_param_default_values(library, plugin_id),
            PluginTestCase::StateInvalidEmpty => state::test_state_invalid_empty(library, plugin_id),
            PluginTestCase::StateInvalidRandom => state::test_state_invalid_random(library, plugin_id),
            PluginTestCase::StateReproducibilityBasic => {
                state::test_state_reproducibility_basic(library, plugin_id, false)
            }
            PluginTestCase::StateReproducibilityNullCookies => {
                state::test_state_reproducibility_basic(library, plugin_id, true)
            }
            PluginTestCase::StateReproducibilityFlush => state::test_state_reproducibility_flush(library, plugin_id),
            PluginTestCase::StateBufferedStreams => state::test_state_buffered_streams(library, plugin_id),
        }
    }
}
