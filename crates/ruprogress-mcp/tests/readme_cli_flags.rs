//! Pins the CLI flag tables in both READMEs to the binary's own `--help`.
//! A flag added to `Cli` and documented nowhere — or a flag removed from
//! `Cli` and left in a table — fails here rather than shipping.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// `clap` synthesises this one; no table documents it.
const NOT_DOCUMENTED: &[&str] = &["--help"];

/// Every `--flag` occurring in `text`, ignoring anything after the first
/// `<PLACEHOLDER>` on a line so a default like `[default: stdio]` can never
/// contribute one.
fn long_flags(text: &str) -> BTreeSet<String> {
    let mut flags = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find("--") {
        rest = &rest[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_lowercase() && c != '-')
            .unwrap_or(rest.len());
        let (flag, tail) = rest.split_at(end);
        // Skip a bare `--` and markdown's `—`-ish rules; a real flag has a
        // letter after the dashes.
        if flag.len() > 2 {
            flags.insert(flag.to_string());
        }
        rest = tail;
    }
    for skip in NOT_DOCUMENTED {
        flags.remove(*skip);
    }
    flags
}

/// The flags the binary actually accepts, read from `--help` so the test
/// never re-states `Cli` (which lives in `main.rs` and is unreachable from
/// an integration test).
fn flags_from_help() -> BTreeSet<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_ruprogress-mcp"))
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(out.status.success(), "--help exited non-zero");
    let help = String::from_utf8(out.stdout).expect("--help is utf-8");
    // Only the option list: a flag named in a description is documentation,
    // not a flag the parser accepts.
    let options = help.split_once("Options:").expect("--help has options").1;
    options
        .lines()
        .filter_map(|line| line.split_whitespace().find(|w| w.starts_with("--")))
        .filter_map(|w| long_flags(w).into_iter().next())
        .collect()
}

/// The flags listed in a markdown table's first column.
fn flags_from_table(markdown: &str) -> BTreeSet<String> {
    markdown
        .lines()
        .filter(|line| line.starts_with("| `--"))
        .filter_map(|line| line.split('|').nth(1))
        .flat_map(long_flags)
        .collect()
}

#[test]
fn readmes_document_every_cli_flag() {
    let expected = flags_from_help();
    assert!(
        expected.len() >= 2,
        "parsed too few flags from --help ({expected:?}) — the parser is broken, not the docs"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for readme in ["README.md", "crates/ruprogress-mcp/README.md"] {
        let markdown = std::fs::read_to_string(root.join(readme))
            .unwrap_or_else(|e| panic!("read {readme}: {e}"));
        assert_eq!(
            flags_from_table(&markdown),
            expected,
            "{readme}'s CLI table does not match `ruprogress-mcp --help`"
        );
    }
}
