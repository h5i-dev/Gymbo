//! `jvx` against a real tool, from a cold cache.
//!
//! This is `ROADMAP.md`'s M7 gate written down: `jvx
//! com.google.googlejavaformat:google-java-format -- --version` from nothing.
//! Every part of the command has to work at once — endpoint parsing with no
//! version, release selection out of `maven-metadata.xml`, a transitive resolve,
//! downloading each jar, reading `Main-Class` out of a manifest that folds it
//! across two lines, and handing the process to a JVM — so a failure here is
//! worth more than any of the unit tests that stand in for it.
//!
//! google-java-format specifically because it is small, its `--version` touches
//! no files, and it does not run without its dependencies: launching it with an
//! incomplete classpath fails with `NoClassDefFoundError` rather than passing.
//!
//! Skipped unless a JVM is available, since without one there is nothing to
//! exec. Set `JV_REQUIRE_ORACLE=1` to make the absence a failure, which is what
//! CI does so that a missing JDK cannot quietly turn this into a no-op.

use std::path::PathBuf;
use std::process::Command;

/// The tool under test, with no version: choosing one is part of what is tested.
const TOOL: &str = "com.google.googlejavaformat:google-java-format";

/// A JVM to launch, or the reason there is none.
fn java() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let path = PathBuf::from(home).join("bin").join("java");
        if path.is_file() {
            return Ok(path);
        }
    }
    match Command::new("java").arg("-version").output() {
        Ok(output) if output.status.success() => Ok(PathBuf::from("java")),
        _ => Err("no java on PATH and JAVA_HOME names none".to_owned()),
    }
}

/// The `jvx` binary this test run built.
///
/// Cargo puts integration-test binaries beside the ones they test, so the
/// executable is two directories up from the test's own path.
fn jvx_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    path.join(if cfg!(windows) { "jvx.exe" } else { "jvx" })
}

/// Whether output describes a repository that would not serve jv, rather than a
/// tool that does not work.
///
/// This is the only kind of test here that needs the network, and Maven Central
/// rate-limits: a run that trips its limit reports `the repository answered
/// 429`. Calling that a jvx failure is worse than useless — it teaches whoever
/// sees it to ignore a red result from the one test that would catch a real jvx
/// bug. So a refusal by the repository is a skip, and `JV_REQUIRE_ORACLE` turns
/// it back into a failure for CI, which is expected to have a network.
fn repository_refused(said: &str) -> bool {
    said.contains("the repository answered 429")
        || said.contains("the repository answered 50")
        || said.contains("is not cached, and jv is offline")
        || said.contains("could not be reached")
        || said.contains("dns error")
}

/// Skips for a repository that would not answer, or fails if CI required one.
fn refused(what: &str, said: &str) -> bool {
    if !repository_refused(said) {
        return false;
    }
    assert!(
        std::env::var_os("JV_REQUIRE_ORACLE").is_none(),
        "JV_REQUIRE_ORACLE is set but the repository refused {what}:\n{said}"
    );
    eprintln!("skipping {what}: the repository would not serve it\n{said}");
    true
}

#[test]
fn jvx_runs_a_tool_named_only_by_its_coordinates() {
    if let Err(reason) = java() {
        if std::env::var("JV_REQUIRE_ORACLE").is_ok() {
            panic!("JV_REQUIRE_ORACLE is set but {reason}");
        }
        eprintln!("skipping the jvx launch test: {reason}");
        return;
    }

    let cache = tempfile::tempdir().expect("a temp dir");
    let output = Command::new(jvx_binary())
        .arg("--cache-dir")
        .arg(cache.path())
        // A populated `~/.m2` would make this pass without ever testing the
        // download path, which is half of what a cold run is.
        .arg("--no-local-repository")
        .arg(TOOL)
        .arg("--")
        .arg("--version")
        .output()
        .expect("jvx should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The tool prints its version banner to standard error, so both streams are
    // in play; which one is the tool's business, not jvx's.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && refused("the jvx launch test", &format!("{stdout}{stderr}")) {
        return;
    }
    assert!(
        output.status.success(),
        "jvx failed with {:?}\nstdout: {stdout}\nstderr: {stderr}",
        output.status.code()
    );
    assert!(
        stdout.contains("google-java-format") || stderr.contains("google-java-format"),
        "no version banner\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn jvx_forwards_the_tools_exit_code() {
    if let Err(reason) = java() {
        if std::env::var("JV_REQUIRE_ORACLE").is_ok() {
            panic!("JV_REQUIRE_ORACLE is set but {reason}");
        }
        eprintln!("skipping the jvx exit code test: {reason}");
        return;
    }

    let cache = tempfile::tempdir().expect("a temp dir");
    // Shares the cache directory with nothing, but the artifacts are already in
    // jv's default cache after the first test on a developer machine; on a cold
    // CI runner this is the second download of the same tool, which is the price
    // of the two tests being independent.
    let output = Command::new(jvx_binary())
        .arg("--cache-dir")
        .arg(cache.path())
        .arg("--no-local-repository")
        .arg(TOOL)
        .arg("--")
        .arg("--no-such-flag")
        .output()
        .expect("jvx should run");

    assert!(
        !output.status.success(),
        "a tool that rejected its arguments should not report success"
    );
}

#[test]
fn an_unreadable_endpoint_fails_before_anything_is_resolved() {
    // No JDK guard and no network: the point is that the grammar rejects this
    // without a session, which is also what makes a typo cheap to discover.
    let output = Command::new(jvx_binary())
        .arg("--offline")
        .arg("google-java-format")
        .output()
        .expect("jvx should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("group:artifact"),
        "the error should show the grammar, got: {stderr}"
    );
}
