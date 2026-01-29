use crate::{
    plugin::{
        ext::{
            audio_ports::{AudioPortConfig, AudioPorts},
            note_ports::{NotePortConfig, NotePorts},
        },
        library::PluginLibrary,
        process::{AudioBuffers, Event, ProcessScope, TransportState},
    },
    tests::{
        TestStatus,
        rng::{NoteGenerator, TransportFuzzer, new_prng},
    },
};
use anyhow::{Context, Result};

const BUFFER_SIZE: u32 = 512;

/// The test for `PluginTestCase::TransportNull`
pub fn test_transport_null(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => NotePortConfig::default(),
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut note_rng = NoteGenerator::new(&note_ports_config);
        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.transport().is_freerun = true;
            process
                .input_queue()
                .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.audio_buffers().randomize(&mut prng);
            process.run()?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::TransportFuzz`
pub fn test_transport_fuzz(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => NotePortConfig::default(),
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut transport_fuzz = TransportFuzzer::new();
        let mut note_rng = NoteGenerator::new(&note_ports_config);
        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..20 {
            transport_fuzz.mutate(&mut prng, process.transport());
            process
                .input_queue()
                .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.audio_buffers().randomize(&mut prng);
            process.run()?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::TransportFuzzSampleAccurate`
pub fn test_transport_fuzz_sample_accurate(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const INTERVALS: &[u32] = &[1000, 100, 1];

    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => NotePortConfig::default(),
    };

    for &interval in INTERVALS {
        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut note_rng = NoteGenerator::new(&note_ports_config);
                let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

                let mut transport_fuzz = TransportFuzzer::new();
                let mut transport_state = TransportState::default();

                for _ in 0..5 {
                    // set initial transport state for the block
                    *process.transport() = transport_state.clone();

                    // add sample-accurate transport events
                    let mut current_sample = 0;
                    while current_sample < BUFFER_SIZE {
                        // advance transport state to the event position, mutate it, and add the event
                        transport_state.advance(interval as i64, process.sample_rate());
                        transport_fuzz.mutate(&mut prng, &mut transport_state);

                        current_sample += interval;
                        process
                            .input_queue()
                            .add_events([Event::Transport(transport_state.as_clap_transport(current_sample))]);
                    }

                    // set it to the start of the next block
                    transport_state.advance(-(current_sample as i64), process.sample_rate());

                    process.audio_buffers().randomize(&mut prng);
                    process
                        .input_queue()
                        .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| {
                format!(
                    "Error during sample-accurate transport test with interval of {} samples",
                    interval
                )
            })?;
    }

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}
