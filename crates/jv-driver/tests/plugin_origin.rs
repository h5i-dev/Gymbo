//! `jv sync` fetches the closure of plugins a build can actually run.
//!
//! The optimisation under test skips the transitive dependencies of
//! `<pluginManagement>` entries that no `<plugins>` block declares, on the
//! grounds that management supplies a version and configuration to plugins that
//! *are* declared and never adds one to a build plan. On spring-petclinic that
//! removed 124 MB — a Kotlin compiler, jOOQ, Liquibase and Saxon, in a
//! single-module Java project.
//!
//! An optimisation like that is only worth having if the cases it must not
//! break are pinned, so this covers the three that decide it:
//!
//!   * a declared plugin keeps its closure;
//!   * a management-only plugin does not, but still gets its own jar and POM;
//!   * a plugin declared *inside an active profile* counts as declared, which
//!     is the case where getting it wrong turns a working CI job into a broken
//!     one — `jv sync` and `mvn` are separate invocations, and a job that
//!     passes `-P release` to one and not the other would otherwise sync a
//!     repository the build cannot use.
//!
//! Everything is served from a `file:` repository built here, so the test needs
//! no network and cannot be flaky.

use std::path::{Path, PathBuf};

use jv_driver::{Config, Session, SyncRequest};

/// Writes `group:artifact:version` with a POM, a jar, and optional dependencies.
fn publish(root: &Path, group: &str, artifact: &str, version: &str, dependencies: &[(&str, &str)]) {
    let directory = root
        .join(group.replace('.', "/"))
        .join(artifact)
        .join(version);
    std::fs::create_dir_all(&directory).unwrap();

    let deps = dependencies
        .iter()
        .map(|(a, v)| {
            format!(
                "<dependency><groupId>com.example</groupId><artifactId>{a}</artifactId>\
                 <version>{v}</version></dependency>"
            )
        })
        .collect::<String>();
    std::fs::write(
        directory.join(format!("{artifact}-{version}.pom")),
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version>
  <dependencies>{deps}</dependencies>
</project>"#
        ),
    )
    .unwrap();
    std::fs::write(
        directory.join(format!("{artifact}-{version}.jar")),
        format!("{artifact} {version}").as_bytes(),
    )
    .unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    repository: String,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");

    // Two plugins, each with a dependency that exists only for it, so the
    // presence of the dependency says which closure was walked.
    publish(
        &repository,
        "com.example",
        "declared-plugin",
        "1.0",
        &[("declared-only", "1.0")],
    );
    publish(
        &repository,
        "com.example",
        "managed-plugin",
        "1.0",
        &[("managed-only", "1.0")],
    );
    publish(
        &repository,
        "com.example",
        "profile-plugin",
        "1.0",
        &[("profile-only", "1.0")],
    );
    publish(&repository, "com.example", "declared-only", "1.0", &[]);
    publish(&repository, "com.example", "managed-only", "1.0", &[]);
    publish(&repository, "com.example", "profile-only", "1.0", &[]);

    let path = root.path().to_path_buf();
    Fixture {
        _root: root,
        repository: format!("file://{}", repository.display()),
        root: path,
    }
}

