//! Conformance with Apple's Pkl: each `tests/conformance/<case>.pkl` is
//! evaluated and compared with `<case>.json`, the output of the reference
//! implementation (`pkl eval -f json`), or with `<case>.error`, the message of
//! the error it reports, which pklr's error must contain. Modules the cases
//! import live in `tests/conformance/lib`. Regenerate the expected files with
//! `scripts/update-conformance.sh`.

use std::path::{Path, PathBuf};

/// Cases where pklr does not yet match Pkl. A listed case that starts
/// matching fails the test, so the entry is removed when the fix lands.
const KNOWN_DIVERGENT: &[&str] = &[
    "class_chain_three",
    "class_default_calls_method",
    "class_inherited_only",
    "class_lexical_beats_inherited",
    "class_method_const_vs_inherited",
    "class_methods_not_rendered",
    "class_outer_in_subclass",
    "object_forward_member",
    "unused_failing_property_in_object",
];

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pkl"))
        .collect();
    cases.sort();
    cases
}

#[tokio::test]
async fn matches_reference_implementation() {
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    for case in cases() {
        let name = case.file_stem().unwrap().to_string_lossy().into_owned();
        let actual = pklr::eval_to_json_async(&case).await;
        let (matches, expected) = match std::fs::read_to_string(case.with_extension("error")) {
            Ok(message) => {
                let message = message.trim();
                (
                    actual
                        .as_ref()
                        .is_err_and(|error| error.to_string().contains(message)),
                    format!("error containing {message:?}"),
                )
            }
            Err(_) => {
                let expected: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(case.with_extension("json"))
                        .unwrap_or_else(|_| panic!("{name}: missing expected output")),
                )
                .unwrap();
                (
                    actual.as_ref().is_ok_and(|actual| *actual == expected),
                    expected.to_string(),
                )
            }
        };
        match (matches, KNOWN_DIVERGENT.contains(&name.as_str())) {
            (true, true) => fixed.push(name),
            (false, false) => failures.push(format!(
                "{name}:\n  expected: {expected}\n  actual:   {}",
                match &actual {
                    Ok(value) => value.to_string(),
                    Err(error) => format!("error: {error}"),
                }
            )),
            _ => {}
        }
    }
    assert!(
        fixed.is_empty(),
        "now matching Pkl, remove from KNOWN_DIVERGENT: {fixed:?}"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
