mod config;
mod report;

pub use config::*;
pub use report::*;

pub fn pluralize(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {}s", count, singular)
    }
}

pub fn pretty_wrap(text: &str, width: usize) -> Vec<std::borrow::Cow<'_, str>> {
    textwrap::wrap(
        text,
        textwrap::Options::new(width)
            .break_words(true)
            .wrap_algorithm(textwrap::WrapAlgorithm::OptimalFit(Default::default())),
    )
}
