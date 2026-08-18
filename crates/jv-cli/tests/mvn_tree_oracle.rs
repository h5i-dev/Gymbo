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
//! Every output format is compared, not just text. `tgf` and `graphml` id their
//! nodes with `Object.hashCode()`, which is a JVM identity hash and cannot be
//! reproduced by anything; those two are compared with ids normalised to their
//! order of first appearance, which still pins every label, every edge and the
//! whole document structure. `text`, `dot` and `json` are compared byte for byte.
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

/// The output formats to compare, and how.
///
/// `json` arrived in maven-dependency-plugin 3.7.0 — 3.6.1 silently falls back
/// to text for an unknown `-DoutputType`, which is the behaviour jv's own
/// `Format::from_str` deliberately refuses to copy.
const FORMATS: &[(&str, Comparison, Verbose)] = &[
    ("text", Comparison::Bytes, Verbose::No),
    ("dot", Comparison::Bytes, Verbose::No),
    ("json", Comparison::Bytes, Verbose::No),
    ("tgf", Comparison::ModuloNodeIds, Verbose::No),
    ("graphml", Comparison::ModuloNodeIds, Verbose::No),
    // `-Dverbose` is a separate renderer, not a flag on the one above: it keeps
    // the losers in the tree and annotates what conflict resolution did to the
    // survivors. It went uncompared until a hand-check found jv annotating
    // every node with "(scope updated from compile)" where Maven annotates
    // none, so it is in the matrix now.
    ("text", Comparison::Bytes, Verbose::Yes),
];

/// Whether to ask both tools for the verbose rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verbose {
    No,
    Yes,
}

/// How exactly a format can be compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Bytes,
    /// Upstream ids nodes with `Object.hashCode()` — a JVM identity hash, which
    /// differs between runs of Maven itself. Everything except the id values is
    /// still compared exactly.
    ModuloNodeIds,
}

/// The plugin version to run.
///
/// Pinned so this test does not change meaning when a new plugin is released,
/// and 3.7.0 rather than 3.6.1 because it is both what Maven 3.9.9's super POM
/// selects and the first version that implements `-DoutputType=json`.
const PLUGIN: &str = "org.apache.maven.plugins:maven-dependency-plugin:3.7.0:tree";

/// Renumbers node ids in order of first appearance, so two documents that
/// differ only in unreproducible ids compare equal.
///
/// Deliberately narrow. Only two shapes are rewritten: a line beginning with
/// digits followed by a space (tgf's node line), and the value of an `id`,
/// `source` or `target` attribute when it is entirely digits (graphml). A
/// label, a coordinate or a version is never touched, so a real difference
/// cannot hide behind this.
fn normalize_ids(text: &str) -> String {
    let mut ids: Vec<String> = Vec::new();
    let mut out = String::with_capacity(text.len());

    for line in text.lines() {
        // tgf writes `<id> <label>` for a node and `<source> <target> <label>`
        // for an edge, so a line can begin with one id or two. Consuming them in
        // a loop handles both without needing to know which section we are in. A
        // label never starts with digits followed by a space — coordinates have
        // no spaces at all — so this cannot eat one.
        let mut rest = line;
        loop {
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits == 0 || !rest[digits..].starts_with(' ') {
                break;
            }
            out.push_str(&renumber(&mut ids, &rest[..digits]));
            out.push(' ');
            rest = &rest[digits + 1..];
        }
        out.push_str(&rewrite_attributes(&mut ids, rest));
        out.push('\n');
    }
    out
}

