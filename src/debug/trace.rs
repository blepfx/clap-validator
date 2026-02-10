//! A tracing layer that outputs Chrome JSON trace files.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tracing_core::span::{Attributes, Current, Id, Record};
use tracing_core::{Metadata, Subscriber};

pub struct ChromeJsonSubscriber {
    start: Instant,
    writer: Mutex<Result<File, std::io::Error>>,
}

impl ChromeJsonSubscriber {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let file = File::create(path).and_then(|mut f| {
            f.write_all(b"[\n")?;
            Ok(f)
        });

        Self {
            start: Instant::now(),
            writer: Mutex::new(file),
        }
    }

    pub fn check_error(&self) -> anyhow::Result<()> {
        match &*self.writer.lock().unwrap() {
            Ok(_) => Ok(()),
            Err(e) => anyhow::bail!("{}", e),
        }
    }

    fn emit(&self, event: TraceEvent) {
        thread_local! {
            static BUFFER: RefCell<Vec<u8>> = RefCell::new(Vec::with_capacity(256));
        }

        BUFFER.with_borrow_mut(|buffer| {
            buffer.clear();
            serde_json::to_writer(&mut *buffer, &event).unwrap();
            buffer.extend_from_slice(b",\n");

            let mut writer = self.writer.lock().unwrap();
            if let Ok(file) = &mut *writer
                && let Err(e) = file.write_all(buffer).and_then(|_| file.flush())
            {
                *writer = Err(e);
            }
        });
    }
}

impl Subscriber for ChromeJsonSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        THREAD_DATA.with_borrow_mut(|thread| {
            let id = Id::from_u64(NEXT_SPAN_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));

            let mut args = TraceArgs::new(span.metadata());
            span.record(&mut args);

            thread.spans.push(ThreadSpan {
                id: id.clone(),
                meta: span.metadata(),
                uses: 1,
                args,
            });

            id
        })
    }

    fn clone_span(&self, id: &Id) -> Id {
        THREAD_DATA.with_borrow_mut(|thread| {
            if let Some(span) = thread.span_mut(id) {
                span.uses += 1;
            }

            id.clone()
        })
    }

    fn try_close(&self, id: Id) -> bool {
        THREAD_DATA.with_borrow_mut(|thread| {
            if let Some(span) = thread.span_mut(&id) {
                span.uses -= 1;
                if span.uses == 0 {
                    thread.remove_span(&id);
                    return true;
                }
            }

            false
        })
    }

    fn record(&self, id: &Id, values: &Record<'_>) {
        THREAD_DATA.with_borrow_mut(|thread| {
            if let Some(span) = thread.span_mut(id) {
                values.record(&mut span.args);
            }
        });
    }

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn current_span(&self) -> Current {
        THREAD_DATA.with_borrow(|thread| {
            if let Some(span) = thread.spans.last() {
                Current::new(span.id.clone(), span.meta)
            } else {
                Current::none()
            }
        })
    }

    fn event(&self, event: &tracing::Event<'_>) {
        let time = self.start.elapsed().as_micros();

        THREAD_DATA.with_borrow_mut(|thread| {
            let mut args = TraceArgs::new(event.metadata());
            event.record(&mut args);

            self.emit(TraceEvent {
                name: args.values.get("message").map(|s| s.as_str()).unwrap_or("?"),
                cat: &thread.thread,
                args: &args,
                ts: time,
                id: 1,
                pid: 1,
                ph: "n",
            });
        });
    }

    fn enter(&self, span: &Id) {
        let time = self.start.elapsed().as_micros();

        THREAD_DATA.with_borrow_mut(|thread| {
            if let Some(span) = thread.span(span) {
                self.emit(TraceEvent {
                    name: span.meta.name(),
                    cat: &thread.thread,
                    ts: time,
                    id: 1,
                    pid: 1,
                    ph: "b",
                    args: &span.args,
                });
            }
        });
    }

    fn exit(&self, span: &Id) {
        let time = self.start.elapsed().as_micros();

        THREAD_DATA.with_borrow_mut(|thread| {
            if let Some(span) = thread.span(span) {
                self.emit(TraceEvent {
                    name: span.meta.name(),
                    cat: &thread.thread,
                    ts: time,
                    id: 1,
                    pid: 1,
                    ph: "e",
                    args: &span.args,
                });
            }
        });
    }
}

static NEXT_SPAN_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static THREAD_DATA: RefCell<ThreadData> = RefCell::new(ThreadData::new());
}

/// Per-span tracer data
struct ThreadSpan {
    /// The span's (per-program unique) ID
    id: Id,

    /// Associated metadata
    meta: &'static Metadata<'static>,

    /// Recorded dynamic arguments
    args: TraceArgs,

    /// Reference count for the span, span is closed when this reaches 0
    uses: u32,
}

/// Per-thread tracer data
struct ThreadData {
    /// Current thread display name
    thread: Box<str>,

    /// A "stack" of currently active spans on this thread, in the order they were entered
    /// In most cases (FIFO) this is extremely fast, but this also allows for out-of-order/overlapping spans at the cost of O(n) lookups
    spans: Vec<ThreadSpan>,
}

impl ThreadData {
    pub fn new() -> Self {
        Self {
            spans: Vec::new(),
            thread: std::thread::current()
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{:?}", std::thread::current().id()))
                .into_boxed_str(),
        }
    }

    /// Find a span by its ID
    fn span(&self, id: &Id) -> Option<&ThreadSpan> {
        self.spans.iter().rfind(|span| &span.id == id)
    }

    /// Find a span by its ID
    fn span_mut(&mut self, id: &Id) -> Option<&mut ThreadSpan> {
        self.spans.iter_mut().rfind(|span| &span.id == id)
    }

    /// Remove a span by its ID, returning it if found
    fn remove_span(&mut self, id: &Id) -> Option<ThreadSpan> {
        self.spans
            .iter()
            .rposition(|span| &span.id == id)
            .map(|idx| self.spans.remove(idx))
    }
}

/// An event that is written to the file
#[derive(serde::Serialize)]
struct TraceEvent<'a> {
    name: &'a str,
    cat: &'a str,
    ts: u128,
    id: u64,
    pid: u64,
    ph: &'a str,
    args: &'a TraceArgs,
}

/// A helper object used to store and record event/span attribute data
#[derive(serde::Serialize, Default)]
#[serde(transparent)]
struct TraceArgs {
    values: BTreeMap<&'static str, String>,
}

impl TraceArgs {
    pub fn new(metadata: &'static Metadata<'static>) -> Self {
        let mut args = TraceArgs::default();

        if let Some(file) = metadata.file() {
            let line = metadata.line().unwrap_or(0);
            args.values.insert("location", format!("{file}:{line}"));
        }

        args
    }
}

impl tracing::field::Visit for TraceArgs {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.values.insert(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.values.insert(field.name(), value.to_string());
    }
}
