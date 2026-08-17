//! `jv tree` against `mvn dependency:tree`, byte for byte.
//!
//! This is the test the whole project is aimed at. Everything else checks a
//! layer against a transcribed corpus; this one runs real Maven on real POMs
//! resolved from Maven Central and demands the same bytes out. A difference here
//! is a difference a user would see.
//!
//! Skipped unless a Maven 3.9 is available. Point `JV_MVN` at one, or have `mvn`
//! on the path. Set `JV_REQUIRE_ORACLE=1` to make the absence a failure, which
//! is what CI does so that a missing Maven cannot quietly turn this into a
//! no-op.
//!
//! # Why the fixtures are what they are
//!
//! Each project is chosen for a resolution behaviour, not for being popular:
//! nearest-wins over a diamond, `dependencyManagement` reaching a transitive
//! dependency, a BOM import, an exclusion, the scope derivation matrix, optional
//! dependencies, and a wide graph where conflict ordering decides the outcome.
//! Each fixture carries the behaviour it covers, so a failure says what broke
//! rather than only which project it broke on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A Maven to compare against, or the reason there is none.
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

/// The `jv` binary this test run built.
///
/// Cargo puts integration-test binaries beside the ones they test, so the
/// executable is two directories up from the test's own path.
fn jv_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    path.join(if cfg!(windows) { "jv.exe" } else { "jv" })
}

/// One project to compare, and why it is here.
struct Fixture {
    name: &'static str,
    /// What behaviour a difference here would indicate.
    covers: &'static str,
    dependencies: &'static str,
    /// Extra POM body, before `<dependencies>`.
    extra: &'static str,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "plain",
        covers: "a transitive chain with no conflicts at all",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-databind</artifactId>
              <version>2.17.1</version>
            </dependency>"#,
    },
    Fixture {
        name: "nearest-wins",
        covers: "two versions of one artifact at different depths",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-databind</artifactId>
              <version>2.17.1</version>
            </dependency>
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-core</artifactId>
              <version>2.15.2</version>
            </dependency>"#,
    },
    Fixture {
        name: "managed-transitive",
        covers: "dependencyManagement reaching a dependency it does not declare",
        extra: r#"
            <dependencyManagement><dependencies>
              <dependency>
                <groupId>com.fasterxml.jackson.core</groupId>
                <artifactId>jackson-core</artifactId>
                <version>2.16.0</version>
              </dependency>
            </dependencies></dependencyManagement>"#,
        dependencies: r#"
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-databind</artifactId>
              <version>2.17.1</version>
            </dependency>"#,
    },
    Fixture {
        name: "bom-import",
        covers: "a BOM import supplying versions across a whole family",
        extra: r#"
            <dependencyManagement><dependencies>
              <dependency>
                <groupId>com.fasterxml.jackson</groupId>
                <artifactId>jackson-bom</artifactId>
                <version>2.17.1</version>
                <type>pom</type>
                <scope>import</scope>
              </dependency>
            </dependencies></dependencyManagement>"#,
        dependencies: r#"
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-databind</artifactId>
            </dependency>
            <dependency>
              <groupId>com.fasterxml.jackson.dataformat</groupId>
              <artifactId>jackson-dataformat-yaml</artifactId>
            </dependency>"#,
    },
    Fixture {
        name: "exclusion",
        covers: "an exclusion pruning a subtree, not just one node",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>com.fasterxml.jackson.core</groupId>
              <artifactId>jackson-databind</artifactId>
              <version>2.17.1</version>
              <exclusions><exclusion>
                <groupId>com.fasterxml.jackson.core</groupId>
                <artifactId>jackson-annotations</artifactId>
              </exclusion></exclusions>
            </dependency>"#,
    },
    Fixture {
        name: "scopes",
        covers: "the scope derivation matrix across a deep test-scoped tree",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>org.junit.jupiter</groupId>
              <artifactId>junit-jupiter</artifactId>
              <version>5.10.2</version>
              <scope>test</scope>
            </dependency>
            <dependency>
              <groupId>jakarta.servlet</groupId>
              <artifactId>jakarta.servlet-api</artifactId>
              <version>6.0.0</version>
              <scope>provided</scope>
            </dependency>"#,
    },
    Fixture {
        name: "optional",
        covers: "optional dependencies, which do not propagate",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>org.apache.logging.log4j</groupId>
              <artifactId>log4j-core</artifactId>
              <version>2.23.1</version>
            </dependency>"#,
    },
    Fixture {
        name: "deep",
        covers: "a wide graph where conflict ordering decides the outcome",
        extra: "",
        dependencies: r#"
            <dependency>
              <groupId>org.apache.httpcomponents.client5</groupId>
              <artifactId>httpclient5</artifactId>
              <version>5.3.1</version>
            </dependency>
            <dependency>
              <groupId>com.google.guava</groupId>
              <artifactId>guava</artifactId>
              <version>33.2.0-jre</version>
            </dependency>
            <dependency>
              <groupId>org.slf4j</groupId>
              <artifactId>slf4j-api</artifactId>
              <version>2.0.13</version>
            </dependency>"#,
    },
];

