use std::fmt::Write;
use yansi::Paint;

#[derive(Debug, Default)]
pub struct Report {
    pub header: String,
    pub items: Vec<ReportItem>,
    pub footer: Vec<String>,
}

#[derive(Debug)]
pub enum ReportItem {
    Text(String),
    Child(Report),
}

impl Report {
    pub fn print(&self) -> String {
        self.print_report(
            &Charset::UNICODE,
            match textwrap::termwidth() {
                w if w > 40 => w - 10,
                w => w,
            },
        )
    }

    fn print_report(&self, charset: &Charset, width: usize) -> String {
        let mut result = String::new();

        let pipe = charset.pipe.dim();
        let bar = charset.bar.dim();
        let rtl = charset.rtl.dim();
        let rbl = charset.rbl.dim();

        // Print the header text
        writeln!(result, "{}{} {}", rtl, bar, self.header.bold()).ok();

        // Print the body
        for item in &self.items {
            match item {
                ReportItem::Text(text) => {
                    for line in textwrap::wrap(text, width.saturating_sub(2)) {
                        writeln!(result, "{} {}", pipe, line).ok();
                    }
                }

                ReportItem::Child(child) => {
                    writeln!(result, "{} ", pipe).ok();

                    let child = child.print_report(charset, width.saturating_sub(2));
                    for line in child.lines() {
                        writeln!(result, "{} {}", pipe, line).ok();
                    }
                }
            }
        }

        // Print the footer line
        write!(result, "{}{}{} ", rbl, bar, bar).ok();

        // Print footer text
        for (i, footer) in self.footer.iter().enumerate() {
            if i > 0 {
                write!(result, " {} ", bar).ok();
            }

            write!(result, "{}", footer).ok();
        }

        result
    }
}

struct Charset {
    pipe: &'static str,
    bar: &'static str,
    rtl: &'static str,
    rbl: &'static str,
}

impl Charset {
    pub const UNICODE: Self = Self {
        pipe: "│",
        bar: "─",
        rtl: "╭",
        rbl: "╰",
    };

    pub const ASCII: Self = Self {
        pipe: "|",
        bar: "",
        rtl: "|",
        rbl: "|",
    };
}