/// Replaces each all-digit `id`/`source`/`target` attribute value.
fn rewrite_attributes(ids: &mut Vec<String>, line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some((before, name, value, after)) = next_attribute(rest) {
        out.push_str(before);
        out.push_str(&format!("{name}=\"{}\"", renumber(ids, value)));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The next `id="…"`, `source="…"` or `target="…"` whose value is all digits.
fn next_attribute(text: &str) -> Option<(&str, &'static str, &str, &str)> {
    let mut search_from = 0;
    loop {
        let (at, name) = ["id", "source", "target"]
            .into_iter()
            .filter_map(|name| {
                text[search_from..]
                    .find(&format!("{name}=\""))
                    .map(|offset| (search_from + offset, name))
            })
            .min_by_key(|(at, _)| *at)?;
        let value_start = at + name.len() + 2;
        let value_end = value_start + text[value_start..].find('"')?;
        let value = &text[value_start..value_end];
        if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some((&text[..at], name, value, &text[value_end + 1..]));
        }
        // Not an id we rewrite; keep looking past it.
        search_from = value_end + 1;
    }
}

/// `N0`, `N1`, … in order of first appearance.
fn renumber(ids: &mut Vec<String>, value: &str) -> String {
    let index = match ids.iter().position(|held| held == value) {
        Some(index) => index,
        None => {
            ids.push(value.to_owned());
            ids.len() - 1
        }
    };
    format!("N{index}")
}

/// Runs `mvn dependency:tree` in one output format and returns what it wrote.
fn maven_tree(
    mvn: &Path,
    project: &Path,
    local_repository: &Path,
    format: &str,
    verbose: Verbose,
) -> Result<String, String> {
    let mut command = Command::new(mvn);
    command
        .current_dir(project)
        .arg("-q")
        .arg("--batch-mode")
        .arg(format!("-Dmaven.repo.local={}", local_repository.display()))
        .arg(PLUGIN)
        .arg(format!("-DoutputType={format}"))
        .arg("-DoutputFile=tree.txt")
        .arg("-DappendOutput=false");
    if verbose == Verbose::Yes {
        command.arg("-Dverbose");
    }
    let output = command
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
fn jv_tree(project: &Path, cache: &Path, format: &str, verbose: Verbose) -> Result<String, String> {
    let mut command = Command::new(jv_binary());
    command
        .current_dir(project)
        .arg("tree")
        .arg("--output-type")
        .arg(format)
        .arg("--cache-dir")
        .arg(cache)
        .arg("--no-local-repository")
        .env("HOME", project);
    if verbose == Verbose::Yes {
        command.arg("--verbose");
    }
    let output = command
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

/// The comparable form of a document.
///
/// Line endings and trailing whitespace differ between the plugin's file writer
/// and jv's stdout, and neither is a resolution difference. Everything else is
/// left exactly as written — the trailing spaces *inside* a dot line are
/// upstream's and are load-bearing, which is why only the line end is trimmed
/// and only trailing blank lines are dropped.
fn normalize(text: &str, comparison: Comparison) -> String {
    let text = match comparison {
        Comparison::Bytes => text.to_owned(),
        Comparison::ModuloNodeIds => normalize_ids(text),
    };
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

        for (format, comparison, verbose) in FORMATS {
            let what = match verbose {
                Verbose::No => format!("{} / {format}", fixture.name),
                Verbose::Yes => format!("{} / {format} -Dverbose", fixture.name),
            };
            let expected = match maven_tree(&mvn, &project, &local_repository, format, *verbose) {
                Ok(tree) => tree,
                Err(reason) => {
                    differences.push(format!("{what}: {reason}"));
                    continue;
                }
            };
            let actual = match jv_tree(&project, &cache, format, *verbose) {
                Ok(tree) => tree,
                Err(reason) => {
                    differences.push(format!("{what}: {reason}"));
                    continue;
                }
            };

            // Two empty strings compare equal, so a case where both tools
            // produced nothing would pass while proving nothing.
            let lines = normalize(&expected, *comparison).lines().count();
            if lines < 2 {
                differences.push(format!(
                    "{what}: mvn produced {lines} line(s), which cannot be a real comparison:\n{expected}"
                ));
                continue;
            }
            compared += 1;

            if normalize(&expected, *comparison) != normalize(&actual, *comparison) {
                differences.push(format!(
                    "{what} ({}):\n--- mvn ---\n{}\n--- jv ---\n{}",
                    fixture.covers,
                    normalize(&expected, *comparison),
                    normalize(&actual, *comparison)
                ));
            }
        }
    }

    eprintln!(
        "compared {compared} of {} fixture/format pairs against mvn",
        FIXTURES.len() * FORMATS.len()
    );
    assert!(
        differences.is_empty(),
        "{} of {} fixture/format pairs differ from mvn dependency:tree:\n\n{}",
        differences.len(),
        FIXTURES.len() * FORMATS.len(),
        differences.join("\n\n")
    );
}