impl Fixture {
    fn pom(&self) -> String {
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example.jvtest</groupId>
  <artifactId>{}</artifactId>
  <version>1.0.0</version>
  {}
  <dependencies>{}
  </dependencies>
</project>
"#,
            self.name, self.extra, self.dependencies
        )
    }
}

/// Runs `mvn dependency:tree` and returns just the tree.
///
/// Maven wraps every line in `[INFO] ` and surrounds the tree with build
/// chatter, so the tree has to be cut out of the log. The root line is the
/// project's own coordinates; the tree runs to the first blank or non-tree line.
fn maven_tree(mvn: &Path, project: &Path, local_repository: &Path) -> Result<String, String> {
    let output = Command::new(mvn)
        .current_dir(project)
        .arg("-q")
        .arg("--batch-mode")
        .arg("-Dmaven.repo.local")
        .arg(format!("-Dmaven.repo.local={}", local_repository.display()))
        // Pinning the plugin version keeps this test from changing meaning when
        // a new dependency-plugin is released.
        .arg("org.apache.maven.plugins:maven-dependency-plugin:3.6.1:tree")
        .arg("-DoutputFile=tree.txt")
        .arg("-DappendOutput=false")
        .output()
        .map_err(|error| format!("cannot run {}: {error}", mvn.display()))?;

    if !output.status.success() {
        return Err(format!(
            "mvn failed in {}:\n{}\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    std::fs::read_to_string(project.join("tree.txt"))
        .map_err(|error| format!("mvn wrote no tree.txt: {error}"))
}

/// Runs `jv tree` and returns its output.
///
/// `--no-local-repository` keeps jv off `~/.m2`, so both tools start from
/// nothing and neither is advantaged by a cache the other does not have. `HOME`
/// is redirected at the project so a developer's own `settings.xml` cannot
/// change what this resolves.
fn jv_tree(project: &Path, cache: &Path) -> Result<String, String> {
    let output = Command::new(jv_binary())
        .current_dir(project)
        .arg("tree")
        .arg("--cache-dir")
        .arg(cache)
        .arg("--no-local-repository")
        .env("HOME", project)
        .output()
        .map_err(|error| format!("cannot run jv: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "jv failed in {}:\n{}",
            project.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Trailing whitespace and line endings differ between the plugin's file output
/// and jv's stdout; neither is a resolution difference.
fn normalize(text: &str) -> String {
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty())
        .collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[test]
fn jv_tree_matches_mvn_dependency_tree() {
    let mvn = match maven() {
        Ok(mvn) => mvn,
        Err(reason) => {
            if std::env::var("JV_REQUIRE_ORACLE").is_ok() {
                panic!("JV_REQUIRE_ORACLE is set but {reason}");
            }
            eprintln!("skipping the mvn dependency:tree oracle: {reason}");
            return;
        }
    };

    let workspace = tempfile::tempdir().expect("a temp dir");
    // One local repository across all fixtures: they share most of their
    // dependencies, and downloading them once keeps the test to one round of
    // network traffic.
    let local_repository = workspace.path().join("m2");
    let cache = workspace.path().join("jv-cache");

    let mut differences = Vec::new();
    let mut compared = 0usize;
    for fixture in FIXTURES {
        let project = workspace.path().join(fixture.name);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("pom.xml"), fixture.pom()).unwrap();

        let expected = match maven_tree(&mvn, &project, &local_repository) {
            Ok(tree) => tree,
            Err(reason) => {
                differences.push(format!("{}: {reason}", fixture.name));
                continue;
            }
        };
        let actual = match jv_tree(&project, &cache) {
            Ok(tree) => tree,
            Err(reason) => {
                differences.push(format!("{}: {reason}", fixture.name));
                continue;
            }
        };

        // Two empty strings compare equal, so a fixture where both tools
        // produced nothing would pass while proving nothing.
        let lines = normalize(&expected).lines().count();
        if lines < 2 {
            differences.push(format!(
                "{}: mvn produced a {lines}-line tree, which cannot be a real comparison:\n{expected}",
                fixture.name
            ));
            continue;
        }
        compared += 1;

        if normalize(&expected) != normalize(&actual) {
            differences.push(format!(
                "{} ({}):\n--- mvn ---\n{}\n--- jv ---\n{}",
                fixture.name,
                fixture.covers,
                normalize(&expected),
                normalize(&actual)
            ));
        }
    }

    eprintln!(
        "compared {compared} of {} fixtures against mvn",
        FIXTURES.len()
    );
    assert!(
        differences.is_empty(),
        "{} of {} fixtures differ from mvn dependency:tree:\n\n{}",
        differences.len(),
        FIXTURES.len(),
        differences.join("\n\n")
    );
}
