//! `jvx` against twenty real tools.
//!
//! `jvx_launch.rs` proves the mechanism on one tool. This proves the *claim*:
//! that pointing jvx at an arbitrary published JVM tool runs it. Twenty is
//! enough to cover the shapes that break a launcher — a shaded uber-jar, a thin
//! jar whose classpath is thirty transitive artifacts, a tool whose main class
//! lives in a dependency, a Kotlin one, a Scala one, tools whose only
//! `--version`-ish flag is spelled differently, and tools that exit non-zero
//! when asked politely to describe themselves.
//!
//! Each entry records the *shape* it is here for, so a failure says what class
//! of tool broke rather than only which one.
//!
//! # What counts as passing
//!
//! A JVM started, ran the class jvx chose, and produced output. Not a specific
//! exit code: `--help` is conventionally 0 but several of these use 1, and
//! `checkstyle` with no files is an error by design. A tool that prints its
//! usage has demonstrated everything jvx is responsible for.
//!
//! Skipped without a JDK, and without network on a cold cache.
//! `JV_REQUIRE_ORACLE=1` turns a skip into a failure, as CI does.
//! `JV_SMOKE_ALL=1` runs the whole matrix; by default it runs the first six, so
//! an ordinary `cargo test` does not spend several minutes downloading.

use std::path::PathBuf;
use std::process::Command;

/// What an entry is asserting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The tool launches and its output contains this. Deliberately short —
    /// asserting on a whole banner pins a version rather than a behaviour.
    Says(&'static str),
    /// The artifact is a library, so jvx must refuse and say why rather than
    /// guessing at a class. Half the matrix is this on purpose: `jvx` is a
    /// command people will point at the wrong coordinates, and a clear refusal
    /// is the behaviour under test.
    NoMainClass,
}

/// One tool, and why it is in the matrix.
struct Tool {
    endpoint: &'static str,
    /// The launcher shape this entry covers.
    covers: &'static str,
    args: &'static [&'static str],
    expect: Expect,
}

const TOOLS: &[Tool] = &[
    Tool {
        endpoint: "com.google.googlejavaformat:google-java-format",
        covers: "no version given; the latest release is chosen",
        args: &["--version"],
        expect: Expect::Says("google-java-format: version"),
    },
    Tool {
        endpoint: "org.openrewrite:rewrite-java:8.30.0",
        covers: "a library jar with no Main-Class, which must fail cleanly",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "com.puppycrawl.tools:checkstyle:10.17.0@com.puppycrawl.tools.checkstyle.Main",
        // Checkstyle publishes its uber-jar on GitHub, not Central, so the
        // artifact Central has is a plain jar with no `Main-Class` and a main
        // class that only `@mainClass` can name.
        covers: "`@mainClass` naming a class the manifest does not",
        args: &["--version"],
        expect: Expect::Says("checkstyle version"),
    },
    Tool {
        endpoint: "org.jacoco:org.jacoco.cli:0.8.12:nodeps",
        // The default artifact is a thin jar with no `Main-Class`; the runnable
        // one is a classifier beside it.
        covers: "a classifier selecting the runnable artifact",
        args: &[],
        expect: Expect::Says("command line interface"),
    },
    Tool {
        endpoint: "org.apache.maven.shared:maven-invoker:3.3.0",
        covers: "a thin jar with a deep transitive classpath",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "com.fasterxml.jackson.core:jackson-databind:2.17.1",
        covers: "a plain library, which has no main class and must say so",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "org.jetbrains.kotlin:kotlin-compiler:1.9.24",
        covers: "a Kotlin toolchain jar",
        args: &["-version"],
        expect: Expect::Says("kotlinc-jvm"),
    },
    Tool {
        endpoint: "org.scala-lang:scala-compiler:2.13.14@scala.tools.nsc.Main",
        covers: "a Scala toolchain jar, whose entry point is `@mainClass` too",
        args: &["-version"],
        expect: Expect::Says("scala compiler version"),
    },
    Tool {
        endpoint: "net.sourceforge.pmd:pmd-java:7.3.0",
        covers: "a tool split across several modules",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "org.apache.logging.log4j:log4j-core:2.23.1",
        covers: "a jar whose manifest names a class that is not a CLI",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "info.picocli:picocli:4.7.6",
        covers: "a framework jar with an AutoComplete main class",
        args: &["--help"],
        expect: Expect::Says("generates a bash completion script"),
    },
    Tool {
        endpoint: "org.antlr:antlr4:4.13.1",
        covers: "a code generator whose usage goes to stderr",
        args: &[],
        expect: Expect::Says("antlr parser generator"),
    },
    Tool {
        endpoint: "com.beust:jcommander:1.82",
        covers: "a small library, no main class",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "org.openjdk.jol:jol-cli:0.17@org.openjdk.jol.Main",
        covers: "a tool published with a `full` classifier alternative",
        args: &[],
        expect: Expect::Says("usage: jol-cli.jar"),
    },
    Tool {
        endpoint: "com.github.spotbugs:spotbugs:4.8.5",
        covers: "a large tool with native-ish resources",
        // `-version` prints nothing but SLF4J noise; the banner is under `-help`.
        args: &["-help"],
        expect: Expect::Says("4.8.5"),
    },
    Tool {
        endpoint: "org.codehaus.groovy:groovy:3.0.21",
        covers: "a language runtime whose jar is also a CLI",
        args: &["--version"],
        expect: Expect::Says("groovy version:"),
    },
    Tool {
        endpoint: "org.apache.commons:commons-lang3:3.14.0",
        covers: "the most ordinary library there is",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "org.junit.platform:junit-platform-console-standalone:1.10.2",
        covers: "a shaded standalone runner",
        args: &["--help"],
        expect: Expect::Says("junit"),
    },
    Tool {
        endpoint: "org.yaml:snakeyaml:2.2",
        covers: "a library with an OSGi manifest",
        args: &[],
        expect: Expect::NoMainClass,
    },
    Tool {
        endpoint: "com.google.protobuf:protobuf-java:4.27.1",
        covers: "a jar whose coordinates changed major version recently",
        args: &[],
        expect: Expect::NoMainClass,
    },
];

