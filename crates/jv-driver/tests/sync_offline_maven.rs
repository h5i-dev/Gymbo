//! `jv sync && mvn -o verify` — the milestone this feature exists for.
//!
//! `jv sync` is jv's way into a CI pipeline that is not ready to stop using
//! Maven: jv does the downloading, Maven does the building. The only test that
//! establishes it works is running real Maven, offline, against a repository jv
//! populated and nothing else. Anything short of that is checking jv against
//! jv's own idea of what Maven wants.
//!
//! Skipped unless a Maven 3.9 is available. Point `JV_MVN` at one, or have `mvn`
//! on the path. `JV_REQUIRE_ORACLE=1` turns the absence into a failure, so a
//! missing Maven cannot quietly make this a no-op.

use std::path::{Path, PathBuf};
use std::process::Command;

use jv_driver::{Config, Session, SyncRequest, sync};

fn maven() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("JV_MVN") {
        let path = PathBuf::from(explicit);
        return match Command::new(&path).arg("-v").output() {
            Ok(output) if output.status.success() => Ok(path),
            _ => Err(format!(
                "JV_MVN points at {}, which does not run",
                path.display()
            )),
        };
    }
    match Command::new("mvn").arg("-v").output() {
        Ok(output) if output.status.success() => Ok(PathBuf::from("mvn")),
        _ => Err("no mvn on PATH and JV_MVN is unset".to_owned()),
    }
}

/// A project with a dependency, a test dependency, and a source file that uses
/// both — so `verify` genuinely compiles and runs something rather than
/// succeeding on an empty module.
fn write_project(directory: &Path) {
    std::fs::write(
        directory.join("pom.xml"),
        r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example.jvsync</groupId>
  <artifactId>synced</artifactId>
  <version>1.0.0</version>
  <properties>
    <maven.compiler.source>17</maven.compiler.source>
    <maven.compiler.target>17</maven.compiler.target>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
  <dependencies>
    <dependency>
      <groupId>com.fasterxml.jackson.core</groupId>
      <artifactId>jackson-databind</artifactId>
      <version>2.17.1</version>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>
"#,
    )
    .unwrap();

    let main = directory.join("src/main/java/com/example/jvsync");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::write(
        main.join("Greeting.java"),
        r#"package com.example.jvsync;

import com.fasterxml.jackson.databind.ObjectMapper;

public final class Greeting {
    public static String asJson(String who) throws Exception {
        return new ObjectMapper().writeValueAsString(java.util.Map.of("hello", who));
    }
}
"#,
    )
    .unwrap();

    let test = directory.join("src/test/java/com/example/jvsync");
    std::fs::create_dir_all(&test).unwrap();
    std::fs::write(
        test.join("GreetingTest.java"),
        r#"package com.example.jvsync;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.Test;

class GreetingTest {
    @Test
    void serializes() throws Exception {
        assertEquals("{\"hello\":\"world\"}", Greeting.asJson("world"));
    }
}
"#,
    )
    .unwrap();
}

#[test]
fn a_synced_repository_builds_offline() {
    let mvn = match maven() {
        Ok(mvn) => mvn,
        Err(reason) => {
            if std::env::var("JV_REQUIRE_ORACLE").is_ok() {
                panic!("JV_REQUIRE_ORACLE is set but {reason}");
            }
            eprintln!("skipping the offline-build gate: {reason}");
            return;
        }
    };

    let workspace = tempfile::tempdir().expect("a temp dir");
    let project_dir = workspace.path().join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    write_project(&project_dir);

    // Both repositories start empty. Maven is given only what jv puts in.
    let local_repository = workspace.path().join("m2");
    let cache = workspace.path().join("jv-cache");
    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();

    let config = Config {
        cache: Some(cache.clone()),
        user_settings: Some(settings.clone()),
        // The plugins the lifecycle binds appear in no POM, and `mvn -o` stops
        // at the first phase without them.
        lifecycle_bindings: true,
        ..Config::new().without_local_repository()
    };
    let session = Session::new(&config).expect("a session");
    let project = session
        .project_at(&project_dir.join("pom.xml"))
        .expect("the project");

    let report = sync(
        &session,
        &project.reactor(),
        &SyncRequest {
            local_repository: Some(local_repository.clone()),
            ..SyncRequest::default()
        },
    )
    .expect("a sync");

    assert!(
        !report.artifacts.is_empty(),
        "sync fetched nothing, so this proves nothing"
    );

    let output = Command::new(&mvn)
        .current_dir(&project_dir)
        .arg("--batch-mode")
        .arg("--offline")
        .arg(format!("-Dmaven.repo.local={}", local_repository.display()))
        .arg("-s")
        .arg(&settings)
        .arg("verify")
        .output()
        .expect("mvn runs");

    assert!(
        output.status.success(),
        "`mvn -o verify` failed against a repository jv populated.\n\
         synced {} artifacts, {} missing: {:?}\n\
         --- stdout ---\n{}\n--- stderr ---\n{}",
        report.artifacts.len(),
        report.missing.len(),
        report.missing,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
