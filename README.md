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
  `jvx`.
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

`jvx` runs any published JVM tool from its coordinates:

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

| | Maven | jv | |
|---|---|---|---|
| dependency tree | `dependency:tree` 1,646 ms | `jv tree` 20 ms | **82×** |
| outdated check | `versions:display-dependency-updates` 1,637 ms | `jv outdated` 47 ms | **35×** |
| run a tool | `exec:java` 1,129 ms | `jvx` 139 ms | **8×** |
| add a dependency | `dependency:add` 1,350 ms | `jv add` 4 ms | **338×** |

The same mechanism sets the limit: a build's time is compilation and tests,
not the host. So jv does not claim to speed up builds, and does not try.

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
  `jv sync && mvn -o verify`.
- **Gradle projects are out of scope** for v0.x. A Gradle-built *dependency*
  resolves fine, since its POM is what Maven reads too.
- **No JDK management, and no daemon.** SDKMAN and mise do the first well. The
  second is deliberate: a process this fast makes a daemon pointless.

## FAQ

- *Will jv make my builds faster?* -- No. `mvn verify` on commons-io is 53.8 s; `jv sync && mvn -o verify` is
58.7 s. That is 0.92×, because a build's time is compilation and tests, which
jv does not do.
- *Why not Coursier?* -- Coursier is excellent, and jv uses it as one of its correctness oracles. But
`cs` runs on the JVM and centers on the Scala workflow. jv is a single native
binary that speaks Maven's own vocabulary (`pom.xml`, `settings.xml`,
`~/.m2`, Maven's flags) for people whose project is a Maven project.
- *Which Maven does jv follow?* -- Maven 3.9.
- *How do you pronounce jv?* -- "jay-vee". Just "jv", lowercase, please.

## Acknowledgements

jv's compatibility work leans on the [Apache Maven](https://maven.apache.org/)
project itself: its resolver's `GenericVersion` is jv's version-ordering
oracle, and real `mvn` runs anchor every differential test.

## License

Apache-2.0. See [LICENSE](LICENSE).
