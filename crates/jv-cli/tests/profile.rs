//! `jv profile`, which runs a build under the EventSpy and forwards its result.
//!
//! Nothing here runs Maven: what is under test is that the command is passed
//! through untouched, that the property is added, and that a missing jar is
//! refused in a way that says how to fix it.

use std::path::PathBuf;
use std::process::Command;

fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

fn run(arguments: &[&str]) -> (bool, String) {
    let output = Command::new(jv_binary())
        .arg("profile")
        .args(arguments)
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
fn a_missing_jar_says_where_it_looked_and_how_to_build_it() {
    let (ok, said) = run(&[
        "--profiler-jar",
        "/nonexistent/jv-profiler.jar",
        "--",
        "true",
    ]);
    assert!(!ok, "should have failed:\n{said}");
    assert!(said.contains("build.sh"), "no way forward given:\n{said}");
    assert!(
        said.contains("/nonexistent"),
        "did not say where it looked:\n{said}"
    );
}

#[test]
fn the_command_runs_and_its_exit_code_is_forwarded() {
    // The jar exists but the command is not Maven, so nothing loads it — which
    // is the point: `jv profile` must not care what it is wrapping.
    let jar = std::env::temp_dir().join("jv-profile-test.jar");
    std::fs::write(&jar, b"not really a jar").expect("the file");

    let (ok, _) = run(&[
        "--profiler-jar",
        jar.to_str().expect("a path"),
        "--",
        "true",
    ]);
    assert!(ok, "a successful command should report success");

    let (ok, _) = run(&[
        "--profiler-jar",
        jar.to_str().expect("a path"),
        "--",
        "false",
    ]);
    assert!(!ok, "a failing command's exit code must be forwarded");
}

#[test]
fn a_command_that_does_not_exist_is_reported_by_name() {
    let jar = std::env::temp_dir().join("jv-profile-test.jar");
    std::fs::write(&jar, b"not really a jar").expect("the file");
    let (ok, said) = run(&[
        "--profiler-jar",
        jar.to_str().expect("a path"),
        "--",
        "definitely-not-a-real-program",
    ]);
    assert!(!ok);
    assert!(
        said.contains("definitely-not-a-real-program"),
        "did not name the command:\n{said}"
    );
}
