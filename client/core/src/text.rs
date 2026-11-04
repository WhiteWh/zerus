//! Interface prose that the frontend translates. The core holds no language:
//! it returns the English source and typed arguments, and the webview renders
//! them with `tr()` from the one generated catalog. Logic and tests compare
//! these values or their [`Text::english`] form, never a translation.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// A catalog source with its `%1`, `%2`… (or plural `%n`) arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Text {
    pub source: Cow<'static, str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<Arg>,
}

/// One argument: nested prose is translated too; everything else is shown verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Arg {
    /// Prose embedded in prose.
    Text(Text),
    /// Identities, paths, branch names, agent output: never translated.
    Verbatim(String),
    /// A count; as the first argument it selects a `%n` plural form.
    Number(i64),
}

impl Default for Text {
    /// An empty placeholder before the first real state; never a catalog source.
    fn default() -> Self {
        Self::new("")
    }
}

impl Text {
    pub const fn new(source: &'static str) -> Self {
        Self {
            source: Cow::Borrowed(source),
            args: Vec::new(),
        }
    }
    pub fn with(source: &'static str, args: Vec<Arg>) -> Self {
        Self {
            source: Cow::Borrowed(source),
            args,
        }
    }
    /// Text that is shown as it is: agent output, an identity, a stored or
    /// remote message. The webview never translates it.
    pub fn verbatim(value: impl Into<String>) -> Self {
        Self::with("%1", vec![Arg::Verbatim(value.into())])
    }
    /// True for the empty default, which views use for "nothing to show".
    pub fn is_empty(&self) -> bool {
        self.source.is_empty() && self.args.is_empty()
    }
    /// The English rendering, for logs, tests and non-webview fallbacks.
    pub fn english(&self) -> String {
        let args: Vec<String> = self.args.iter().map(Arg::english).collect();
        let mut out = String::with_capacity(self.source.len());
        let mut rest = self.source.as_ref();
        while let Some(at) = rest.find('%') {
            out.push_str(&rest[..at]);
            let tail = &rest[at + 1..];
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                let index: usize = tail[..digits].parse().unwrap_or(0);
                match index.checked_sub(1).and_then(|i| args.get(i)) {
                    Some(value) => out.push_str(value),
                    None => out.push_str(&rest[at..at + 1 + digits]),
                }
                rest = &tail[digits..];
            } else if let Some(stripped) = tail.strip_prefix('n') {
                out.push_str(args.first().map(String::as_str).unwrap_or("%n"));
                rest = stripped;
            } else {
                out.push('%');
                rest = tail;
            }
        }
        out.push_str(rest);
        out
    }
}

impl Arg {
    fn english(&self) -> String {
        match self {
            Arg::Text(text) => text.english(),
            Arg::Verbatim(value) => value.clone(),
            Arg::Number(value) => value.to_string(),
        }
    }
}

impl From<Text> for Arg {
    fn from(text: Text) -> Self {
        Arg::Text(text)
    }
}
impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Arg::Verbatim(value.to_owned())
    }
}
impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::Verbatim(value)
    }
}
impl From<i64> for Arg {
    fn from(value: i64) -> Self {
        Arg::Number(value)
    }
}
impl From<usize> for Arg {
    fn from(value: usize) -> Self {
        Arg::Number(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

/// Marks a catalog source. `text!("…")` is a [`Text`]; `text!("… %1", arg)`
/// adds arguments. The coverage test reads these literals.
#[macro_export]
macro_rules! text {
    ($source:literal) => {
        $crate::text::Text::new($source)
    };
    ($source:literal, $($arg:expr),+ $(,)?) => {
        $crate::text::Text::with($source, vec![$($crate::text::Arg::from($arg)),+])
    };
}

/// Marks a catalog source kept as `&'static str` (constants and closed tables).
#[macro_export]
macro_rules! source {
    ($source:literal) => {
        $source
    };
}

/// The literals after `text!(` and `source!(` in Rust source, with Rust's
/// simple escapes: the catalog coverage tests of the core and the app read them.
#[doc(hidden)]
pub fn marked_sources(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["text!(", "source!("] {
        let mut rest = code;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let trimmed = rest.trim_start();
            let Some(body) = trimmed.strip_prefix('"') else {
                continue;
            };
            let mut value = String::new();
            let mut chars = body.chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next() {
                        Some('n') => value.push('\n'),
                        Some('t') => value.push('\t'),
                        Some(other) => value.push(other),
                        None => break,
                    },
                    other => value.push(other),
                }
            }
            out.push(value);
        }
    }
    out
}

#[cfg(test)]
mod tests;
