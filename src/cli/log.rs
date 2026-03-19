//! A tracing layer that logs events to standard output in a compact human readable format.

use crate::cli::tracing::{event, record};
use std::cell::RefCell;
use std::fmt::Write;
use std::time::SystemTime;
use yansi::Paint;

pub struct CustomLogger;

impl log::Log for CustomLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, log: &log::Record) {
        thread_local! {
            static BUFFER: RefCell<String> = const { RefCell::new(String::new()) }
        }

        let elapsed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            .rem_euclid(1.0);

        let prefix = match log.level() {
            log::Level::Error => "ERROR".red().bold(),
            log::Level::Warn => " WARN".yellow(),
            log::Level::Info => " INFO".green(),
            log::Level::Debug => "DEBUG".blue(),
            log::Level::Trace => "TRACE".white(),
        };

        event(
            log.args(),
            record! {
                level: log.level().to_string(),
                target: log.target()
            },
        );

        BUFFER.with_borrow_mut(|buffer| {
            buffer.clear();
            write!(buffer, "{:>5.3}", elapsed.dim()).ok();
            write!(buffer, " {}: ", prefix).ok();
            write!(buffer, "{}", log.args()).ok();
            write!(buffer, " {}", log.target().dim().italic()).ok();
            writeln!(buffer).ok();
            eprint!("{}", buffer);
        });
    }

    fn flush(&self) {}
}
