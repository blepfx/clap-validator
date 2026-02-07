//! Miscellaneous functions for data conversions.

use rayon::iter::{ParallelBridge, ParallelIterator};
use std::path::PathBuf;

/// A temporary directory used by the validator. This is cleared when launching the validator.
pub fn validator_temp_dir() -> PathBuf {
    /// [`std::env::temp_dir`], but taking `XDG_RUNTIME_DIR` on Linux into account.
    fn temp_dir() -> PathBuf {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from)
            && dir.is_dir()
        {
            return dir;
        }

        std::env::temp_dir()
    }

    temp_dir().join("clap-validator")
}

impl<T: ?Sized> IteratorExt for T where T: Iterator {}
pub trait IteratorExt: Iterator {
    /// Map the iterator in parallel if `parallel` is `true`, or sequentially if it is `false`.
    /// Returns an iterator over the mapped values, in arbitrary order.
    fn map_parallel<R: Send>(self, parallel: bool, f: impl Fn(Self::Item) -> R + Send + Sync) -> impl Iterator<Item = R>
    where
        Self: Sized + Send,
        Self::Item: Send,
    {
        if parallel {
            self.par_bridge().map(f).collect::<Vec<_>>()
        } else {
            self.map(f).collect::<Vec<_>>()
        }
        .into_iter()
    }
}

macro_rules! spanned {
    ($name:literal, $($key:ident: $value:expr,)* $body:block) => {{
        let span = tracing::trace_span!($name, $($key = ?$value,)* result = tracing::field::Empty).entered();
        let result = $body;
        span.record("result", &tracing::field::debug(&result));
        result
    }};
}

pub(crate) use spanned;
