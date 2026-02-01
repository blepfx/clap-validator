use clap_sys::events::*;
use std::pin::Pin;
use std::sync::Mutex;

use crate::panic::fail_test;

/// An event queue that can be used as either an input queue or an output queue. This is always
/// allocated through a `Pin<Box<EventQueue>>` so the pointers are stable. The `VTable` type
/// argument should be either `clap_input_events` or `clap_output_events`.
#[derive(Debug)]
pub struct EventQueue {
    vtable_input: clap_input_events,
    vtable_output: clap_output_events,
    /// The actual event queue. Since we're going for correctness over performance, this uses a very
    /// suboptimal memory layout by just using an `enum` instead of doing fancy bit packing.
    events: Mutex<Vec<Event>>,
}

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

impl EventQueue {
    /// Construct a new event queue. This can be used as both an input and an output queue.
    pub fn new() -> Pin<Box<Self>> {
        let mut queue = Box::pin(Self {
            vtable_input: clap_input_events {
                // This is set to point to this object below
                ctx: std::ptr::null_mut(),
                size: Some(Self::size),
                get: Some(Self::get),
            },

            vtable_output: clap_output_events {
                // This is set to point to this object below
                ctx: std::ptr::null_mut(),
                try_push: Some(Self::try_push),
            },

            // Using a mutex here is obviously a terrible idea in a real host, but we're not a real
            // host
            events: Mutex::new(Vec::new()),
        });

        queue.vtable_input.ctx = &*queue as *const Self as *mut _;
        queue.vtable_output.ctx = &*queue as *const Self as *mut _;
        queue
    }

    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }

    pub fn add_events(&self, extend: impl IntoIterator<Item = Event>) {
        self.events.lock().unwrap().extend(extend);
    }

    pub fn sort_events(&self) {
        let mut events = self.events.lock().unwrap();
        events.sort_by_key(|event| event.header().time);
    }

    pub fn is_sorted(&self) -> bool {
        let events = self.events.lock().unwrap();
        events.is_sorted_by_key(|event| event.header().time)
    }

    pub fn read(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Get the vtable pointer for input events.
    pub fn vtable_input(self: &Pin<Box<Self>>) -> *const clap_input_events {
        &self.vtable_input
    }

    /// Get the vtable pointer for output events.
    pub fn vtable_output(self: &Pin<Box<Self>>) -> *const clap_output_events {
        &self.vtable_output
    }

    unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
        unsafe {
            if list.is_null() || (*list).ctx.is_null() {
                fail_test!("'clap_input_events::size' was called with a null pointer");
            }

            let this = &*((*list).ctx as *const Self);
            this.events.lock().unwrap().len() as u32
        }
    }

    unsafe extern "C" fn get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
        unsafe {
            if list.is_null() || (*list).ctx.is_null() {
                fail_test!("'clap_input_events::get' was called with a null pointer");
            }

            let this = &*((*list).ctx as *const Self);
            let events = this.events.lock().unwrap();
            match events.get(index as usize) {
                Some(event) => event.header(),
                None => {
                    log::warn!(
                        "The plugin tried to get an event with index {index} ({} total events)",
                        events.len()
                    );
                    std::ptr::null()
                }
            }
        }
    }

    unsafe extern "C" fn try_push(list: *const clap_output_events, event: *const clap_event_header) -> bool {
        unsafe {
            if list.is_null() || (*list).ctx.is_null() || event.is_null() {
                fail_test!("'clap_output_events::try_push' was called with a null pointer");
            }

            // The monotonicity of the plugin's event insertion order is checked as part of the output
            // consistency checks
            let this = &*((*list).ctx as *const Self);
            this.events.lock().unwrap().push(Event::from_raw(event));
            true
        }
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
            Event::Transport(event) => &event.header,
            Event::Unknown(header) => header,
        }
    }
}
