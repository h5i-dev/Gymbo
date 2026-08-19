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
    assert!(
        said.contains("offline") || said.contains("no versions"),
        "{said}"
    );
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
    assert!(
        said.contains("0 of 0") || said.contains("(0 of 0"),
        "{said}"
    );
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

// ------------------------------------------------- managed versions

/// A project laid out as parent + child, so inheritance is real rather than
/// simulated.
fn run_in(directory: &std::path::Path, pom: &std::path::Path, extra: &[&str]) -> (bool, String) {
    let settings = directory.join("settings.xml");
    std::fs::write(&settings, "<settings/>").expect("settings");
    let output = Command::new(jv_binary())
        .arg("outdated")
        .arg("-f")
        .arg(pom)
        .arg("-s")
        .arg(&settings)
        .arg("--offline")
        .arg("--cache-dir")
        .arg(directory.join("cache"))
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

#[test]
fn a_managed_version_this_pom_declares_is_considered() {
    // Offline nothing can be resolved, so the proof that it was *considered* is
    // that it appears among what could not be checked. That is enough: the
    // question here is which candidates are gathered, not what the answer is.
    let directory = tempfile::tempdir().expect("a temp dir");
    let pom = directory.path().join("pom.xml");
    std::fs::write(
        &pom,
        "<project><modelVersion>4.0.0</modelVersion><groupId>com.example</groupId>\
         <artifactId>demo</artifactId><version>1.0</version><dependencyManagement>\
         <dependencies><dependency><groupId>org.slf4j</groupId>\
         <artifactId>slf4j-api</artifactId><version>2.0.9</version></dependency>\
         </dependencies></dependencyManagement></project>",
    )
    .expect("the POM");

    let (ok, said) = run_in(directory.path(), &pom, &[]);
    assert!(ok, "{said}");
    assert!(
        said.contains("slf4j-api"),
        "a managed version this POM declares was not considered:\n{said}"
    );
}

#[test]
fn a_version_managed_by_a_parent_is_left_out() {
    // The reason the raw POM is re-read. An effective model cannot tell these
    // apart, and only one of them is a version the reader can change here.
    let directory = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
        directory.path().join("pom.xml"),
        "<project><modelVersion>4.0.0</modelVersion><groupId>com.example</groupId>\
         <artifactId>parent</artifactId><version>1.0</version><packaging>pom</packaging>\
         <dependencyManagement><dependencies><dependency><groupId>org.slf4j</groupId>\
         <artifactId>slf4j-api</artifactId><version>2.0.9</version></dependency>\
         </dependencies></dependencyManagement><modules><module>child</module></modules></project>",
    )
    .expect("the parent");

    let child = directory.path().join("child");
    std::fs::create_dir_all(&child).expect("the child directory");
    let child_pom = child.join("pom.xml");
    std::fs::write(
        &child_pom,
        "<project><modelVersion>4.0.0</modelVersion><parent><groupId>com.example</groupId>\
         <artifactId>parent</artifactId><version>1.0</version></parent>\
         <artifactId>child</artifactId></project>",
    )
    .expect("the child");

    let (ok, said) = run_in(directory.path(), &child_pom, &["--no-recursive"]);
    assert!(ok, "{said}");
    assert!(
        !said.contains("slf4j"),
        "a version the parent manages was reported against the child, where it \
         cannot be changed:\n{said}"
    );
}

#[test]
fn an_imported_bom_is_considered() {
    // Model building expands an import and the entry itself disappears from the
    // effective model, so this only works because the raw version is used as a
    // fallback. It is the entry most worth reporting: for a project that takes
    // its versions from a BOM, bumping the import is the upgrade.
    let directory = tempfile::tempdir().expect("a temp dir");
    let pom = directory.path().join("pom.xml");
    std::fs::write(
        &pom,
        "<project><modelVersion>4.0.0</modelVersion><groupId>com.example</groupId>\
         <artifactId>demo</artifactId><version>1.0</version><dependencyManagement>\
         <dependencies><dependency><groupId>com.fasterxml.jackson</groupId>\
         <artifactId>jackson-bom</artifactId><version>2.16.1</version>\
         <type>pom</type><scope>import</scope></dependency>\
         </dependencies></dependencyManagement></project>",
    )
    .expect("the POM");

    let (ok, said) = run_in(directory.path(), &pom, &[]);
    assert!(ok, "{said}");
    assert!(
        said.contains("jackson-bom"),
        "the imported BOM was not considered:\n{said}"
    );
}