impl Fixture {
    /// A project declaring one plugin and managing another, plus a plugin in a
    /// profile that is active only when `-Drelease=on` is set.
    fn project(&self) -> PathBuf {
        let directory = self.root.join("project");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("pom.xml"),
            format!(
                r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId><artifactId>app</artifactId><version>1.0</version>
  <repositories>
    <repository><id>local-test</id><url>{repository}</url></repository>
  </repositories>
  <pluginRepositories>
    <pluginRepository><id>local-test</id><url>{repository}</url></pluginRepository>
  </pluginRepositories>
  <build>
    <plugins>
      <plugin>
        <groupId>com.example</groupId><artifactId>declared-plugin</artifactId><version>1.0</version>
      </plugin>
    </plugins>
    <pluginManagement>
      <plugins>
        <plugin>
          <groupId>com.example</groupId><artifactId>managed-plugin</artifactId><version>1.0</version>
        </plugin>
      </plugins>
    </pluginManagement>
  </build>
  <profiles>
    <profile>
      <id>release</id>
      <activation><property><name>release</name><value>on</value></property></activation>
      <build>
        <plugins>
          <plugin>
            <groupId>com.example</groupId><artifactId>profile-plugin</artifactId><version>1.0</version>
          </plugin>
        </plugins>
      </build>
    </profile>
  </profiles>
</project>"#,
                repository = self.repository
            ),
        )
        .unwrap();
        directory.join("pom.xml")
    }

    /// Syncs and returns every artifact id placed in the local repository.
    fn synced(&self, pom: &Path, properties: &[(&str, &str)]) -> Vec<String> {
        let settings = self.root.join("settings.xml");
        std::fs::write(&settings, "<settings/>").unwrap();
        let local = self.root.join("m2");
        let _ = std::fs::remove_dir_all(&local);

        let config = Config {
            user_settings: Some(settings),
            cache: Some(self.root.join("cache")),
            ignore_local_repository: true,
            user_properties: properties
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..Config::default()
        };
        let session = Session::new(&config).expect("a session");
        let project = session.project_at(pom).expect("the project loads");

        jv_driver::sync(
            &session,
            &[&project],
            &SyncRequest {
                local_repository: Some(local.clone()),
                ..SyncRequest::default()
            },
        )
        .expect("sync runs");

        let mut found = Vec::new();
        for entry in walk(&local) {
            if entry.extension().is_some_and(|e| e == "jar")
                && let Some(name) = entry.file_stem().and_then(|s| s.to_str())
            {
                found.push(name.to_owned());
            }
        }
        found.sort();
        found
    }
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn a_declared_plugins_closure_is_fetched() {
    let fixture = fixture();
    let synced = fixture.synced(&fixture.project(), &[]);
    assert!(
        synced
            .iter()
            .any(|name| name.starts_with("declared-plugin")),
        "the plugin itself: {synced:?}"
    );
    assert!(
        synced.iter().any(|name| name.starts_with("declared-only")),
        "and its dependency, which only it reaches: {synced:?}"
    );
}

#[test]
fn a_management_only_plugin_keeps_its_jar_but_not_its_closure() {
    let fixture = fixture();
    let synced = fixture.synced(&fixture.project(), &[]);
    assert!(
        synced.iter().any(|name| name.starts_with("managed-plugin")),
        "the plugin's own jar is cheap and still fetched: {synced:?}"
    );
    assert!(
        !synced.iter().any(|name| name.starts_with("managed-only")),
        "nothing declares it, so no build plan loads its dependencies: {synced:?}"
    );
}

#[test]
fn a_plugin_declared_in_an_active_profile_counts_as_declared() {
    // The case that decides whether this optimisation is safe in CI. A profile
    // that is active during the sync contributes a *declared* plugin, and its
    // closure has to travel with it.
    let fixture = fixture();
    let synced = fixture.synced(&fixture.project(), &[("release", "on")]);
    assert!(
        synced.iter().any(|name| name.starts_with("profile-plugin")),
        "the profile's plugin: {synced:?}"
    );
    assert!(
        synced.iter().any(|name| name.starts_with("profile-only")),
        "and its closure, which is what makes the synced repository usable: {synced:?}"
    );
}

#[test]
fn an_inactive_profiles_plugin_is_absent_entirely() {
    // The honest limitation, pinned so it is a decision rather than a surprise:
    // a profile jv did not activate contributes nothing, so a build that
    // activates it later has neither the plugin nor its dependencies. `jv sync`
    // and `mvn` must be given the same profiles.
    let fixture = fixture();
    let synced = fixture.synced(&fixture.project(), &[]);
    assert!(
        !synced.iter().any(|name| name.starts_with("profile-plugin")),
        "an inactive profile contributes nothing: {synced:?}"
    );
}
