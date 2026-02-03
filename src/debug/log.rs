//! A tracing layer that logs events to standard output in compact human readable format.

use std::fmt::{Debug, Write};
use std::time::Instant;
use tracing::field::Field;
use yansi::Paint;

pub struct LogStderrLayer<S> {
    _inner: std::marker::PhantomData<S>,
    start: Instant,
}

impl<S> LogStderrLayer<S> {
    pub fn new() -> Self {
        Self {
            _inner: std::marker::PhantomData,
            start: Instant::now(),
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for LogStderrLayer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        thread_local! {
            static BUFFER: std::cell::RefCell<String> = std::cell::RefCell::new(String::with_capacity(256));
        }

        let elapsed = self.start.elapsed();
        let prefix = match *event.metadata().level() {
            tracing::Level::ERROR => "ERROR".red().bold(),
            tracing::Level::WARN => " WARN".yellow().bold(),
            tracing::Level::INFO => " INFO".cyan().bold(),
            tracing::Level::DEBUG => "DEBUG".white().bold(),
            tracing::Level::TRACE => "TRACE".dim().bold(),
        };

        BUFFER.with_borrow_mut(|buffer| {
            buffer.clear();
            write!(buffer, "{}{}", elapsed.as_millis().dim(), "ms".dim()).ok();
            write!(buffer, " {}: ", prefix).ok();
            event.record(&mut WriteMessage(buffer));
            event.record(&mut WriteFields(buffer));
            writeln!(buffer).ok();
            eprint!("{}", buffer);
        });
    }
}

struct WriteMessage<'a>(&'a mut String);

struct WriteFields<'a>(&'a mut String);

impl<'a> tracing::field::Visit for WriteMessage<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            write!(self.0, "{:?}", value).unwrap();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            write!(self.0, "{}", value).unwrap();
        }
    }
}

impl<'a> tracing::field::Visit for WriteFields<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() != "message" {
            write!(self.0, " {:?}", value.italic().dim()).unwrap();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() != "message" {
            write!(self.0, " {}", value.italic().dim()).unwrap();
        }
    }
}
