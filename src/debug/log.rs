//! A tracing layer that logs events to standard output in a compact human readable format.

use std::cell::RefCell;
use std::fmt::{Debug, Write};
use std::time::Instant;
use tracing::field::Field;
use tracing::level_filters::LevelFilter;
use tracing::{Level, Subscriber, span};
use yansi::Paint;

pub struct LogStderrSubscriber {
    level: LevelFilter,
    start: Instant,
}

impl LogStderrSubscriber {
    pub fn new(level: LevelFilter) -> Self {
        Self {
            level,
            start: Instant::now(),
        }
    }

    fn write(&self, level: Level, content: impl FnOnce(&mut String)) {
        thread_local! {
            static BUFFER: RefCell<String> = const { RefCell::new(String::new()) }
        }

        let elapsed = self.start.elapsed();
        let prefix = match level {
            Level::ERROR => "ERROR".red().bold(),
            Level::WARN => " WARN".yellow(),
            Level::INFO => " INFO".green(),
            Level::DEBUG => "DEBUG".blue(),
            Level::TRACE => "TRACE".white(),
        };

        BUFFER.with_borrow_mut(|buffer| {
            buffer.clear();
            write!(buffer, "{:>5}{}", elapsed.as_millis().dim(), "ms".dim()).ok();
            write!(buffer, " {}: ", prefix).ok();
            content(buffer);
            writeln!(buffer).ok();
            eprint!("{}", buffer);
        });
    }
}

// why subscriber directly and not a layer?
// the initial layer implementation was taking 25% of total runtime doing practically nothing
impl Subscriber for LogStderrSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.level() <= &self.level && metadata.is_event()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.level)
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn current_span(&self) -> tracing_core::span::Current {
        tracing_core::span::Current::none()
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        self.write(*event.metadata().level(), |buffer| {
            event.record(&mut WriteMessage(buffer));
            event.record(&mut WriteFields(buffer));
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
