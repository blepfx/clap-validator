use crate::debug::{Recordable, Recorder, Span, fail_test, record};
use crate::plugin::util::{CHECK_POINTER, Proxy, Proxyable};
use clap_sys::events::*;
use std::fmt::Debug;
use std::sync::Mutex;

#[derive(Debug)]
pub struct InputEventQueue(Mutex<Vec<Event>>);

#[derive(Debug)]
pub struct OutputEventQueue(Mutex<Vec<Event>>);

/// An event sent to or from the plugin. This uses an enum to make the implementation simple and
/// correct at the cost of more wasteful memory usage.
#[derive(Debug, Clone)]
#[repr(C, align(8))]
pub enum Event {
    /// `CLAP_EVENT_NOTE_ON`, `CLAP_EVENT_NOTE_OFF`, `CLAP_EVENT_NOTE_CHOKE`, or `CLAP_EVENT_NOTE_END`.
    Note(clap_event_note),
    /// `CLAP_EVENT_NOTE_EXPRESSION`.
    NoteExpression(clap_event_note_expression),
    /// `CLAP_EVENT_MIDI`.
    Midi(clap_event_midi),
    /// `CLAP_EVENT_MIDI2`.
    Midi2(clap_event_midi2),
    /// `CLAP_EVENT_MIDI_SYSEX`.
    Sysex(clap_event_midi_sysex),
    /// `CLAP_EVENT_PARAM_VALUE`.
    ParamValue(clap_event_param_value),
    /// `CLAP_EVENT_PARAM_MOD`.
    ParamMod(clap_event_param_mod),
    /// `CLAP_EVENT_TRANSPORT`.
    Transport(clap_event_transport),
    /// An unhandled event type. This is only used when the plugin outputs an event we don't handle
    /// or recognize.
    Unknown(clap_event_header),
}

impl Proxyable for InputEventQueue {
    type Vtable = clap_input_events;

    fn init(&self) -> Self::Vtable {
        clap_input_events {
            ctx: CHECK_POINTER,
            size: Some(Self::size),
            get: Some(Self::get),
        }
    }
}

impl Proxyable for OutputEventQueue {
    type Vtable = clap_output_events;

    fn init(&self) -> Self::Vtable {
        clap_output_events {
            ctx: CHECK_POINTER,
            try_push: Some(Self::try_push),
        }
    }
}

impl InputEventQueue {
    pub fn new() -> Proxy<Self> {
        Proxy::new(Self(Mutex::new(Vec::new())))
    }

    pub fn clear(&self) {
        let mut events = self.0.lock().unwrap();
        events.clear();
    }

    pub fn last_event_time(&self) -> Option<u32> {
        let events = self.0.lock().unwrap();
        events.last().map(|event| event.header().time)
    }

    pub fn add_events(&self, extend: impl IntoIterator<Item = Event>) {
        let mut events = self.0.lock().unwrap();
        let is_empty = events.is_empty();
        events.extend(extend);
        if !is_empty {
            events.sort_by_key(|event| event.header().time);
        }
    }

    unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
        let span = Span::begin("clap_input_events::size", ());

        let state = unsafe {
            Proxy::<Self>::from_vtable(list).unwrap_or_else(|e| {
                fail_test!("clap_input_events::size: {}", e);
            })
        };

        if Proxy::vtable(&state).ctx != CHECK_POINTER {
            fail_test!("clap_input_events::size: plugin messed with the 'ctx' pointer");
        }

        let events = state.0.lock().unwrap();
        span.finish(record!(result: events.len() as u32));
        events.len() as u32
    }

    unsafe extern "C" fn get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
        let span = Span::begin(
            "clap_input_events::get",
            record! {
                index: index
            },
        );

        let state = unsafe {
            Proxy::<Self>::from_vtable(list).unwrap_or_else(|e| {
                fail_test!("clap_input_events::size: {}", e);
            })
        };

        if Proxy::vtable(&state).ctx != CHECK_POINTER {
            fail_test!("clap_input_events::size: plugin messed with the 'ctx' pointer");
        }

        let events = state.0.lock().unwrap();
        match events.get(index as usize) {
            Some(event) => {
                span.finish(record!(event: event));
                event.header()
            }
            None => {
                log::warn!(
                    "The plugin tried to get an out of bounds event with index {index} ({} total events)",
                    events.len()
                );
                std::ptr::null()
            }
        }
    }
}

