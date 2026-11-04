//! Interface text the app draws natively (tray and menu-bar items, their
//! tooltip): the webview's generated catalog, read once. The language is the
//! saved Settings > Language at startup; a change applies after a restart, as
//! in the webview. Verbatim arguments (names, versions) are never translated.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use zerus_client_core::text::{Arg, Text};

static RUSSIAN: AtomicBool = AtomicBool::new(false);
static CATALOG: LazyLock<serde_json::Map<String, serde_json::Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../src/i18n.ru.json")).unwrap_or_default()
});

/// Settings' `workspace/language` at startup: "ru" selects Russian.
pub fn configure(language: &str) {
    RUSSIAN.store(language == "ru", Ordering::Relaxed);
}

/// A core Text in the interface language, nested prose included.
pub fn render(text: &Text) -> String {
    render_in(text, RUSSIAN.load(Ordering::Relaxed))
}

/// Lines of a tooltip, each rendered, joined by line breaks.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub fn lines(texts: &[Text]) -> String {
    texts.iter().map(render).collect::<Vec<_>>().join("\n")
}

fn render_in(text: &Text, russian: bool) -> String {
    let args: Vec<String> = text
        .args
        .iter()
        .map(|arg| match arg {
            Arg::Text(nested) => render_in(nested, russian),
            Arg::Verbatim(value) => value.clone(),
            Arg::Number(value) => value.to_string(),
        })
        .collect();
    let count = text.args.first().and_then(|arg| match arg {
        Arg::Number(value) => Some(*value),
        _ => None,
    });
    let template = if russian {
        translation(&text.source, count)
    } else {
        None
    };
    fill(template.as_deref().unwrap_or(&text.source), &args)
}

/// The Russian template, choosing a plural form by the first numeric argument.
fn translation(source: &str, count: Option<i64>) -> Option<String> {
    match CATALOG.get(source)? {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Array(forms) => {
            let form = russian_form(count.unwrap_or(0));
            forms.get(form)?.as_str().map(str::to_owned)
        }
        _ => None,
    }
}

fn russian_form(count: i64) -> usize {
    let n = count.unsigned_abs();
    if n % 10 == 1 && n % 100 != 11 {
        0
    } else if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) {
        1
    } else {
        2
    }
}

/// One pass over the template, as the webview's `translateWithCatalog`
/// (`%L?<digits>` or `%n`): an argument containing `%1` is never expanded
/// again, and a missing argument leaves its token as written.
fn fill(template: &str, args: &[String]) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let (mut at, mut copied) = (0, 0);
    while at < bytes.len() {
        if bytes[at] != b'%' {
            at += 1;
            continue;
        }
        let mut end = at + 1;
        let value = if bytes.get(end) == Some(&b'n') {
            end += 1;
            args.first()
        } else {
            if bytes.get(end) == Some(&b'L') {
                end += 1;
            }
            let digits = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            template[digits..end]
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|index| args.get(index))
        };
        match value {
            Some(value) => {
                out.push_str(&template[copied..at]);
                out.push_str(value);
                copied = end;
                at = end;
            }
            None => at += 1,
        }
    }
    out.push_str(&template[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests pass the language explicitly: the process-wide choice is shared
    // with every other test of the app.
    #[test]
    fn english_fills_arguments_and_russian_uses_the_catalog() {
        let open = zerus_client_core::text!("Update available: Zerus %1", "0.48.0");
        assert_eq!(render_in(&open, false), "Update available: Zerus 0.48.0");
        assert_eq!(
            render_in(&Text::verbatim("100% done %1"), false),
            "100% done %1"
        );
        assert_eq!(render_in(&open, true), "Доступно обновление: Zerus 0.48.0");
        // Verbatim values stay as they are, and are never expanded again.
        assert_eq!(render_in(&Text::verbatim("a %1 b"), true), "a %1 b");
        assert_eq!(
            render_in(
                &zerus_client_core::text!("%1%2", "studio", zerus_client_core::text!("  (paused)")),
                true
            ),
            "studio  (пауза)"
        );
        assert_eq!(
            render_in(&zerus_client_core::text!("%1 need attention", 3_i64), true),
            "Требуют внимания: 3"
        );
    }

    #[test]
    fn filling_matches_the_webview_tokens() {
        let args = ["a".to_owned(), "b".to_owned()];
        assert_eq!(fill("%2-%1 %L1 %n %3 50%", &args), "b-a a a %3 50%");
        assert_eq!(fill("%1", &["%2".to_owned(), "x".to_owned()]), "%2");
    }

    #[test]
    fn russian_plural_forms_follow_the_webview_rule() {
        assert_eq!(
            [1, 2, 5, 11, 21, 22, 25, 111, 112].map(russian_form),
            [0, 1, 2, 2, 0, 1, 2, 2, 2]
        );
    }
}
