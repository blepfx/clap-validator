use crate::debug::{Recordable, Recorder};
use std::fs::File;
use std::io::{BufWriter, Error, Write};
use std::path::Path;
use std::time::Instant;

pub struct TraceWriter {
    file: Result<BufWriter<File>, Error>,
    start: Instant,
}

impl TraceWriter {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            file: File::create(path)
                .map(BufWriter::new)
                .and_then(|mut f| f.write_all(b"[\n").map(|_| f)),
            start: Instant::now(),
        }
    }

    pub fn check_error(&self) -> Result<(), &Error> {
        self.file.as_ref().map(|_| ())
    }

    pub fn write<T: Recordable>(&mut self, name: &str, cat: &str, tag: &str, args: &T) {
        if let Ok(file) = &mut self.file {
            let result = serde_json::to_writer(
                &mut *file,
                &TraceEvent {
                    name,
                    cat,
                    ts: self.start.elapsed().as_micros(),
                    id: 1,
                    pid: 1,
                    ph: tag,
                    args: &RecordableAsSerde(args),
                },
            )
            .map_err(Error::other)
            .and_then(|_| file.write_all(b",\n"))
            .and_then(|_| file.flush());

            if let Err(e) = result {
                self.file = Err(e);
            }
        }
    }
}

/// An event that is written to the file
#[derive(serde::Serialize)]
struct TraceEvent<'a, S: serde::Serialize> {
    name: &'a str,
    cat: &'a str,
    ts: u128,
    id: u64,
    pid: u64,
    ph: &'a str,
    args: &'a S,
}

struct DisplayAsSerde<'a>(&'a dyn std::fmt::Display);
struct RecordableAsSerde<T: Recordable>(T);

impl<T: Recordable> serde::Serialize for RecordableAsSerde<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        struct SerdeRecorder<S: serde::ser::SerializeMap> {
            serializer: S,
            state: Result<(), S::Error>,
        }

        impl<S: serde::ser::SerializeMap> Recorder for SerdeRecorder<S> {
            fn value(&mut self, name: &str, value: &dyn std::fmt::Display) {
                self.state = self.serializer.serialize_entry(name, &DisplayAsSerde(value));
            }

            fn group(&mut self, name: &str, record: &dyn Recordable) {
                self.state = self.serializer.serialize_entry(name, &RecordableAsSerde(record));
            }
        }

        let mut recorder = SerdeRecorder {
            serializer: serializer.serialize_map(None)?,
            state: Ok(()),
        };

        self.0.record(&mut recorder);

        recorder.state?;
        recorder.serializer.end()
    }
}

impl serde::Serialize for DisplayAsSerde<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self.0)
    }
}
