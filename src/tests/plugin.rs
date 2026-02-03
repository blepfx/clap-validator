//! Tests for individual plugin instances.

use super::TestCase;
use crate::plugin::library::PluginLibrary;
use crate::tests::TestStatus;
use anyhow::{Context, Result};
use std::path::Path;

mod descriptor;
mod layout;
mod params;
mod processing;
mod state;
mod transport;

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
    #[strum(serialize = "layout-audio-ports-activation")]
    LayoutAudioPortsActivation,
    #[strum(serialize = "layout-audio-ports-config")]
    LayoutAudioPortsConfig,
    #[strum(serialize = "layout-configurable-audio-ports")]
    LayoutConfigurableAudioPorts,
    #[strum(serialize = "process-audio-basic-out-of-place")]
    ProcessAudioBasicOutOfPlace,
    #[strum(serialize = "process-audio-basic-in-place")]
    ProcessAudioBasicInPlace,
    #[strum(serialize = "process-audio-double-out-of-place")]
    ProcessAudioDoubleOutOfPlace,
    #[strum(serialize = "process-audio-double-in-place")]
    ProcessAudioDoubleInPlace,
    #[strum(serialize = "process-sleep-constant-mask")]
    ProcessSleepConstantMask,
    #[strum(serialize = "process-sleep-process-status")]
    ProcessSleepProcessStatus,
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
    #[strum(serialize = "transport-null")]
    TransportNull,
    #[strum(serialize = "transport-fuzz")]
    TransportFuzz,
    #[strum(serialize = "transport-fuzz-sample-accurate")]
    TransportFuzzSampleAccurate,
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
            PluginTestCase::ProcessAudioBasicOutOfPlace => String::from(
                "Processes random audio through the plugin with its default parameter values and tests whether the \
                 output does not contain any non-finite or subnormal values. Uses out-of-place audio processing.",
            ),
            PluginTestCase::ProcessAudioBasicInPlace => String::from(
                "Processes random audio through the plugin with its default parameter values and tests whether the \
                 output does not contain any non-finite or subnormal values. Uses in-place audio processing for buses \
                 that support it.",
            ),
            PluginTestCase::ProcessAudioDoubleOutOfPlace => format!(
                "Same as '{}', but uses 64-bit floating point audio buffers instead of 32-bit ones for ports that \
                 support it.",
                PluginTestCase::ProcessAudioBasicOutOfPlace,
            ),
            PluginTestCase::ProcessAudioDoubleInPlace => format!(
                "Same as '{}', but uses 64-bit floating point audio buffers instead of 32-bit ones for ports that \
                 support it.",
                PluginTestCase::ProcessAudioBasicInPlace,
            ),
            PluginTestCase::LayoutAudioPortsActivation => format!(
                "Same as '{}', but this time it toggles the activation state of audio ports on and off via the \
                 'audio-ports-activation' extension.",
                PluginTestCase::ProcessAudioBasicOutOfPlace,
            ),
            PluginTestCase::LayoutConfigurableAudioPorts => format!(
                "Same as '{}', but this time it tries random configurations exposed via the \
                 'configurable-audio-ports' extension.",
                PluginTestCase::ProcessAudioBasicOutOfPlace,
            ),
            PluginTestCase::LayoutAudioPortsConfig => format!(
                "Same as '{}', but this time it tries all available port configurations exposed via the \
                 'audio-ports-config' extension.",
                PluginTestCase::ProcessAudioBasicInPlace,
            ),
            PluginTestCase::ProcessSleepConstantMask => String::from(
                "Processes random audio through the plugin with its default parameter values while setting the \
                 constant mask on silent blocks, and tests whether the output does not contain any non-finite or \
                 subnormal values and that the plugin sets the constant mask correctly",
            ),
            PluginTestCase::ProcessSleepProcessStatus => String::from(
                "Processes random audio through the plugin with its default parameter values while checking if the \
                 output is consistent with the returned process status, and tests whether the output does not contain \
                 any non-finite or subnormal values and that the plugin sets the process status correctly",
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
                 while trying different maximum block sizes ranging from 1 to 16k, including non-power-of-two ones, \
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
                "The exact same test as '{}', but this time the parameter values are snapped to the minimum and \
                 maximum values.",
                PluginTestCase::ParamFuzzBasic
            ),
            PluginTestCase::ParamFuzzSampleAccurate => String::from(
                "Sets parameter values in a sample-accurate fashion while processing audio, generating them at fixed \
                 intervals (10, 100, 1000 samples). The plugin passes the test if it doesn't produce any infinite or \
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
                "The exact same test as '{}', but with all cookies in the parameter events set to null pointers. The \
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
            PluginTestCase::TransportNull => String::from(
                "Performs audio processing with a 'null' transport pointer, simulating a free-running transport \
                 state. The plugin passes the test if it doesn't produce any infinite or NaN values, and doesn't \
                 crash.",
            ),
            PluginTestCase::TransportFuzz => String::from(
                "Performs audio processing while randomly changing the transport state on every block. The plugin \
                 passes the test if it doesn't produce any infinite or NaN values, and doesn't crash.",
            ),
            PluginTestCase::TransportFuzzSampleAccurate => format!(
                "Same as '{}', but this time the test sends 'clap_event_transport' events in sample-accurate fashion \
                 while processing audio, generating them at fixed intervals (1, 100, 1000 samples). The plugin passes \
                 the test if it doesn't produce any infinite or NaN values, and doesn't crash.",
                PluginTestCase::TransportFuzz
            ),
        }
    }

    fn run(&self, (library_path, plugin_id): Self::TestArgs) -> Result<TestStatus> {
        let _span = tracing::debug_span!("PluginTestCase::run", test_case = %self, plugin_id = %plugin_id, library_path = %library_path.display()).entered();

        // SAFETY: This is called on the main thread.
        let library = &PluginLibrary::load(library_path)
            .with_context(|| format!("Could not load '{}'", library_path.display()))?;

        match self {
            PluginTestCase::DescriptorConsistency => descriptor::test_consistency(library, plugin_id),
            PluginTestCase::FeaturesCategories => descriptor::test_features_categories(library, plugin_id),
            PluginTestCase::FeaturesDuplicates => descriptor::test_features_duplicates(library, plugin_id),
            PluginTestCase::LayoutAudioPortsActivation => {
                layout::test_layout_audio_ports_activation(library, plugin_id)
            }
            PluginTestCase::LayoutAudioPortsConfig => layout::test_layout_audio_ports_config(library, plugin_id),
            PluginTestCase::LayoutConfigurableAudioPorts => {
                layout::test_layout_configurable_audio_ports(library, plugin_id)
            }
            PluginTestCase::ProcessAudioBasicOutOfPlace => {
                processing::test_process_audio_basic(library, plugin_id, false)
            }
            PluginTestCase::ProcessAudioBasicInPlace => processing::test_process_audio_basic(library, plugin_id, true),
            PluginTestCase::ProcessAudioDoubleOutOfPlace => {
                processing::test_process_audio_double(library, plugin_id, false)
            }
            PluginTestCase::ProcessAudioDoubleInPlace => {
                processing::test_process_audio_double(library, plugin_id, true)
            }
            PluginTestCase::ProcessSleepConstantMask => {
                processing::test_process_sleep_constant_mask(library, plugin_id)
            }
            PluginTestCase::ProcessSleepProcessStatus => {
                processing::test_process_sleep_process_status(library, plugin_id)
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
            PluginTestCase::TransportNull => transport::test_transport_null(library, plugin_id),
            PluginTestCase::TransportFuzz => transport::test_transport_fuzz(library, plugin_id),
            PluginTestCase::TransportFuzzSampleAccurate => {
                transport::test_transport_fuzz_sample_accurate(library, plugin_id)
            }
        }
    }
}
