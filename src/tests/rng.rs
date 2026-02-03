//! Utilities for generating pseudo-random data.

use crate::plugin::ext::audio_ports::AudioPortConfig;
use crate::plugin::ext::configurable_audio_ports::{AudioPortsRequest, AudioPortsRequestInfo};
use crate::plugin::ext::note_ports::NotePortConfig;
use crate::plugin::ext::params::{Param, ParamInfo};
use crate::plugin::process::{Event, TransportState};
use clap_sys::events::*;
use clap_sys::ext::ambisonic::*;
use midi_consts::channel_event as midi;
use rand::Rng;
use rand::seq::{IndexedRandom, IteratorRandom};
use rand_pcg::Pcg32;
use std::ops::RangeInclusive;

/// Create a new pseudo-random number generator with a fixed seed.
pub fn new_prng() -> Pcg32 {
    Pcg32::new(1337, 420)
}

/// A random note and MIDI event generator that generates consistent events based on the
/// capabilities stored in a [`NotePortConfig`]
#[derive(Debug, Clone)]
pub struct NoteGenerator<'a> {
    /// The note ports to generate random events for.
    config: &'a NotePortConfig,

    /// The parameter info to generate random poly modulation and automation events for.
    params: Option<&'a ParamInfo>,

    /// Only generate consistent events. This prevents things like note off events for notes that
    /// aren't playing, double note on events, and generating note expressions for notes that aren't
    /// active.
    only_consistent_events: bool,

    /// The range for the next event's timing relative to the previous event.
    /// This will be capped to 0 when generating events
    sample_offset_range: RangeInclusive<i32>,

    /// Contains the currently playing notes per-port. We'll be nice and not send overlapping notes
    /// or note-offs without a corresponding note-on.
    ///
    /// TODO: Do send overlapping notes with different note IDs if the plugin claims to support it.
    active_notes: Vec<Vec<Note>>,
    /// The CLAP note ID for the next note on event.
    next_note_id: i32,
}

/// A helper to generate random parameter automation and modulation events in a couple different
/// ways to stress test a plugin's parameter handling.
pub struct ParamFuzzer<'a> {
    /// The parameter info to generate random events for.
    params: &'a ParamInfo,

    /// Whether to snap generated parameter values to the parameter's minimum or maximum value.
    snap_to_bounds: bool,

    /// The range for the next event's timing relative to the previous event.
    /// This will be capped to 0 when generating events
    sample_offset_range: RangeInclusive<i32>,
}

/// A helper to generate random transport events in a couple different ways to stress test a plugin's transport handling.
pub struct TransportFuzzer {
    probability_change: f64,
}

/// The description of an active note in the [`NoteGenerator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Note {
    pub key: i16,
    pub channel: i16,
    pub note_id: i32,
    /// Whether the note has been choked, we can only send this event once per note.
    pub choked: bool,
}

/// The different kinds of events we can generate. The event type chosen depends on the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteEventType {
    ClapNoteOn,
    ClapNoteOff,
    ClapNoteChoke,
    ClapNoteExpression,
    MidiNoteOn,
    MidiNoteOff,
    MidiChannelPressure,
    MidiPolyKeyPressure,
    MidiPitchBend,
    MidiCc,
    MidiProgramChange,
    ParamValue,
    ParamModulation,
}

impl NoteEventType {
    const CLAP_EVENTS: &'static [NoteEventType] = &[
        NoteEventType::ClapNoteOn,
        NoteEventType::ClapNoteOff,
        NoteEventType::ClapNoteChoke,
        NoteEventType::ClapNoteExpression,
    ];
    const MIDI_EVENTS: &'static [NoteEventType] = &[
        NoteEventType::MidiNoteOn,
        NoteEventType::MidiNoteOff,
        NoteEventType::MidiChannelPressure,
        NoteEventType::MidiPolyKeyPressure,
        NoteEventType::MidiPitchBend,
        NoteEventType::MidiCc,
        NoteEventType::MidiProgramChange,
    ];
    const PARAM_EVENTS: &'static [NoteEventType] = &[NoteEventType::ParamValue, NoteEventType::ParamModulation];

    /// Get a slice containing the event types supported by a plugin. Returns None if the plugin
    /// supports neither CLAP note events nor MIDI.
    pub fn supported_types(
        supports_clap_note_events: bool,
        supports_midi_events: bool,
        supports_param_events: bool,
    ) -> impl Iterator<Item = NoteEventType> {
        let clap = if supports_clap_note_events {
            Self::CLAP_EVENTS
        } else {
            &[]
        };
        let midi = if supports_midi_events { Self::MIDI_EVENTS } else { &[] };
        let param = if supports_param_events { Self::PARAM_EVENTS } else { &[] };

        clap.iter().chain(midi.iter()).chain(param.iter()).copied()
    }
}