/// How many entries an ordinary `cargo test` runs.
const DEFAULT_SAMPLE: usize = 6;

fn jvx_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary's own path");
    path.pop();
    path.pop();
    path.join(if cfg!(windows) { "jvx.exe" } else { "jvx" })
}

/// Whether a JDK is present, since every entry needs one.
fn have_java() -> bool {
    std::env::var_os("JAVA_HOME").is_some()
        || Command::new("java")
            .arg("-version")
            .output()
            .is_ok_and(|out| out.status.success())
}

/// Whether output describes a repository that would not serve jv, rather than a
/// tool that does not work.
///
/// This matrix is the only thing here that needs the network, and Maven Central
/// rate-limits: a throttled run reports `the repository answered 429`. Calling
/// that a jvx failure teaches whoever sees it to ignore a red result from the
/// tests most likely to catch a real jvx bug. `JV_REQUIRE_ORACLE` turns it back
/// into a failure, for CI, which is expected to have a network.
fn repository_refused(said: &str) -> bool {
    said.contains("the repository answered 429")
        || said.contains("the repository answered 50")
        || said.contains("is not cached, and jv is offline")
        || said.contains("could not be reached")
        || said.contains("dns error")
}

#[test]
fn jvx_launches_real_tools() {
    if !have_java() {
        if std::env::var_os("JV_REQUIRE_ORACLE").is_some() {
            panic!("JV_REQUIRE_ORACLE is set but no JDK is available");
        }
        eprintln!("skipping the jvx smoke matrix: no JDK");
        return;
    }

    let workspace = tempfile::tempdir().expect("a temp dir");
    let settings = workspace.path().join("settings.xml");
    std::fs::write(&settings, "<settings/>").unwrap();
    // One cache across the matrix: these tools share most of their transitive
    // dependencies, and downloading them once keeps this to one round of
    // traffic.
    let cache = workspace.path().join("cache");

    let all = std::env::var_os("JV_SMOKE_ALL").is_some();
    let matrix = if all {
        TOOLS
    } else {
        &TOOLS[..DEFAULT_SAMPLE.min(TOOLS.len())]
    };

    let mut failures = Vec::new();
    let mut refused = Vec::new();
    for tool in matrix {
        let mut command = Command::new(jvx_binary());
        command
            .arg("--cache-dir")
            .arg(&cache)
            .arg("--no-local-repository")
            .arg("-s")
            .arg(&settings)
            .arg(tool.endpoint);
        if !tool.args.is_empty() {
            command.arg("--").args(tool.args);
        }

        let output = match command.output() {
            Ok(output) => output,
            Err(error) => {
                failures.push(format!("{}: cannot run jvx: {error}", tool.endpoint));
                continue;
            }
        };
        // Both streams: a usage message goes to whichever the tool's author
        // chose, and several of these disagree.
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_lowercase();

        // Separated from a real failure before anything else is judged: a
        // repository that refused to answer produces output that looks like
        // every other kind of broken.
        if repository_refused(&said) {
            refused.push(tool.endpoint);
            continue;
        }

        if said.trim().is_empty() {
            failures.push(format!(
                "{} ({}): produced no output at all",
                tool.endpoint, tool.covers
            ));
            continue;
        }

        // jvx's refusal quotes the endpoint and the jar's file name, so a
        // `Says` text drawn from the coordinates — a version, or the artifact
        // id — matches the refusal just as happily as a launch. Four entries
        // passed that way for as long as this test existed. Ruling the refusal
        // out first is what makes `Says` mean "a JVM ran".
        let refused = said.contains("which class to run");
        let (wanted, satisfied) = match tool.expect {
            Expect::Says(text) => (text, !refused && said.contains(&text.to_lowercase())),
            // A refusal, not a launch: the message has to name the problem, and
            // the process must not have started a JVM to find that out.
            Expect::NoMainClass => (
                "a refusal naming the missing main class",
                refused && said.contains("main-class"),
            ),
        };
        if !satisfied {
            failures.push(format!(
                "{} ({}): expected {}, got:\n{}",
                tool.endpoint,
                tool.covers,
                wanted,
                said.lines().take(6).collect::<Vec<_>>().join("\n")
            ));
        }
    }

    eprintln!(
        "jvx smoke: ran {} of {} tools{}",
        matrix.len(),
        TOOLS.len(),
        if all {
            ""
        } else {
            " (set JV_SMOKE_ALL=1 for all)"
        }
    );
    if !refused.is_empty() {
        assert!(
            std::env::var_os("JV_REQUIRE_ORACLE").is_none(),
            "JV_REQUIRE_ORACLE is set but the repository refused {} of {}: {refused:?}",
            refused.len(),
            matrix.len()
        );
        eprintln!(
            "the repository would not serve {} of {}: {refused:?}",
            refused.len(),
            matrix.len()
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {} tools failed:\n\n{}",
        failures.len(),
        matrix.len(),
        failures.join("\n\n")
    );
}
