//! `jv outdated`.
//!
//! Offline with an empty cache, so nothing here depends on a repository. That
//! makes the case under test the important one: what the command says when it
//! *cannot* answer.

use std::path::PathBuf;
use std::process::Command;

fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

fn run(pom_body: &str, extra: &[&str]) -> (bool, String) {
    let directory = tempfile::tempdir().expect("a temp dir");
    let pom = directory.path().join("pom.xml");
    std::fs::write(&pom, pom_body).expect("the POM");
    let settings = directory.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").expect("settings");

    let output = Command::new(jv_binary())
        .arg("outdated")
        .arg("-f")
        .arg(&pom)
        .arg("-s")
        .arg(&settings)
        .arg("--offline")
        .arg("--cache-dir")
        .arg(directory.path().join("cache"))
        .args(extra)
        .output()
        .expect("jv should run");
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

const WITH_DEPENDENCY: &str = "\
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>demo</artifactId>
  <version>1.0</version>
  <dependencies>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>2.0.9</version>
    </dependency>
  </dependencies>
</project>
";

#[test]
fn a_lookup_that_failed_is_never_reported_as_up_to_date() {
    // The failure this command must not have. "Up to date" and "I could not
    // check" are indistinguishable to a reader, and only one of them means what
    // it says — on the single question people run this to be sure about.
    let (ok, said) = run(WITH_DEPENDENCY, &[]);
    assert!(ok, "{said}");
    assert!(
        said.contains("could not be checked"),
        "an unanswerable check was not reported:\n{said}"
    );
    assert!(
        !said.contains("everything is up to date"),
        "claimed up to date without checking anything:\n{said}"
    );
    // And it says why, so the reader can tell a network problem from a typo.
    assert!(said.contains("offline") || said.contains("no versions"), "{said}");
}

#[test]
fn a_project_with_nothing_to_check_says_so() {
    let (ok, said) = run(
        "<project><modelVersion>4.0.0</modelVersion><groupId>g</groupId>\
         <artifactId>a</artifactId><version>1</version></project>",
        &[],
    );
    assert!(ok, "{said}");
    assert!(said.contains("up to date"), "{said}");
    assert!(said.contains("0 of 0") || said.contains("(0 of 0"), "{said}");
}

#[test]
fn exit_code_is_zero_when_nothing_is_known_to_be_outdated() {
    // `--exit-code` is a gate on *being outdated*, not on having failed to
    // look. A CI job that cannot reach the network should not be told its
    // dependencies are stale.
    let (ok, said) = run(WITH_DEPENDENCY, &["--exit-code"]);
    assert!(
        ok,
        "a failed lookup must not be reported as an outdated dependency:\n{said}"
    );
}

#[test]
fn a_dependency_with_no_version_is_not_checked() {
    // Its version comes from management; there is nothing here to bump.
    let (ok, said) = run(
        "<project><modelVersion>4.0.0</modelVersion><groupId>g</groupId>\
         <artifactId>a</artifactId><version>1</version><dependencies><dependency>\
         <groupId>org.slf4j</groupId><artifactId>slf4j-api</artifactId></dependency>\
         </dependencies></project>",
        &[],
    );
    assert!(ok, "{said}");
    assert!(
        !said.contains("slf4j"),
        "a managed dependency was checked anyway:\n{said}"
    );
}