impl Note {
    fn random(prng: &mut Pcg32) -> Self {
        Note {
            key: prng.random_range(0..128),
            channel: prng.random_range(0..16),
            note_id: prng.random_range(0..100),
            choked: false,
        }
    }
}

impl<'a> NoteGenerator<'a> {
    /// Create a new random note generator based on a plugin's note port configuration. By default
    /// these events are consistent, meaning that there are no things like note offs before a note
    /// on, duplicate note ons, or note expressions for notes that don't exist.
    pub fn new(config: &'a NotePortConfig) -> Self {
        let num_inputs = config.inputs.len();

        NoteGenerator {
            config,
            params: None,

            only_consistent_events: true,

            // The range for the next event's timing relative to the `current_sample`. This will be
            // capped at 0, so there's a ~58% chance the next event occurs on the same time interval as
            // the previous event.
            sample_offset_range: -6..=5,

            active_notes: vec![Vec::new(); num_inputs],
            next_note_id: 0,
        }
    }

    /// Set the range for the next event's timing relative to the previous event. This will be
    /// clamped to 0 when generating events.
    pub fn with_sample_offset_range(mut self, range: RangeInclusive<i32>) -> Self {
        self.sample_offset_range = range;
        self
    }

    /// Set the parameter info to generate random polyphonic automation and modulation events for.
    pub fn with_params(mut self, params: &'a ParamInfo) -> Self {
        self.params = Some(params);
        self
    }

    /// Allow inconsistent events, like note off events without a corresponding note on and note
    /// expression events for notes that aren't currently playing.
    pub fn with_inconsistent_events(mut self) -> Self {
        self.only_consistent_events = false;
        self
    }

    /// Fill an event queue with random events for the next `num_samples` samples. This does not
    /// clear the event queue. If the queue was not empty, then this will do a stable sort after
    /// inserting _all_ events.
    pub fn generate_events(&mut self, prng: &mut Pcg32, num_samples: u32) -> Vec<Event> {
        let mut events = vec![];
        let mut sample = prng.random_range(self.sample_offset_range.clone()).max(0) as u32;

        while sample < num_samples {
            let Some(event) = self.generate_event(prng, sample) else {
                break;
            };

            events.push(event);
            sample += prng.random_range(self.sample_offset_range.clone()).max(0) as u32;
        }

        events
    }

