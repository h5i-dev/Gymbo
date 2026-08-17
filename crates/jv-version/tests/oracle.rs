//! Differential test against Maven Resolver's real `GenericVersion`.
//!
//! The corpus tests check jv against a transcription of upstream's assertions.
//! This test checks jv against upstream's *implementation*: it compiles
//! `GenericVersion` and `GenericQualifiers` straight out of a maven-resolver
//! checkout, drives both sides over tens of thousands of generated inputs, and
//! compares tokenization, comparison sign, and qualifier detection.
//!
//! Transcribed assertions can only cover cases someone thought to write down;
//! this covers the shape of the input space, which is where a port of a fiddly
//! state machine actually goes wrong.
//!
//! The test skips itself when a JDK or the maven-resolver sources are missing,
//! so a fresh clone still runs green. Set `JV_REQUIRE_ORACLE=1` (CI does) to
//! turn an unavailable oracle into a failure. Point `JV_MAVEN_RESOLVER_SRC` at
//! a checkout to override discovery.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use jv_version::{Version, qualifier};

/// Upper bound on generated comparison pairs. Large enough to exercise the
/// padding and kind-mismatch paths densely, small enough to stay a fast test.
const MAX_PAIRS: usize = 40_000;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root")
}

/// Locates a maven-resolver checkout, or explains why it could not.
fn resolver_src() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(from_env) = std::env::var("JV_MAVEN_RESOLVER_SRC") {
        candidates.push(PathBuf::from(from_env));
    }
    candidates.push(workspace_root().join("_reference").join("maven-resolver"));

    for candidate in &candidates {
        if candidate
            .join("maven-resolver-util/src/main/java/org/eclipse/aether/util/version/GenericVersion.java")
            .is_file()
        {
            return Ok(candidate.clone());
        }
    }
    Err(format!(
        "no maven-resolver checkout found (looked in {}); \
         clone it or set JV_MAVEN_RESOLVER_SRC",
        candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Compiles the oracle, reusing the previous build when nothing changed.
fn compile_oracle(resolver: &Path) -> Result<PathBuf, String> {
    let oracle_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("oracle")
        .join("java");
    let oracle_source = oracle_root.join("org/eclipse/aether/util/version/Oracle.java");
    let classes = Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle-classes");
    let compiled = classes.join("org/eclipse/aether/util/version/Oracle.class");

    let up_to_date = match (compiled.metadata(), oracle_source.metadata()) {
        (Ok(out), Ok(src)) => match (out.modified(), src.modified()) {
            (Ok(out_time), Ok(src_time)) => out_time >= src_time,
            _ => false,
        },
        _ => false,
    };
    if up_to_date {
        return Ok(classes);
    }

    std::fs::create_dir_all(&classes).map_err(|e| e.to_string())?;
    let sourcepath = [
        resolver.join("maven-resolver-api/src/main/java"),
        resolver.join("maven-resolver-util/src/main/java"),
        oracle_root.clone(),
    ]
    .iter()
    .map(|p| p.display().to_string())
    .collect::<Vec<_>>()
    .join(":");

    let output = Command::new("javac")
        .arg("-nowarn")
        .arg("-d")
        .arg(&classes)
        .arg("-sourcepath")
        .arg(&sourcepath)
        .arg(&oracle_source)
        .output()
        .map_err(|e| format!("cannot run javac: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "javac failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(classes)
}

/// Builds the set of version strings to test.
///
/// Three sources, each covering what the others miss: every string the corpus
/// mentions (real-world shapes upstream cared about), a systematic cross-product
/// of segment kinds and delimiters (the tokenizer's state transitions), and
/// deterministic pseudo-random strings (combinations nobody would think of).
fn version_universe() -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let push = |candidate: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
        // The oracle protocol is line- and tab-delimited.
        if candidate.contains(['\t', '\n', '\r']) {
            return;
        }
        if seen.insert(candidate.to_owned()) {
            out.push(candidate.to_owned());
        }
    };

    // 1. Everything the ordering corpus mentions, divergent sections included:
    // any string is a valid input even when its expected ordering differs.
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/ordering.txt");
    if let Ok(text) = std::fs::read_to_string(&corpus) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((keyword, rest)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let rest = rest.trim_start();
            match keyword {
                "lt" | "eqp" => {
                    if let Some((a, b)) = rest.split_once('|') {
                        push(a, &mut out, &mut seen);
                        push(b, &mut out, &mut seen);
                    }
                }
                "order" => {
                    for part in rest.split('<') {
                        push(part.trim(), &mut out, &mut seen);
                    }
                }
                "eq" => {
                    for part in rest.split("==") {
                        push(part.trim(), &mut out, &mut seen);
                    }
                }
                _ => {}
            }
        }
    }

    // 2. Systematic cross-product over the tokenizer's decision points.
    const BASES: &[&str] = &["0", "1", "2", "10", "1.0", "1.0.0", "1.2.3", "0.0.1"];
    const SEPARATORS: &[&str] = &[".", "-", "_", ""];
    const SEGMENTS: &[&str] = &[
        "alpha",
        "beta",
        "milestone",
        "rc",
        "cr",
        "snapshot",
        "ga",
        "final",
        "release",
        "sp",
        "a",
        "b",
        "m",
        "min",
        "max",
        "foo",
        "SNAPSHOT",
        "Alpha",
        "",
    ];
    const TAILS: &[&str] = &["", "1", "0", "2", "10", "007"];
    for base in BASES {
        for separator in SEPARATORS {
            for segment in SEGMENTS {
                for tail in TAILS {
                    push(
                        &format!("{base}{separator}{segment}{separator}{tail}"),
                        &mut out,
                        &mut seen,
                    );
                }
            }
        }
    }

    // 3. Hand-picked edge cases: empty and degenerate input, leading zeros, the
    // Int/BigInt boundary at ten digits, and non-ASCII.
    for special in [
        "",
        "0",
        "00",
        "000",
        "0.0",
        "-",
        ".",
        "_",
        "---",
        "1.",
        ".1",
        "1..2",
        "1-",
        "-1",
        "1_2",
        "999999999",
        "1000000000",
        "9999999999",
        "10000000000",
        "1.0000000001",
        "1.00000000000000000001",
        "99999999999999999999999999999999",
        "1.2.min",
        "1.2.max",
        "min",
        "max",
        "MIN",
        "MAX",
        "1.min.2",
        "1.0-日本語",
        "1.0-Ä",
        "1.0.0.0.0.0.0.0.0.0",
    ] {
        push(special, &mut out, &mut seen);
    }

    // 4. Deterministic pseudo-random strings from the alphabet that matters.
    const ALPHABET: &[u8] = b"0123456789abmrcsgpZ.-_";
    let mut state: u64 = 0x5DEE_CE66_D00D_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    for _ in 0..1500 {
        let len = 1 + next() % 14;
        let candidate: String = (0..len)
            .map(|_| ALPHABET[next() % ALPHABET.len()] as char)
            .collect();
        push(&candidate, &mut out, &mut seen);
    }

    out
}

/// Builds comparison pairs: locally dense (all pairs inside small windows, so
/// similar strings are compared against each other) plus a deterministic random
/// sample for long-range coverage.
fn comparison_pairs(universe: &[String]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    const WINDOW: usize = 12;
    for chunk_start in (0..universe.len()).step_by(WINDOW) {
        let chunk_end = (chunk_start + WINDOW).min(universe.len());
        for i in chunk_start..chunk_end {
            for j in i + 1..chunk_end {
                pairs.push((i, j));
            }
        }
    }

    let mut state: u64 = 0xDEAD_BEEF_CAFE_F00D_u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    while pairs.len() < MAX_PAIRS {
        let i = next() % universe.len();
        let j = next() % universe.len();
        if i != j {
            pairs.push((i, j));
        }
    }
    pairs.truncate(MAX_PAIRS);
    pairs
}

/// Formats a tokenized version the way `java.util.List#toString` would, so the
/// two sides can be compared as text.
fn format_items(version: &Version) -> String {
    let rendered: Vec<String> = version.items().iter().map(|i| i.to_string()).collect();
    format!("[{}]", rendered.join(", "))
}

#[test]
fn matches_upstream_generic_version() {
    let required = std::env::var_os("JV_REQUIRE_ORACLE").is_some();

    let resolver = match resolver_src() {
        Ok(path) => path,
        Err(why) => {
            if required {
                panic!("JV_REQUIRE_ORACLE is set but {why}");
            }
            eprintln!("skipping oracle test: {why}");
            return;
        }
    };
    if !tool_available("javac") || !tool_available("java") {
        if required {
            panic!("JV_REQUIRE_ORACLE is set but no JDK (javac/java) is on PATH");
        }
        eprintln!("skipping oracle test: no JDK on PATH");
        return;
    }

    let classes = match compile_oracle(&resolver) {
        Ok(path) => path,
        Err(why) => {
            if required {
                panic!("JV_REQUIRE_ORACLE is set but the oracle failed to build: {why}");
            }
            eprintln!("skipping oracle test: {why}");
            return;
        }
    };

    let universe = version_universe();
    assert!(
        universe.len() > 2000,
        "generated only {} version strings; the generator is broken",
        universe.len()
    );
    let pairs = comparison_pairs(&universe);

    // Write the whole request to a file rather than piping: a pipe would
    // deadlock once Java's output fills the buffer while we are still writing.
    let request_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle-input.txt");
    let mut request = String::new();
    for version in &universe {
        request.push('T');
        request.push_str(version);
        request.push('\n');
    }
    for version in &universe {
        request.push('Q');
        request.push_str(version);
        request.push('\n');
    }
    for (i, j) in &pairs {
        request.push('C');
        request.push_str(&universe[*i]);
        request.push('\t');
        request.push_str(&universe[*j]);
        request.push('\n');
    }
    std::fs::write(&request_path, &request).expect("write oracle input");

    let output = Command::new("java")
        .arg("-cp")
        .arg(&classes)
        .arg("org.eclipse.aether.util.version.Oracle")
        .arg(&request_path)
        .output()
        .expect("run oracle");
    assert!(
        output.status.success(),
        "oracle exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("oracle output is UTF-8");
    let expected: Vec<&str> = stdout.lines().collect();
    let want_lines = universe.len() * 2 + pairs.len();
    assert_eq!(
        expected.len(),
        want_lines,
        "oracle returned {} lines, expected {want_lines}",
        expected.len()
    );

    let mut mismatches: Vec<String> = Vec::new();
    let record = |message: String, mismatches: &mut Vec<String>| {
        if mismatches.len() < 30 {
            mismatches.push(message);
        } else if mismatches.len() == 30 {
            mismatches.push("... (further mismatches suppressed)".to_owned());
        }
    };

    let mut checked = 0usize;
    let mut cursor = 0usize;

    for version in &universe {
        let ours = format_items(&Version::parse(version));
        if ours != expected[cursor] {
            record(
                format!(
                    "tokenize {version:?}: upstream {}, jv {ours}",
                    expected[cursor]
                ),
                &mut mismatches,
            );
        }
        cursor += 1;
        checked += 1;
    }

    for version in &universe {
        let ours = match qualifier(version) {
            Some(shift) => shift.to_string(),
            None => "none".to_owned(),
        };
        if ours != expected[cursor] {
            record(
                format!(
                    "qualifier {version:?}: upstream {}, jv {ours}",
                    expected[cursor]
                ),
                &mut mismatches,
            );
        }
        cursor += 1;
        checked += 1;
    }

    for (i, j) in &pairs {
        let ours = match Version::parse(&universe[*i]).cmp(&Version::parse(&universe[*j])) {
            std::cmp::Ordering::Less => "-1",
            std::cmp::Ordering::Equal => "0",
            std::cmp::Ordering::Greater => "1",
        };
        if ours != expected[cursor] {
            record(
                format!(
                    "compare {:?} vs {:?}: upstream {}, jv {ours}",
                    universe[*i], universe[*j], expected[cursor]
                ),
                &mut mismatches,
            );
        }
        cursor += 1;
        checked += 1;
    }

    assert!(
        mismatches.is_empty(),
        "{} of {checked} oracle checks disagree:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    eprintln!(
        "oracle: {checked} checks agree with maven-resolver \
         ({} versions tokenized, {} qualifier probes, {} comparisons)",
        universe.len(),
        universe.len(),
        pairs.len()
    );
}
