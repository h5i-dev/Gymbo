<h1 align="center">jv</h1>

<p align="center">
  <a href="https://github.com/h5i-dev/jv/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/h5i-dev/jv/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/jv/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/jv?color=blue"></a>
  <a href="https://github.com/h5i-dev/jv/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/jv?style=social"></a>
</p>

A fast dependency toolkit for Maven projects, written in Rust.

```console
$ jv tree                     # the dependency tree, byte-identical to Maven's
$ jv outdated                 # what has a newer version
$ jv add org.slf4j:slf4j-api  # picks the version, or omits it if a BOM manages one
$ jvx com.google.googlejavaformat:google-java-format -- --replace Foo.java
```

## Highlights

- **Fully compatible with Maven project.** No new manifest, no lockfile,
  no POM changes.
- [8–338× faster](#speed) than the `mvn` equivalents of the commands it
  replaces.
- [Inspects dependencies](#inspecting-dependencies): `jv tree` is
  byte-identical to `mvn dependency:tree`, in all five output formats.
- [Edits your POM](#editing-a-pom): `jv add` resolves the newest release
  when you omit a version.
- [Runs any published JVM tool](#running-tools) from its coordinates with
  `jvx`, on the `uvx` model.
- [Makes offline builds work](#offline-builds): `jv sync && mvn -o verify`,
  including the lifecycle plugins that appear in no POM.
- [Profiles Maven itself](#profiling-a-build): `jv profile -- mvn test`
  shows where a build's time actually goes.

## Installation

Build from source:

```console
$ git clone https://github.com/h5i-dev/jv && cd jv
$ cargo build --release        # target/release/{jv,jvx}
```

## Features

#### Inspecting dependencies

```console
$ jv tree                     # byte-identical to mvn dependency:tree
$ jv tree --verbose           # why each version won
$ jv resolve                  # the resolved artifact list
$ jv resolve --classpath      # a ready-to-use classpath string
$ jv outdated                 # what has a newer version
```

`jv outdated` covers declared dependencies plus the `<dependencyManagement>`
entries your POM declares itself, including imported BOMs, and deliberately
skips parent-managed entries you cannot change from that POM anyway.

#### Editing a POM

```console
$ jv add com.google.guava:guava            # resolves the newest release
$ jv add com.fasterxml.jackson.core:jackson-databind   # BOM manages it: no <version> written
$ jv add org.junit.jupiter:junit-jupiter --test
$ jv remove com.google.guava:guava
```

Edits are byte-precise: jv rewrites only the span it changes, so comments,
indentation, CRLF and the XML declaration survive. One test deletes the
inserted span back out of the output and asserts byte equality with the input.

#### Running tools

`jvx` runs any published JVM tool from its coordinates, on the `uvx` model:

```console
$ jvx org.junit.platform:junit-platform-console-standalone:1.10.2 -- --help
$ jvx com.puppycrawl.tools:checkstyle:10.17.0@com.puppycrawl.tools.checkstyle.Main -- --version
```

The endpoint is `group:artifact[:version[:classifier]][@mainClass]`. Omit the
version for the latest release. When jv cannot tell which class to run, it
says so and names what it tried.

#### Offline builds

`jv sync` populates `~/.m2` with everything a build needs, including the
lifecycle plugins that appear in no POM, so Maven then builds with no network:

```console
$ jv sync --recursive && mvn -o verify
```

This is a correctness tool, not a speed one. It exists so a build cannot fail
because Central had a bad minute, and because `mvn dependency:go-offline`
produces a repository that often cannot build at all.

#### Profiling a build

```console
$ jv profile -- mvn test
```

An `EventSpy` that breaks the run into model building, dependency and plugin
resolution, and each mojo's execution. Maven ships nothing comparable.

## Speed

Every `mvn` invocation costs about a second before it does anything you asked
for: JVM start, then classworlds, the Plexus container, and plugin loading.
For a build that is noise. For asking which version won, or adding one
dependency, it is the whole cost. jv pays none of it.

commons-io, warm caches, medians of five alternated rounds, both tools
required to exit zero. 10-core WSL2, Maven 3.9.9.

| | Maven | jv | |
|---|---|---|---|
| dependency tree | `dependency:tree` 1,646 ms | `jv tree` 20 ms | **82×** |
| outdated check | `versions:display-dependency-updates` 1,637 ms | `jv outdated` 47 ms | **35×** |
| run a tool | `exec:java` 1,129 ms | `jvx` 139 ms | **8×** |
| add a dependency | `dependency:add` 1,350 ms | `jv add` 4 ms | **338×** |

One mechanism explains every row: the ratio is just how much real work the
command does on top of Maven's fixed second. A dependency tree does almost
none, so 82×. `exec:java` starts a JVM and runs a tool, so 8×. Measured
directly: turning off the versions plugin's `<dependencyManagement>` pass
removed 19 of its 29 metadata lookups and saved 16 ms out of 1,653. The
lookups cost about a millisecond each. The rest is the host.

The same mechanism sets the limit: a build's time is compilation and tests,
not the host. So jv does not claim to speed up builds, and does not try. See
the [FAQ](#will-jv-make-my-builds-faster).

## What it replaces

| You run today | With jv |
|---|---|
| `mvn dependency:tree` | `jv tree`, byte-identical, in all five output formats |
| `mvn dependency:tree -Dverbose` | `jv tree --verbose`, why each version won |
| `mvn dependency:list` | `jv resolve` |
| `mvn dependency:build-classpath` | `jv resolve --classpath` |
| `mvn versions:display-dependency-updates` | `jv outdated` |
| `mvn dependency:go-offline` | `jv sync` |
| `mvn dependency:add -Dgav=g:a:v` | `jv add g:a` |
| `mvn dependency:remove` | `jv remove g:a` |
| *(nothing)* | `jv profile -- mvn test` |

## What jv does not do

- **It does not build.** No compile, test, package. The pairing is
  `jv sync && mvn -o verify`, not a replacement for `mvn`.
- **Some projects still will not build offline.** On the default corpus tier,
  4 of 8 do. The rest hit limits `dependency:go-offline` also hits, such as
  spotless resolving a formatter whose version is a constant inside its own
  jar. `jv sync --also g:a:v` is the escape hatch.
- **Gradle projects are out of scope** for v0.x. A Gradle-built *dependency*
  resolves fine, since its POM is what Maven reads too.
- **No JDK management, and no daemon.** SDKMAN and mise do the first well. The
  second is deliberate: a process this fast makes a daemon pointless.

## Correctness

Compatibility is the product, so jv is measured against Maven rather than
against its own expectations.

- `jv tree` matches `mvn dependency:tree` byte for byte across the
  differential harness, in all five output formats and again under
  `-Dverbose`.
- `scripts/ring3.sh` diffs every module of spring-petclinic, dropwizard,
  jackson-databind, commons-lang and maven-dependency-plugin at pinned
  commits. 46 modules, 0 differing.
- `scripts/corpus.sh` syncs real projects and builds them offline, running
  `dependency:go-offline` as a control to attribute any failure, and
  `-b OLD_JV` to catch a regression the control would excuse.
- Version ordering agrees with maven-resolver's own `GenericVersion`, driven
  as an oracle across 50,862 generated inputs.
- Effective POMs match `mvn help:effective-pom` from 3.9.9 exactly.

## FAQ

#### Will jv make my builds faster?

No. `mvn verify` on commons-io is 53.8 s; `jv sync && mvn -o verify` is
58.7 s. That is 0.92×, because a build's time is compilation and tests, which
jv does not do. Adopt `jv sync` for offline correctness. What jv makes fast is
the *other* commands: the second you pay to ask a question, dozens of times a
day.

#### Doesn't `mvn dependency:add` already do this?

Since maven-dependency-plugin 3.11, yes. And it is good. Verified by running
it, not by reading a changelog: the plugin preserves formatting and comments
exactly as jv does, and omits `<version>` when a BOM already manages the
artifact. Neither of those is a reason to prefer jv.

Two differences survive that test. The plugin has no repository-metadata
lookup, so `mvn dependency:add -Dgav=com.google.guava:guava` fails with *"No
version specified and no managed version found"*: you must already know the
version. `jv add com.google.guava:guava` resolves the newest release. And the
plugin pays Maven's fixed second to edit one line, which is where the 338×
comes from.

#### Why not Coursier?

Coursier is excellent, and jv uses it as one of its correctness oracles. But
`cs` runs on the JVM and centers on the Scala workflow. jv is a single native
binary that speaks Maven's own vocabulary (`pom.xml`, `settings.xml`,
`~/.m2`, Maven's flags) for people whose project is a Maven project.

#### Which Maven does jv follow?

Maven 3.9. `docs/spec/` records where 3.9 and Maven 4 diverge and says which
jv follows in each case.

#### How do you pronounce jv?

"jay-vee". Just "jv", lowercase, please.

## More

[`ROADMAP.md`](ROADMAP.md) holds the architecture, the milestones, and the
measurements that closed several directions off.
[`docs/development.md`](docs/development.md) explains how to run the tests.

## Acknowledgements

jv's compatibility work leans on the [Apache Maven](https://maven.apache.org/)
project itself: its resolver's `GenericVersion` is jv's version-ordering
oracle, and real `mvn` runs anchor every differential test.
The shape of the tool owes an obvious debt to
[uv](https://github.com/astral-sh/uv) and [Coursier](https://get-coursier.io/).

## License

Apache-2.0. See [LICENSE](LICENSE).