    /// Generate a random note event for one of the plugin's note ports depending on the port's
    /// capabilities. Returns an error if the plugin doesn't have any note ports or if the note
    /// ports don't support either MIDI or CLAP note events.
    pub fn generate_event(&mut self, prng: &mut Pcg32, time_offset: u32) -> Option<Event> {
        if self.config.inputs.is_empty() {
            return None;
        }

        let note_port_idx = prng.random_range(0..self.config.inputs.len());

        // We could do this in a smarter way to avoid generating impossible event types (like a note
        // off when there are no active notes), but this should work fine.
        for _ in 0..1024 {
            // We'll ignore the prefered note dialect and pick from all of the supported note dialects.
            // The plugin may get a CLAP note on and a MIDI note off if it supports both of those things
            let event_type = NoteEventType::supported_types(
                self.config.inputs[note_port_idx].supports_clap(),
                self.config.inputs[note_port_idx].supports_midi(),
                self.params.is_some(),
            )
            .choose(prng)?;

            match event_type {
                NoteEventType::ClapNoteOn => {
                    let note = if self.only_consistent_events {
                        let note = Note {
                            note_id: self.next_note_id,
                            ..Note::random(prng)
                        };

                        if self.active_notes[note_port_idx].contains(&note) {
                            continue;
                        }
                        self.active_notes[note_port_idx].push(note);
                        self.next_note_id = self.next_note_id.wrapping_add(1);

                        note
                    } else {
                        Note::random(prng)
                    };

                    let velocity = prng.random_range(0.0..=1.0);
                    return Some(Event::Note(clap_event_note {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_ON,
                            // TODO: There's a live flag here, should we also randomize this?
                            flags: 0,
                        },
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        velocity,
                    }));
                }
                NoteEventType::ClapNoteOff => {
                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx].remove(note_idx)
                    } else {
                        Note::random(prng)
                    };

                    let velocity = prng.random_range(0.0..=1.0);
                    return Some(Event::Note(clap_event_note {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_OFF,
                            flags: 0,
                        },
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        velocity,
                    }));
                }
                NoteEventType::ClapNoteChoke => {
                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        // A note can only be choked once
                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        let note = &mut self.active_notes[note_port_idx][note_idx];
                        if note.choked {
                            continue;
                        }
                        note.choked = true;

                        *note
                    } else {
                        Note::random(prng)
                    };

                    // Does a velocity make any sense here? Probably not.
                    let velocity = prng.random_range(0.0..=1.0);
                    return Some(Event::Note(clap_event_note {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_CHOKE,
                            flags: 0,
                        },
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        velocity,
                    }));
                }
                NoteEventType::ClapNoteExpression => {
                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx][note_idx]
                    } else {
                        Note::random(prng)
                    };

                    let expression_id = prng.random_range(CLAP_NOTE_EXPRESSION_VOLUME..=CLAP_NOTE_EXPRESSION_PRESSURE);
                    let value_range = match expression_id {
                        CLAP_NOTE_EXPRESSION_VOLUME => 0.0..=4.0,
                        CLAP_NOTE_EXPRESSION_TUNING => -128.0..=128.0,
                        _ => 0.0..=1.0,
                    };
                    let value = prng.random_range(value_range);

                    return Some(Event::NoteExpression(clap_event_note_expression {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note_expression>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_EXPRESSION,
                            flags: 0,
                        },
                        expression_id,
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        value,
                    }));
                }
                NoteEventType::MidiNoteOn => {
                    let note = if self.only_consistent_events {
                        let note = Note {
                            note_id: self.next_note_id,
                            ..Note::random(prng)
                        };

                        if self.active_notes[note_port_idx].contains(&note) {
                            continue;
                        }
                        self.active_notes[note_port_idx].push(note);
                        self.next_note_id = self.next_note_id.wrapping_add(1);

                        note
                    } else {
                        Note::random(prng)
                    };

                    let velocity = prng.random_range(0.0..=1.0);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [
                            midi::NOTE_ON | note.channel as u8,
                            note.key as u8,
                            (velocity * 127.0f32).round().clamp(0.0, 127.0) as u8,
                        ],
                    }));
                }
                NoteEventType::MidiNoteOff => {
                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx].remove(note_idx)
                    } else {
                        Note::random(prng)
                    };

                    let velocity = prng.random_range(0.0..=1.0);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [
                            midi::NOTE_OFF | note.channel as u8,
                            note.key as u8,
                            (velocity * 127.0f32).round().clamp(0.0, 127.0) as u8,
                        ],
                    }));
                }
                NoteEventType::MidiChannelPressure => {
                    let channel = prng.random_range(0..16);
                    let pressure = prng.random_range(0..128);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [midi::CHANNEL_KEY_PRESSURE | channel, pressure, 0],
                    }));
                }
                NoteEventType::MidiPolyKeyPressure => {
                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx][note_idx]
                    } else {
                        Note::random(prng)
                    };

                    let pressure = prng.random_range(0..128);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [
                            midi::POLYPHONIC_KEY_PRESSURE | note.channel as u8,
                            note.key as u8,
                            pressure,
                        ],
                    }));
                }
                NoteEventType::MidiPitchBend => {
                    // May as well just generate the two bytes directly instead of doing fancy things
                    let channel = prng.random_range(0..16);
                    let byte1 = prng.random_range(0..128);
                    let byte2 = prng.random_range(0..128);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [midi::PITCH_BEND_CHANGE | channel, byte1, byte2],
                    }));
                }
                NoteEventType::MidiCc => {
                    let channel = prng.random_range(0..16);
                    let cc = prng.random_range(0..128);
                    let value = prng.random_range(0..128);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [midi::CONTROL_CHANGE | channel, cc, value],
                    }));
                }
                NoteEventType::MidiProgramChange => {
                    let channel = prng.random_range(0..16);
                    let program_number = prng.random_range(0..128);
                    return Some(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [midi::PROGRAM_CHANGE | channel, program_number, 0],
                    }));
                }
                NoteEventType::ParamValue => {
                    let Some(params) = self.params else {
                        continue;
                    };

                    let Some((param_id, param)) = params
                        .iter()
                        .filter(|(_, param)| !param.readonly() && !param.hidden() && param.poly_automatable())
                        .choose(prng)
                    else {
                        continue;
                    };

                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx][note_idx]
                    } else {
                        Note::random(prng)
                    };

                    return Some(Event::ParamValue(clap_event_param_value {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_param_value>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_PARAM_VALUE,
                            flags: 0,
                        },
                        param_id: *param_id,
                        cookie: param.cookie,
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        value: ParamFuzzer::random_value(param, prng),
                    }));
                }
                NoteEventType::ParamModulation => {
                    let Some(params) = self.params else {
                        continue;
                    };

                    let Some((param_id, param)) = params
                        .iter()
                        .filter(|(_, param)| !param.readonly() && !param.hidden() && param.poly_modulatable())
                        .choose(prng)
                    else {
                        continue;
                    };

                    let note = if self.only_consistent_events {
                        if self.active_notes[note_port_idx].is_empty() {
                            continue;
                        }

                        let note_idx = prng.random_range(0..self.active_notes[note_port_idx].len());
                        self.active_notes[note_port_idx][note_idx]
                    } else {
                        Note::random(prng)
                    };

                    return Some(Event::ParamValue(clap_event_param_value {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_param_value>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_PARAM_VALUE,
                            flags: 0,
                        },
                        param_id: *param_id,
                        cookie: param.cookie,
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        value: ParamFuzzer::random_modulation(param, prng),
                    }));
                }
            }
        }

        panic!("Unable to generate a random note event after 1024 tries");
    }

    pub fn stop_all_voices(&mut self, time_offset: u32) -> Vec<Event> {
        let mut events = vec![];
        for (note_port_idx, active_notes) in self.active_notes.iter_mut().enumerate() {
            let supports_clap = self.config.inputs[note_port_idx].supports_clap();

            for note in active_notes.drain(..) {
                if supports_clap {
                    events.push(Event::Note(clap_event_note {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_note>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_NOTE_OFF,
                            flags: 0,
                        },
                        note_id: note.note_id,
                        port_index: note_port_idx as i16,
                        channel: note.channel,
                        key: note.key,
                        velocity: 0.0,
                    }));
                } else {
                    events.push(Event::Midi(clap_event_midi {
                        header: clap_event_header {
                            size: std::mem::size_of::<clap_event_midi>() as u32,
                            time: time_offset,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: CLAP_EVENT_MIDI,
                            flags: 0,
                        },
                        port_index: note_port_idx as u16,
                        data: [midi::NOTE_OFF | note.channel as u8, note.key as u8, 0],
                    }));
                }
            }
        }

        events
    }

    #[allow(unused)]
    pub fn reset(&mut self) {
        self.next_note_id = 0;
        for active_notes in &mut self.active_notes {
            active_notes.clear();
        }
    }
}

