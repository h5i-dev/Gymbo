//! `jv sync` warns when the machine cannot satisfy a build's toolchain.
//!
//! Toolchains do not affect resolution — `JdkVersionProfileActivator` reads
//! `java.version` and nothing in Maven's model building references them — so
//! there is no graph to get wrong here. What there is: a sync that reports
//! success and leaves a project `mvn -o verify` cannot build, failing later
//! with an error that never mentions `toolchains.xml`. jv is holding the POM at
//! the moment it could say so, so it does.
//!
//! The warning has to be quiet when the requirement *is* satisfied, or it
//! becomes noise people learn to ignore — which is the failure mode of a check
//! like this. Both directions are asserted.

use std::path::{Path, PathBuf};

use jv_driver::{Config, Session, SyncRequest};
use jv_model::toolchains::parse_toolchains;

/// A project requiring a JDK toolchain, with a `file:` repository so nothing
/// here needs the network.
fn project(directory: &Path, requirement: Option<&str>) -> PathBuf {
    let toolchains = requirement.map_or(String::new(), |version| {
        format!(
            r#"
      <plugin>
        <groupId>org.apache.maven.plugins</groupId>
        <artifactId>maven-toolchains-plugin</artifactId>
        <version>3.1.0</version>
        <configuration>
          <toolchains>
            <jdk><version>{version}</version></jdk>
          </toolchains>
        </configuration>
      </plugin>"#
        )
    });

    let pom = directory.join("pom.xml");
    std::fs::write(
        &pom,
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>needs-toolchain</artifactId>
  <version>1.0</version>
  <build>
    <plugins>{toolchains}
    </plugins>
  </build>
</project>"#
        ),
    )
    .unwrap();
    pom
}

/// A `toolchains.xml` providing one JDK, pointed at a directory that exists so
/// Maven's "jdkHome must exist" rule is satisfied.
fn toolchains_providing(version: &str, home: &Path) -> jv_model::toolchains::Toolchains {
    parse_toolchains(&format!(
        "<toolchains><toolchain><type>jdk</type>\
         <provides><version>{version}</version></provides>\
         <configuration><jdkHome>{}</jdkHome></configuration>\
         </toolchain></toolchains>",
        home.display()
    ))
}

fn warnings(pom: &Path, toolchains: jv_model::toolchains::Toolchains) -> Vec<String> {
    let cache = pom.parent().unwrap().join("cache");
    let settings = pom.parent().unwrap().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();

    let config = Config {
        user_settings: Some(settings),
        cache: Some(cache),
        ignore_local_repository: true,
        offline: true,
        ..Config::default()
    };
    let session = Session::new(&config).expect("a session");
    let project = session.project_at(pom).expect("the project loads");

    jv_driver::sync(
        &session,
        &[&project],
        &SyncRequest {
            // The project has no dependencies; only the toolchain check is
            // under test, and plugin resolution would need the network.
            plugins: false,
            plugin_dependencies: false,
            local_repository: None,
            toolchains,
            ..SyncRequest::default()
        },
    )
    .expect("sync runs")
    .warnings
}

#[test]
fn an_unsatisfiable_toolchain_is_reported() {
    let directory = tempfile::tempdir().unwrap();
    let pom = project(directory.path(), Some("[21,)"));
    let home = directory.path().join("jdk-11");
    std::fs::create_dir_all(&home).unwrap();

    let warnings = warnings(&pom, toolchains_providing("11", &home));
    assert!(
        warnings.iter().any(|warning| warning.contains("toolchain")),
        "a build requiring JDK 21 against a machine providing 11 should warn: {warnings:?}"
    );
}

#[test]
fn a_satisfied_toolchain_is_silent() {
    let directory = tempfile::tempdir().unwrap();
    let pom = project(directory.path(), Some("[11,)"));
    let home = directory.path().join("jdk-17");
    std::fs::create_dir_all(&home).unwrap();

    let warnings = warnings(&pom, toolchains_providing("17", &home));
    assert!(
        !warnings.iter().any(|warning| warning.contains("toolchain")),
        "17 satisfies [11,), so there is nothing to say: {warnings:?}"
    );
}

#[test]
fn a_project_without_the_plugin_is_never_warned_about() {
    let directory = tempfile::tempdir().unwrap();
    let pom = project(directory.path(), None);

    let warnings = warnings(&pom, jv_model::toolchains::Toolchains::default());
    assert!(
        !warnings.iter().any(|warning| warning.contains("toolchain")),
        "most projects declare no toolchain and must not be nagged: {warnings:?}"
    );
}

#[test]
fn a_toolchain_pointing_at_a_missing_jdk_does_not_count_as_provided() {
    // Maven discards a toolchain whose jdkHome is gone, so reporting it as a
    // match would promise something the build will not get.
    let directory = tempfile::tempdir().unwrap();
    let pom = project(directory.path(), Some("[17,)"));
    let absent = directory.path().join("uninstalled-jdk");

    let warnings = warnings(&pom, toolchains_providing("17", &absent));
    assert!(
        warnings.iter().any(|warning| warning.contains("toolchain")),
        "a toolchain whose jdkHome no longer exists must not satisfy anything: {warnings:?}"
    );
}
