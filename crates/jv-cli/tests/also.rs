//! `jv sync --also`, the escape hatch for what no POM says.
//!
//! Nothing here reaches a repository: the point is that the coordinates are
//! understood and that a malformed one is refused clearly rather than turned
//! into a request for a path that cannot exist.

use std::path::PathBuf;
use std::process::Command;

fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

/// Runs `jv sync --offline` against an empty cache, which fails — the question
/// is only *how* it fails, which is what `--also` parsing decides.
fn sync_with(also: &[&str]) -> String {
    let workspace = tempfile::tempdir().expect("a temp dir");
    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();
    let pom = workspace.path().join("pom.xml");
    std::fs::write(
        &pom,
        r#"<project>
             <modelVersion>4.0.0</modelVersion>
             <groupId>com.example</groupId>
             <artifactId>demo</artifactId>
             <version>1.0</version>
           </project>"#,
    )
    .unwrap();

    let mut command = Command::new(jv_binary());
    command
        .arg("sync")
        .arg("-f")
        .arg(&pom)
        .arg("-s")
        .arg(&settings)
        .arg("--offline")
        .arg("--cache-dir")
        .arg(workspace.path().join("cache"))
        .arg("--local-repository")
        .arg(workspace.path().join("m2"));
    for coordinates in also {
        command.arg("--also").arg(coordinates);
    }
    let output = command.output().expect("jv should run");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn coordinates_are_understood_and_reached_for() {
    // Offline with an empty cache it cannot be fetched, and saying so proves it
    // was asked for — which is the whole contract of the flag.
    let said = sync_with(&["com.palantir.javaformat:palantir-java-format:2.38.0"]);
    assert!(
        said.contains("palantir-java-format"),
        "the artifact was never reached for:\n{said}"
    );
}

#[test]
fn an_extension_and_classifier_may_be_given() {
    let said = sync_with(&["com.example:thing:zip:resources:1.0"]);
    assert!(said.contains("thing"), "{said}");
}

#[test]
fn a_malformed_coordinate_is_refused_by_name() {
    // Two parts is the common slip. It must not be turned into a request for a
    // path that cannot exist, and the message has to say which argument.
    let said = sync_with(&["com.example:thing"]);
    assert!(
        said.contains("com.example:thing") && said.contains("group:artifact:version"),
        "unhelpful refusal:\n{said}"
    );
}

#[test]
fn an_empty_part_is_refused() {
    let said = sync_with(&["com.example::1.0"]);
    assert!(said.contains("no part may be empty"), "{said}");
}
