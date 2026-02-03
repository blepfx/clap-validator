//! A tracing layer that outputs Chrome JSON trace files.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;
use tracing::Subscriber;
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

pub struct ChromeJsonLayer<S> {
    start: Instant,
    writer: Mutex<BufWriter<File>>,
    _inner: PhantomData<S>,
}

impl<S> ChromeJsonLayer<S> {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let mut file = BufWriter::new(File::create(path).unwrap());
        file.write_all(b"[\n").unwrap();

        Self {
            start: Instant::now(),
            writer: Mutex::new(file),
            _inner: PhantomData,
        }
    }

    fn emit(&self, event: Trace) {
        let mut writer = self.writer.lock().unwrap();
        serde_json::to_writer(&mut *writer, &event).unwrap();
        writer.write_all(b",\n").unwrap();
        writer.flush().unwrap();
    }
}

impl<S> Layer<S> for ChromeJsonLayer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, ctx: Context<'_, S>) {
        let time = self.start.elapsed().as_micros();

        let mut data = TraceArgs::default();
        attrs.record(&mut data);

        self.emit(Trace {
            name: attrs.metadata().name(),
            cat: std::thread::current().name().unwrap_or("?"),
            ts: time,
            id: 1,
            pid: 1,
            ph: "b",
            args: &data,
        });

        ctx.span(_id).unwrap().extensions_mut().insert(data);
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let time = self.start.elapsed().as_micros();

        let mut args = TraceArgs::default();
        event.record(&mut args);

        self.emit(Trace {
            name: args.values.get("message").map(|s| s.as_str()).unwrap_or("?"),
            cat: std::thread::current().name().unwrap_or("?"),
            ts: time,
            id: 1,
            pid: 1,
            ph: "n",
            args: &args,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).unwrap();
        if let Some(args) = span.extensions_mut().get_mut::<TraceArgs>() {
            values.record(args);
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let time = self.start.elapsed().as_micros();
        let span = ctx.span(&id).unwrap();

        if let Some(args) = span.extensions().get::<TraceArgs>() {
            self.emit(Trace {
                name: span.name(),
                cat: std::thread::current().name().unwrap_or("?"),
                ts: time,
                id: 1,
                pid: 1,
                ph: "e",
                args,
            });
        }
    }
}

#[derive(serde::Serialize)]
struct Trace<'a> {
    name: &'a str,
    cat: &'a str,
    ts: u128,
    id: u64,
    pid: u64,
    ph: &'a str,
    args: &'a TraceArgs,
}

#[derive(serde::Serialize, Default)]
#[serde(transparent)]
struct TraceArgs {
    values: BTreeMap<&'static str, String>,
}

impl tracing::field::Visit for TraceArgs {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.values.insert(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.values.insert(field.name(), value.to_string());
    }
}
