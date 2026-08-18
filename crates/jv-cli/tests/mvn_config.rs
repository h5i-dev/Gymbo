//! `.mvn/maven.config` reaches resolution.
//!
//! The gap this closes is not a missing feature but a silent divergence: a
//! project that sets `-Dsomething` in `.mvn/maven.config` resolves one graph
//! under `mvn` and a different one under a tool that ignores the file, and
//! neither output mentions the file. A unit test over the parser cannot catch
//! that, because the parser was never called.
//!
//! So this drives the real binary and asserts on the tree it prints. The
//! dependency the profile adds is served from a `file:` repository built here,
//! so the test needs no network and cannot be flaky: what is under test is
//! whether the config file was read, not whether Central is up.
//!
//! Every case matches behaviour checked against Maven 3.9.9 first — see the
//! table in `jv_driver::mvn_config`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

/// Lays out `com.example:extra:1.0` in a `file:` repository.
fn repository(root: &Path) -> String {
    let directory = root.join("com").join("example").join("extra").join("1.0");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("extra-1.0.pom"),
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>extra</artifactId>
  <version>1.0</version>
</project>"#,
    )
    .unwrap();
    std::fs::write(directory.join("extra-1.0.jar"), b"not really a jar").unwrap();
    format!("file://{}", root.display())
}

/// A project whose `extra` profile is activated by `-Dactivator=on`, and which
/// depends on `com.example:extra` only when it is.
fn project(directory: &Path, repository_url: &str, config: Option<&str>) {
    std::fs::write(
        directory.join("pom.xml"),
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>cfg</artifactId>
  <version>1.0</version>
  <repositories>
    <repository>
      <id>local-test</id>
      <url>{repository_url}</url>
    </repository>
  </repositories>
  <profiles>
    <profile>
      <id>extra</id>
      <activation>
        <property><name>activator</name><value>on</value></property>
      </activation>
      <dependencies>
        <dependency>
          <groupId>com.example</groupId>
          <artifactId>extra</artifactId>
          <version>1.0</version>
        </dependency>
      </dependencies>
    </profile>
  </profiles>
</project>"#
        ),
    )
    .unwrap();

    let dot_mvn = directory.join(".mvn");
    std::fs::create_dir_all(&dot_mvn).unwrap();
    if let Some(config) = config {
        std::fs::write(dot_mvn.join("maven.config"), config).unwrap();
    }
    std::fs::write(directory.join("settings.xml"), "<settings/>").unwrap();
}

/// Runs `jv tree` from inside `directory`, since `.mvn` is found by walking up
/// from the working directory.
fn jv_tree(directory: &Path, cache: &Path, extra: &[&str]) -> String {
    let output = Command::new(jv_binary())
        .current_dir(directory)
        .arg("tree")
        .arg("--cache-dir")
        .arg(cache)
        .arg("--no-local-repository")
        .arg("-s")
        .arg(directory.join("settings.xml"))
        .args(extra)
        .env("HOME", directory)
        .output()
        .expect("jv runs");
    assert!(
        output.status.success(),
        "jv tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One project per case, sharing a repository and a cache.
struct Workspace {
    _root: tempfile::TempDir,
    repository: String,
    cache: PathBuf,
    root: PathBuf,
}

fn workspace() -> Workspace {
    let root = tempfile::tempdir().unwrap();
    let repository = repository(&root.path().join("repository"));
    let cache = root.path().join("cache");
    let path = root.path().to_path_buf();
    Workspace {
        _root: root,
        repository,
        cache,
        root: path,
    }
}

impl Workspace {
    fn project(&self, name: &str, config: Option<&str>) -> PathBuf {
        let directory = self.root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        project(&directory, &self.repository, config);
        directory
    }
}

#[test]
fn maven_config_properties_change_what_resolves() {
    let workspace = workspace();

    let baseline = jv_tree(&workspace.project("without", None), &workspace.cache, &[]);
    assert!(
        !baseline.contains("com.example:extra"),
        "the profile should be off without the config:\n{baseline}"
    );

    let configured = jv_tree(
        &workspace.project("with", Some("-Dactivator=on\n")),
        &workspace.cache,
        &[],
    );
    assert!(
        configured.contains("com.example:extra"),
        ".mvn/maven.config did not reach profile activation:\n{configured}"
    );
}

#[test]
fn the_command_line_beats_maven_config() {
    let workspace = workspace();
    let directory = workspace.project("conflict", Some("-Dactivator=on\n"));

    // Checked against Maven 3.9.9 on exactly this conflict: the command line
    // wins, because the file's arguments are parsed first.
    let tree = jv_tree(&directory, &workspace.cache, &["-Dactivator=off"]);
    assert!(
        !tree.contains("com.example:extra"),
        "a command-line -D must override the same key in .mvn/maven.config:\n{tree}"
    );
}

#[test]
fn a_config_line_maven_would_ignore_is_ignored_here_too() {
    let workspace = workspace();
    // Maven does not trim these lines, so the argument is unparseable and the
    // profile stays off. Honouring it would make jv resolve a graph the real
    // build never produces — a divergence in the helpful direction is still a
    // divergence.
    let directory = workspace.project("padded", Some("  -Dactivator=on  \n"));

    let tree = jv_tree(&directory, &workspace.cache, &[]);
    assert!(
        !tree.contains("com.example:extra"),
        "a padded config line must be ignored, as Maven ignores it:\n{tree}"
    );
}

#[test]
fn comments_and_blank_lines_do_not_stop_the_argument_being_read() {
    let workspace = workspace();
    let directory = workspace.project("commented", Some("# why\n\n-Dactivator=on\n"));
    let tree = jv_tree(&directory, &workspace.cache, &[]);
    assert!(tree.contains("com.example:extra"), "{tree}");
}

#[test]
fn the_config_is_found_from_a_subdirectory() {
    let workspace = workspace();
    let root = workspace.project("root", Some("-Dactivator=on\n"));

    // A module directory below the one holding `.mvn`, with its own POM.
    let module = root.join("module");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::copy(root.join("pom.xml"), module.join("pom.xml")).unwrap();
    std::fs::copy(root.join("settings.xml"), module.join("settings.xml")).unwrap();

    let tree = jv_tree(&module, &workspace.cache, &[]);
    assert!(
        tree.contains("com.example:extra"),
        ".mvn must be found by walking up, as Maven's launcher does:\n{tree}"
    );
}
