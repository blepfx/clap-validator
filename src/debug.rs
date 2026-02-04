mod log;
mod panic;
mod trace;

pub use log::*;
pub use panic::*;
pub use trace::*;

/// Records a value in the current tracing span and returns it.
pub fn record<T: std::fmt::Debug>(name: &'static str, value: T) -> T {
    tracing::Span::current().record(name, tracing::field::debug(&value));
    value
}
