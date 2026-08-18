//! Reads every graph in Maven Resolver's transformer corpus.
//!
//! These 45 files are the exhaustive statement of what conflict resolution has
//! to do, so being able to read all of them is the precondition for checking jv
//! against any of them. This test asserts only that: parsing. Asserting the
//! *results* comes with the resolver itself.
//!
//! Skips itself when `_reference/` is absent; `JV_REQUIRE_ORACLE=1` makes that a
//! failure, as CI does.

use std::path::{Path, PathBuf};

use jv_testkit::graph_dsl;

fn corpus_dir() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../_reference/maven-resolver")
        .join("maven-resolver-util/src/test/resources/transformer");
    path.is_dir().then_some(path)
}

/// Placeholders in this corpus are always scopes, and a parse test only needs
/// them to be *valid* scopes. Which ones each case actually uses is the driving
/// Java test's business, and matters only once results are compared.
const SUBSTITUTIONS: &[&str] = &[
    "compile", "test", "provided", "runtime", "compile", "test", "provided", "runtime", "compile",
    "test", "provided", "runtime", "compile", "test", "provided", "runtime",
];

#[test]
fn every_transformer_graph_parses() {
    let Some(corpus) = corpus_dir() else {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but _reference/maven-resolver is missing");
        }
        eprintln!("skipping: _reference/ not present (see docs/development.md)");
        return;
    };

    let mut files = 0usize;
    let mut graphs = 0usize;
    let mut nodes = 0usize;
    let mut with_cycles = 0usize;
    let mut failures = Vec::new();

    let mut stack = vec![corpus.clone()];
    let mut paths = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "txt") {
                paths.push(path);
            }
        }
    }
    paths.sort();

    for path in &paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            failures.push(format!("{}: unreadable", path.display()));
            continue;
        };
        files += 1;
        match graph_dsl::parse_all(&text, SUBSTITUTIONS) {
            Ok(parsed) => {
                if parsed.is_empty() {
                    failures.push(format!("{}: parsed to no graph at all", path.display()));
                    continue;
                }
                graphs += parsed.len();
                for graph in &parsed {
                    let order = graph.preorder();
                    nodes += order.len();
                    // A node reachable from itself is how the corpus spells a
                    // cycle; the walk has to survive it.
                    if order.len() > graph.len() {
                        with_cycles += 1;
                    }
                }
            }
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }

    assert!(
        files >= 40,
        "found only {files} corpus files under {}",
        corpus.display()
    );
    assert!(
        failures.is_empty(),
        "{} corpus file(s) failed to parse:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // A green run that parsed nothing interesting would prove nothing.
    assert!(nodes > 200, "only {nodes} nodes across the corpus");
    assert!(
        with_cycles > 0,
        "no corpus graph exercised a shared or cyclic node"
    );

    eprintln!(
        "parsed {graphs} graph(s) from {files} corpus file(s): \
         {nodes} nodes, {with_cycles} graph(s) with shared or cyclic nodes"
    );
}
