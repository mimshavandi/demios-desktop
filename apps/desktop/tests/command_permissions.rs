//! Guard: every IPC command the app registers (`generate_handler!` in `src/lib.rs`) is
//! listed in `build.rs` (so Tauri generates its `allow-<command>` permission) and granted
//! to the main webview in `capabilities/default.json`, unless it is listed below as
//! deliberately not webview-callable. A missed command builds and passes the UI tests (they
//! use the IPC mock), but in the real app every call fails with "<command> not allowed".
//! The skills commands (#132, #137, #146) shipped that way.
//!
//! It also checks the reverse: no `allow-` grant or `build.rs` entry for a command that
//! isn't registered.

use std::collections::BTreeSet;

/// Commands registered in `src/lib.rs` that the webview must not call, each with its
/// reason. None today. Adding one here is a decision: say why.
const NOT_WEBVIEW_CALLABLE: &[(&str, &str)] = &[];

const LIB_RS: &str = include_str!("../src/lib.rs");
const BUILD_RS: &str = include_str!("../build.rs");
const DEFAULT_CAPABILITY: &str = include_str!("../capabilities/default.json");

/// Lines without `//` comments.
fn code(src: &str) -> String {
    src.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The command names inside `generate_handler![ … ]`: `module::name` → `name`.
fn registered() -> BTreeSet<String> {
    let src = code(LIB_RS);
    let mut found = src.match_indices("generate_handler![");
    let (start, m) = found.next().expect("lib.rs registers its commands");
    assert!(
        found.next().is_none(),
        "lib.rs has more than one generate_handler!; teach this test about it"
    );
    let body = &src[start + m.len()..];
    let body = &body[..body.find(']').expect("generate_handler! is closed")];
    let names: Vec<String> = body
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.rsplit("::").next().unwrap_or(s).to_string())
        .collect();
    let set: BTreeSet<String> = names.iter().cloned().collect();
    assert_eq!(set.len(), names.len(), "a command is registered twice");
    assert!(!set.is_empty());
    set
}

/// The quoted names in `build.rs`'s `COMMANDS` list.
fn in_build_rs() -> BTreeSet<String> {
    let src = code(BUILD_RS);
    let start = src
        .find("const COMMANDS: &[&str] = &[")
        .expect("build.rs lists COMMANDS");
    let body = &src[start..];
    let body = &body[body.find("= &[").unwrap() + 4..];
    let body = &body[..body.find("];").expect("COMMANDS is closed")];
    body.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or_else(|| panic!("not a quoted command name in COMMANDS: {s}"))
                .to_string()
        })
        .collect()
}

/// The app's own commands granted in the default capability (`allow-a-b` → `a_b`).
/// Plugin and core permissions (`core:…`, `x:…`) are not app commands.
fn granted() -> BTreeSet<String> {
    let cap: serde_json::Value =
        serde_json::from_str(DEFAULT_CAPABILITY).expect("default.json is JSON");
    assert_eq!(
        cap["webviews"],
        serde_json::json!(["main"]),
        "the default capability is for the main webview only"
    );
    cap["permissions"]
        .as_array()
        .expect("a permissions list")
        .iter()
        .map(|p| p.as_str().expect("permissions are plain identifiers"))
        .filter(|p| !p.contains(':'))
        .map(|p| {
            p.strip_prefix("allow-")
                .unwrap_or_else(|| panic!("an app permission that isn't allow-<command>: {p}"))
                .replace('-', "_")
        })
        .collect()
}

#[test]
fn every_registered_command_is_listed_and_granted_or_excluded_on_purpose() {
    let registered = registered();
    let listed = in_build_rs();
    let granted = granted();
    let excluded: BTreeSet<String> = NOT_WEBVIEW_CALLABLE
        .iter()
        .map(|(name, why)| {
            assert!(!why.trim().is_empty(), "{name}: say why it is excluded");
            name.to_string()
        })
        .collect();

    let not_listed: Vec<_> = registered.difference(&listed).collect();
    assert!(
        not_listed.is_empty(),
        "registered in lib.rs but missing from build.rs COMMANDS: {not_listed:?}"
    );
    let not_granted: Vec<_> = registered
        .iter()
        .filter(|c| !granted.contains(*c) && !excluded.contains(*c))
        .collect();
    assert!(
        not_granted.is_empty(),
        "registered but neither granted in capabilities/default.json nor in \
         NOT_WEBVIEW_CALLABLE: {not_granted:?}"
    );
}

#[test]
fn nothing_is_listed_or_granted_that_is_not_registered() {
    let registered = registered();
    let stale_listed: Vec<_> = in_build_rs().difference(&registered).cloned().collect();
    assert!(
        stale_listed.is_empty(),
        "in build.rs COMMANDS but not registered: {stale_listed:?}"
    );
    let stale_granted: Vec<_> = granted().difference(&registered).cloned().collect();
    assert!(
        stale_granted.is_empty(),
        "granted but not registered: {stale_granted:?}"
    );
    for (name, _) in NOT_WEBVIEW_CALLABLE {
        assert!(
            registered.contains(*name),
            "{name} is excluded but not registered"
        );
        assert!(
            !granted().contains(*name),
            "{name} is excluded and granted at once"
        );
    }
}
