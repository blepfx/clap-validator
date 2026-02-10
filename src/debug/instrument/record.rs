use super::WRITER;
use std::ffi::CStr;
use std::fmt::Display;

pub fn event(message: impl Display, context: impl Recordable) {
    if let Some(writer) = WRITER.get() {
        writer.lock().unwrap().write(
            &message.to_string(),
            std::thread::current().name().unwrap_or("?"),
            "n",
            &context,
        );
    }
}

pub struct Span<'a> {
    name: &'a str,
}

impl Drop for Span<'_> {
    fn drop(&mut self) {
        Span { name: self.name }.finish(())
    }
}

impl<'a> Span<'a> {
    pub fn begin(name: &'a str, context: impl Recordable) -> Self {
        if let Some(writer) = WRITER.get() {
            writer
                .lock()
                .unwrap()
                .write(name, std::thread::current().name().unwrap_or("?"), "b", &context);
        }

        Self { name }
    }

    pub fn finish<T: Recordable>(self, context: T) {
        if let Some(writer) = WRITER.get() {
            writer
                .lock()
                .unwrap()
                .write(self.name, std::thread::current().name().unwrap_or("?"), "e", &context);
        }

        std::mem::forget(self);
    }

    pub fn name(&self) -> &'a str {
        self.name
    }
}

pub trait Recorder {
    fn value(&mut self, name: &str, value: &dyn Display);
    fn group(&mut self, name: &str, record: &dyn Recordable);
}

impl dyn Recorder + '_ {
    pub fn record<T: Recordable>(&mut self, name: &str, value: T) {
        self.group(name, &value);
    }
}

pub trait Recordable {
    fn record(&self, record: &mut dyn Recorder);
}

impl Recordable for () {
    fn record(&self, _: &mut dyn Recorder) {}
}

impl Recordable for CStr {
    fn record(&self, record: &mut dyn Recorder) {
        match self.to_str() {
            Ok(s) => record.value("", &s),
            Err(_) => record.value("", &"<invalid utf-8>"),
        }
    }
}

impl<T: Recordable + ?Sized> Recordable for &T {
    fn record(&self, record: &mut dyn Recorder) {
        (*self).record(record);
    }
}

macro_rules! impl_display {
    ($($ty:ty),*) => {
        $(impl Recordable for $ty {
            fn record(&self, record: &mut dyn Recorder) {
                record.value("", &self);
            }
        })*
    };
}

impl_display!(
    bool,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    f32,
    f64,
    str,
    String,
    std::borrow::Cow<'_, str>,
    std::fmt::Arguments<'_>
);

pub fn from_fn(f: impl Fn(&mut dyn Recorder)) -> impl Recordable {
    struct FnRecord<F: Fn(&mut dyn Recorder)>(F);
    impl<F: Fn(&mut dyn Recorder)> Recordable for FnRecord<F> {
        fn record(&self, record: &mut dyn Recorder) {
            (self.0)(record);
        }
    }
    FnRecord(f)
}

macro_rules! record {
    ($($name:ident: $value:expr),*) => {
        $crate::debug::from_fn(|record| {
            $(record.group(stringify!($name), &$value);)*
        })
    };
}

pub(crate) use record;