impl<'a> ParamFuzzer<'a> {
    /// Create a new parameter fuzzer. This ignores parameters that are readonly or hidden.
    pub fn new(params: &'a ParamInfo) -> Self {
        ParamFuzzer {
            params,
            snap_to_bounds: false,
            sample_offset_range: -10..=20,
        }
    }

    pub fn snap_to_bounds(mut self) -> Self {
        self.snap_to_bounds = true;
        self
    }

    /// Fill an event queue with random parameter change events for the next `num_samples` samples.
    /// This does not clear the event queue. If the queue was not empty, then this will do a stable
    /// sort after inserting _all_ events.
    ///
    /// Unlike [`ParamFuzzer::randomize_params_at`], this generates [`Event::ParamMod`] events as well as
    /// generating events at random irregular unsynchronized (between different parameters) intervals.
    pub fn generate_events(&self, prng: &mut Pcg32, num_samples: u32) -> Vec<Event> {
        let mut events = vec![];
        let mut sample = prng.random_range(self.sample_offset_range.clone()).max(0) as u32;
        while sample < num_samples {
            let Some(event) = self.generate_event(prng) else {
                break;
            };

            events.push(event);
            sample += prng.random_range(self.sample_offset_range.clone()).max(0) as u32;
        }

        events
    }

