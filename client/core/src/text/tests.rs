use super::*;
use std::path::{Path, PathBuf};

#[test]
fn english_fills_arguments_nested_prose_and_plural_counts() {
    let nested = Text::with("Delete “%1”?", vec![Arg::from("alpha")]);
    let outer = Text::with("%1 (%2)", vec![Arg::from(nested), Arg::from(3usize)]);
    assert_eq!(outer.english(), "Delete “alpha”? (3)");
    let plural = Text::with("%n session(s)", vec![Arg::from(2i64)]);
    assert_eq!(plural.english(), "2 session(s)");
    // A literal percent, a missing argument and a lone % stay as written.
    assert_eq!(Text::new("100% done %2").english(), "100% done %2");
}

#[test]
fn the_wire_shape_is_source_plus_tagged_arguments() {
    let text = Text::with(
        "New branch: %1\nStart from: HEAD",
        vec![Arg::from("work-1")],
    );
    assert_eq!(
        serde_json::to_value(&text).unwrap(),
        serde_json::json!({ "source": "New branch: %1\nStart from: HEAD", "args": [{ "kind": "verbatim", "value": "work-1" }] })
    );
    assert_eq!(
        serde_json::to_value(Text::new("Start session")).unwrap(),
        serde_json::json!({ "source": "Start session" })
    );
    let back: Text = serde_json::from_value(serde_json::to_value(&text).unwrap()).unwrap();
    assert_eq!(back, text);
}

/// Every `text!`/`source!` literal in the core is a catalog source, so the
/// webview can translate it. Plural sources are present as their three forms.
#[test]
fn every_marked_core_source_is_in_the_generated_catalog() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let catalog: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(root.join("../src/i18n.ru.json")).unwrap())
            .unwrap();
    let mut files = Vec::new();
    collect(&root.join("src"), &mut files);
    let mut sources = Vec::new();
    for file in files {
        // This module's own examples are not interface text.
        if file.ends_with("src/text/tests.rs") || file.ends_with("src/text.rs") {
            continue;
        }
        sources.extend(marked_sources(&std::fs::read_to_string(&file).unwrap()));
    }
    assert!(!sources.is_empty());
    let missing: Vec<_> = sources
        .iter()
        .filter(|s| !catalog.contains_key(s.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "core sources missing from client/src/i18n.ru.json: {missing:#?}"
    );
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