impl OutputEventQueue {
    pub fn new() -> Proxy<Self> {
        Proxy::new(Self(Mutex::new(Vec::new())))
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    pub fn read(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }

    unsafe extern "C" fn try_push(list: *const clap_output_events, event: *const clap_event_header) -> bool {
        let span = Span::begin("clap_output_events::try_push", ());
        let state = unsafe {
            Proxy::<Self>::from_vtable(list).unwrap_or_else(|e| {
                fail_test!("clap_output_events::try_push: {}", e);
            })
        };

        if Proxy::vtable(&state).ctx != CHECK_POINTER {
            fail_test!("clap_output_events::try_push: plugin messed with the 'ctx' pointer");
        }

        if event.is_null() {
            fail_test!("clap_output_events::try_push: 'event' pointer is null");
        }

        // The monotonicity of the plugin's event insertion order is checked as part of the output
        // consistency checks

        let event = unsafe { Event::from_raw(event) };
        span.finish(record!(event: event));
        state.0.lock().unwrap().push(event);
        true
    }
}

impl Event {
    /// Parse an event from a plugin-provided pointer. Returns an error if the pointer as a null pointer
    pub unsafe fn from_raw(ptr: *const clap_event_header) -> Self {
        assert!(!ptr.is_null(), "Null pointer provided for 'clap_event_header'.");

        unsafe {
            match ((*ptr).space_id, ((*ptr).type_)) {
                (
                    CLAP_CORE_EVENT_SPACE_ID,
                    CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE | CLAP_EVENT_NOTE_END,
                ) => Event::Note(*(ptr as *const clap_event_note)),
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_EXPRESSION) => {
                    Event::NoteExpression(*(ptr as *const clap_event_note_expression))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE) => {
                    Event::ParamValue(*(ptr as *const clap_event_param_value))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_MOD) => {
                    Event::ParamMod(*(ptr as *const clap_event_param_mod))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI) => Event::Midi(*(ptr as *const clap_event_midi)),
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI2) => Event::Midi2(*(ptr as *const clap_event_midi2)),
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI_SYSEX) => {
                    Event::Sysex(*(ptr as *const clap_event_midi_sysex))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_TRANSPORT) => {
                    Event::Transport(*(ptr as *const clap_event_transport))
                }
                (_, _) => Event::Unknown(*ptr),
            }
        }
    }

    /// Get a a reference to the event's header.
    pub fn header(&self) -> &clap_event_header {
        match self {
            Event::Note(event) => &event.header,
            Event::NoteExpression(event) => &event.header,
            Event::ParamValue(event) => &event.header,
            Event::ParamMod(event) => &event.header,
            Event::Midi(event) => &event.header,
            Event::Midi2(event) => &event.header,
            Event::Sysex(event) => &event.header,
            Event::Transport(event) => &event.header,
            Event::Unknown(header) => header,
        }
    }
}

impl Recordable for Event {
    fn record(&self, record: &mut dyn Recorder) {
        record.record(
            "type",
            match (self.header().space_id, self.header().type_) {
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_ON) => "CLAP_EVENT_NOTE_ON",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF) => "CLAP_EVENT_NOTE_OFF",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_CHOKE) => "CLAP_EVENT_NOTE_CHOKE",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_END) => "CLAP_EVENT_NOTE_END",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_EXPRESSION) => "CLAP_EVENT_NOTE_EXPRESSION",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_GESTURE_BEGIN) => "CLAP_EVENT_PARAM_GESTURE_BEGIN",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_GESTURE_END) => "CLAP_EVENT_PARAM_GESTURE_END",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE) => "CLAP_EVENT_PARAM_VALUE",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_MOD) => "CLAP_EVENT_PARAM_MOD",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI) => "CLAP_EVENT_MIDI",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI2) => "CLAP_EVENT_MIDI2",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI_SYSEX) => "CLAP_EVENT_MIDI_SYSEX",
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_TRANSPORT) => "CLAP_EVENT_TRANSPORT",
                (_, _) => "?",
            },
        );

        record.record("space_id", self.header().space_id);
        record.record("type_id", self.header().type_);
        record.record("time", self.header().time);
        record.record("flags.is_live", self.header().flags & CLAP_EVENT_IS_LIVE != 0);
        record.record("flags.dont_record", self.header().flags & CLAP_EVENT_DONT_RECORD != 0);

        match self {
            Event::Note(event) => {
                record.record("note_id", event.note_id);
                record.record("key", event.key);
                record.record("port", event.port_index);
                record.record("channel", event.channel);
                record.record("velocity", event.velocity);
            }
            Event::NoteExpression(event) => {
                record.record("note_id", event.note_id);
                record.record("port_index", event.port_index);
                record.record("key", event.key);
                record.record("channel", event.channel);

                record.record(
                    "expression",
                    match event.expression_id {
                        CLAP_NOTE_EXPRESSION_VOLUME => "CLAP_NOTE_EXPRESSION_VOLUME",
                        CLAP_NOTE_EXPRESSION_PAN => "CLAP_NOTE_EXPRESSION_PAN",
                        CLAP_NOTE_EXPRESSION_TUNING => "CLAP_NOTE_EXPRESSION_TUNING",
                        CLAP_NOTE_EXPRESSION_VIBRATO => "CLAP_NOTE_EXPRESSION_VIBRATO",
                        CLAP_NOTE_EXPRESSION_BRIGHTNESS => "CLAP_NOTE_EXPRESSION_BRIGHTNESS",
                        CLAP_NOTE_EXPRESSION_PRESSURE => "CLAP_NOTE_EXPRESSION_PRESSURE",
                        CLAP_NOTE_EXPRESSION_EXPRESSION => "CLAP_NOTE_EXPRESSION_EXPRESSION",
                        _ => "?",
                    },
                );

                record.record("expression_id", event.expression_id);
                record.record("value", event.value);
            }
            Event::ParamValue(event) => {
                record.record("param_id", event.param_id);
                record.record("value", event.value);
                record.record("note_id", event.note_id);
                record.record("port_index", event.port_index);
                record.record("key", event.key);
                record.record("channel", event.channel);
            }
            Event::ParamMod(event) => {
                record.record("param_id", event.param_id);
                record.record("amount", event.amount);
                record.record("note_id", event.note_id);
                record.record("port_index", event.port_index);
                record.record("key", event.key);
                record.record("channel", event.channel);
            }
            Event::Midi(event) => {
                record.record("port_index", event.port_index);
                record.record("raw", format_args!("{:X?}", event.data));
            }
            Event::Midi2(event) => {
                record.record("port_index", event.port_index);
                record.record("raw", format_args!("{:X?}", event.data));
            }
            Event::Sysex(event) => {
                record.record("port_index", event.port_index);

                if event.buffer.is_null() {
                    record.record("data", "<null>");
                } else {
                    record.record(
                        "data",
                        format_args!("{:X?}", unsafe {
                            std::slice::from_raw_parts(event.buffer, event.size as usize)
                        }),
                    );
                }
            }

            Event::Transport(event) => {
                record.record("transport", event);
            }
            Event::Unknown(..) => {}
        }
    }
}