    /// Generate a single random parameter change event for one of the plugin's parameters.
    pub fn generate_event(&self, prng: &mut Pcg32) -> Option<Event> {
        let (param_id, param_info) = self
            .params
            .iter()
            .filter(|(_, info)| !info.readonly() && !info.hidden())
            .choose(prng)?;

        if !self.snap_to_bounds && param_info.modulatable() && prng.random_bool(0.5) {
            Some(Event::ParamValue(clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id: *param_id,
                cookie: param_info.cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: ParamFuzzer::random_modulation(param_info, prng),
            }))
        } else {
            Some(Event::ParamValue(clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: if param_info.automatable() {
                        0
                    } else {
                        CLAP_EVENT_IS_LIVE
                    },
                },
                param_id: *param_id,
                cookie: param_info.cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: ParamFuzzer::random_value(param_info, prng),
            }))
        }
    }

    /// Randomize _all_ parameters at a certain sample index using **automation**, returning an
    /// iterator yielding automation events for all parameters.
    pub fn randomize_params_at(&'a self, prng: &'a mut Pcg32, time_offset: u32) -> impl Iterator<Item = Event> + 'a {
        self.params.iter().filter_map(move |(param_id, param_info)| {
            // We can send parameter changes for parameters that are not automatable:
            //
            // > The host can send live user changes for this parameter regardless of this flag.
            if param_info.readonly() || param_info.hidden() {
                return None;
            }

            let value = if self.snap_to_bounds {
                if prng.random_bool(0.5) {
                    *param_info.range.start()
                } else {
                    *param_info.range.end()
                }
            } else {
                ParamFuzzer::random_value(param_info, prng)
            };

            Some(Event::ParamValue(clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: time_offset,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: if param_info.automatable() {
                        0
                    } else {
                        CLAP_EVENT_IS_LIVE
                    },
                },
                param_id: *param_id,
                cookie: param_info.cookie,
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            }))
        })
    }

    pub fn random_value(param: &Param, prng: &mut Pcg32) -> f64 {
        if param.stepped() {
            // We already confirmed that the range starts and ends in an integer when
            // constructing the parameter info
            prng.random_range(param.range.clone()).round()
        } else {
            prng.random_range(param.range.clone())
        }
    }

    pub fn random_modulation(param: &Param, prng: &mut Pcg32) -> f64 {
        let range = (param.range.end() - param.range.start()).abs() * 0.5;

        if param.stepped() {
            prng.random_range(-range..=range).round()
        } else {
            prng.random_range(-range..=range)
        }
    }
}

impl TransportFuzzer {
    /// Create a new transport fuzzer.
    pub fn new() -> Self {
        TransportFuzzer {
            probability_change: 0.2,
        }
    }

    /// Mutates an existing transport state.
    pub fn mutate(&mut self, prng: &mut Pcg32, transport: &mut TransportState) {
        // toggle playback state with 20% probability
        if prng.random_bool(self.probability_change) {
            transport.is_playing = !transport.is_playing;
        }

        // toggle recording state with 20% probability
        if prng.random_bool(self.probability_change) {
            transport.is_recording = !transport.is_recording;
        }

        // change time signature with 20% probability
        if prng.random_bool(self.probability_change) {
            if prng.random_bool(0.5) {
                transport.time_signature = None;
            } else {
                transport.time_signature = Some((prng.random_range(1..=16), prng.random_range(1..=4)));
            }
        }

        // change tempo (instanteous) with 20% probability
        if prng.random_bool(self.probability_change) {
            if prng.random_bool(0.5) {
                transport.tempo = None;
            } else {
                transport.tempo = Some((prng.random_range(40.0..=480.0), 0.0));
            }
        }

        // change tempo (ramp) with 40% probability
        if let Some((tempo, ramp)) = &mut transport.tempo
            && prng.random_bool(self.probability_change)
        {
            // safeguard to prevent extremely low tempos
            if *tempo < 40.0 {
                *tempo = 40.0;
                *ramp = prng.random_range(0.0..=0.01);
            }

            *ramp = prng.random_range(-0.01..=0.01);
        }

        // seek to a new position with 10% probability
        if prng.random_bool(self.probability_change) {
            if prng.random_bool(0.5) {
                transport.position_seconds = None;
            } else {
                transport.position_seconds = Some(prng.random_range(0.0..=60.0));
            }

            if prng.random_bool(0.5) {
                transport.position_beats = None;
            } else {
                transport.position_beats = Some(prng.random_range(0.0..=240.0));
            }

            if prng.random_bool(0.5) {
                transport.sample_pos = None;
            } else {
                // we can only seek forward
                transport.sample_pos = Some(transport.sample_pos.unwrap_or(0) + prng.random_range(0..=100_000) as u64);
            }
        }

        if transport.tempo.is_none() {
            transport.position_beats = None;
        }
    }
}

pub fn random_layout_requests(config: &AudioPortConfig, prng: &mut Pcg32) -> Vec<AudioPortsRequest<'static>> {
    fn random_request_info(prng: &mut Pcg32) -> AudioPortsRequestInfo<'static> {
        match prng.random_range(0..=4) {
            0 => AudioPortsRequestInfo::Mono,
            1 => AudioPortsRequestInfo::Stereo,
            2 => AudioPortsRequestInfo::Untyped {
                channel_count: prng.random_range(1..=16),
            },
            3 => {
                const AMBISONIC_ACN_SN3D: clap_ambisonic_config = clap_ambisonic_config {
                    ordering: CLAP_AMBISONIC_ORDERING_ACN,
                    normalization: CLAP_AMBISONIC_NORMALIZATION_SN3D,
                };

                const AMBISONIC_FUMA_MAXN: clap_ambisonic_config = clap_ambisonic_config {
                    ordering: CLAP_AMBISONIC_ORDERING_FUMA,
                    normalization: CLAP_AMBISONIC_NORMALIZATION_MAXN,
                };

                let channel_count = prng.random_range(1..=4u32).pow(2);
                let is_acn_sn3d = prng.random_bool(0.5);

                AudioPortsRequestInfo::Ambisonic {
                    channel_count,
                    config: if is_acn_sn3d {
                        &AMBISONIC_ACN_SN3D
                    } else {
                        &AMBISONIC_FUMA_MAXN
                    },
                }
            }
            _ => {
                const SURROUND_MAPS: &[&[u8]] = &[
                    &[0, 1],              // Stereo; FL FR
                    &[0, 2, 1],           // 3.0;    FL FC FR
                    &[0, 2, 1, 3],        // 3.1;    FL FC FR LFE
                    &[0, 2, 1, 8],        // 4.0;    FL FC FR BC
                    &[0, 2, 1, 8, 3],     // 4.1;    FL FC FR BC LFE
                    &[0, 2, 1, 9, 10],    // 5.0;    FL FC FR SL SR
                    &[0, 2, 1, 9, 10, 3], // 5.1;    FL FC FR SL SR LFE
                ];

                AudioPortsRequestInfo::Surround {
                    channel_map: SURROUND_MAPS.choose(prng).unwrap(),
                }
            }
        }
    }

    let mut requests = vec![];

    for index in 0..config.inputs.len() {
        if prng.random_bool(0.1) {
            // skip request for some inputs
            continue;
        }

        requests.push(AudioPortsRequest {
            is_input: true,
            port_index: index as u32,
            request_info: random_request_info(prng),
        });
    }

    for index in 0..config.outputs.len() {
        if prng.random_bool(0.1) {
            // skip request for some outputs
            continue;
        }

        requests.push(AudioPortsRequest {
            is_input: false,
            port_index: index as u32,
            request_info: random_request_info(prng),
        });
    }

    requests
}
