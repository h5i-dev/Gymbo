<h1 align="center">jv</h1>

<p align="center"><strong>Maven's dependency resolution, in a single Rust binary.</strong></p>

<p align="center">
  <a href="https://github.com/h5i-dev/jv/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/h5i-dev/jv/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/jv/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/jv?color=blue"></a>
  <a href="https://github.com/h5i-dev/jv/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/jv?style=social"></a>
</p>

**jv** answers the dependency questions Maven answers (which versions win, what
the classpath is, what to download) without starting a JVM. It reads your
`pom.xml` and your `~/.m2/settings.xml`, resolves with Maven 3.9's exact rules,
and shares Maven's local repository.

<table align="center">
<tr>
<td>⚡ Milliseconds, not seconds</td>
<td>🎯 Byte-identical to <code>mvn dependency:tree</code></td>
</tr>
<tr>
<td>🤝 Shares <code>~/.m2</code>, reads your <code>settings.xml</code></td>
<td>📦 One static binary, no JVM</td>
</tr>
</table>

```bash
jv tree                       # the dependency tree, byte-identical to Maven's
jv resolve --classpath        # a classpath you can paste into java -cp
jv sync && mvn -o verify      # jv downloads, Maven builds, no network
jvx com.google.googlejavaformat:google-java-format -- --version
```

**A jv project is just your Maven project.** No new manifest, no lockfile, no
migration, no changes to your POM. jv reads what Maven already reads, and writes
where Maven already looks — so nothing it does is visible to a teammate who
keeps using `mvn`.

> **Status: early development.** `jv tree`, `jv resolve`, `jv sync` and `jvx`
> all work and are verified against real Maven on every commit. No release is
> tagged yet, so for now you build it yourself.

---

## Speed

|  | jv | `mvn dependency:tree` | |
|---|---|---|---|
| Warm | **26 ms** | 1,778 ms | 69× |
| Cold | **0.75 s** | 11.3 s | 15× |

Median of five warm runs on one project (Jackson, HttpClient 5, Guava, JUnit 5;
23 nodes), 10-core aarch64 Linux, Maven 3.9.9. Both tools start from an empty
cache for the cold row. `scripts/benchmark.sh` produces this table, and refuses
to report a time unless the two tools' output matches first: a benchmark
against wrong output measures nothing. Cold timings are network-bound and will
differ on your machine; the warm row is the one that reflects the tool.

Both figures come from the same idea: jv crawls POMs ahead of the resolver
instead of fetching them one round trip at a time, and the crawler hands over
what it parsed rather than parsing everything twice. A 104-node Spring Boot
project went from 22.4 s to 3.5 s cold and 150 ms to 59 ms warm that way. The
warm figure also stopped including a JVM boot, which jv used to pay on every
run to read one line out of it.

---

## Install

No release is tagged yet, so build from source:

```bash
git clone https://github.com/h5i-dev/jv && cd jv
cargo build --release       # binaries land in target/release/{jv,jvx}
```

Once v0.1.0 ships, this is the install:

```bash
curl -LsSf https://raw.githubusercontent.com/h5i-dev/jv/main/scripts/install.sh | sh
```

The script picks the build for your platform, verifies it against the SHA-256
published beside the release, and installs to `~/.local/bin`. Set
`JV_INSTALL_DIR` to install elsewhere, or `JV_VERSION=vX.Y.Z` to pin a version.
Until then it has nothing to fetch and will tell you so rather than guess.

jv needs a JDK on `PATH` or at `JAVA_HOME` only to run tools with `jvx`, and to
decide which `<jdk>` profile activators match. Resolution itself needs no JVM.
v0.x targets Linux and macOS; on Windows, use WSL2.

---

## Adopting it

You do not have to adopt jv all at once, and you never have to give up Maven.
Three steps, in increasing order of commitment. Each is useful on its own.

### 1. Replace the commands you use to *look* at dependencies

These only read. They cannot change what your build produces, so there is
nothing to weigh up.

| What you run today | With jv | |
|---|---|---|
| `mvn dependency:tree` | `jv tree` | byte-identical, in all five output formats |
| `mvn dependency:tree -DoutputType=dot` | `jv tree -t dot` | also `json`, `tgf`, `graphml` |
| `mvn dependency:tree -Dverbose` | `jv tree --verbose` | why each version won, and which lost |
| `mvn dependency:list` | `jv resolve` | the resolved set, one line each |
| `mvn dependency:build-classpath` | `jv resolve --classpath` | |
| `mvn dependency:go-offline` | `jv sync` | see step 2 |
| *(any of the above)*, whole reactor | add `--recursive` | every module of a multi-module build |

Familiar flags carry over with the spelling Maven gives them: `-o` for offline,
`-U` to force updates, `-s` for a settings file, `-P` for profiles, `-D` for
properties, `-f` for a POM somewhere else.

```console
$ jv tree --verbose
com.example:demo:jar:1.0
+- com.fasterxml.jackson.core:jackson-databind:jar:2.17.1:compile
|  +- com.fasterxml.jackson.core:jackson-annotations:jar:2.17.1:compile
|  \- (com.fasterxml.jackson.core:jackson-core:jar:2.17.1:compile - omitted for conflict with 2.15.0)
\- com.fasterxml.jackson.core:jackson-core:jar:2.15.0:compile
```

### 2. Take the download out of your build

`jv sync` populates Maven's local repository with everything the build needs:
dependencies, plugins, and the plugins the lifecycle binds for your packaging,
which appear in no POM anywhere. Maven then builds with no network at all:

```bash
jv sync --recursive     # or just `jv sync` for a single module
mvn -o verify           # compiles, tests, packages, with no network
```

This is the change to make in CI. Dependency download is the slow,
network-bound part of a Maven build; `mvn -o` afterwards does no network
I/O, so the build cannot fail because Central had a bad minute. Your POM, your
plugins, and your build output are untouched. Maven still does all the
building.

`jv sync` writes into `~/.m2/repository`, the same place `mvn` does. Use
`--local-repository DIR` to point somewhere else, or `--cache-only` to fill
jv's own cache and leave Maven's repository alone.

### 3. Run tools without installing them

`jvx` runs any published JVM tool straight from its coordinates, on the `uvx`
model. No install step, no wrapper script, no `<plugin>` block added to a POM
just to run something once.

```bash
jvx com.google.googlejavaformat:google-java-format -- --replace Foo.java
jvx org.junit.platform:junit-platform-console-standalone:1.10.2 -- --help
jvx org.jacoco:org.jacoco.cli:0.8.12:nodeps -- report --help
```

The endpoint is `group:artifact[:version[:classifier]][@mainClass]`. Omit the
version and jv resolves the latest release; everything after `--` goes to the
tool untouched. The two optional fields reach tools whose jar does not
advertise itself. Use a `:classifier` when the runnable artifact sits beside the
default one, and `@mainClass` when the manifest names no `Main-Class`:

```bash
jvx com.puppycrawl.tools:checkstyle:10.17.0@com.puppycrawl.tools.checkstyle.Main -- --version
```

When jv cannot tell which class to run, it says so and names what it tried,
rather than guessing.

### In GitHub Actions

The two steps above, as one action:

```yaml
- uses: h5i-dev/jv@v1
  with:
    sync: true
- run: mvn -o verify
```

It installs jv, verifies the archive's checksum, caches jv's store keyed by your
POMs, and runs `jv sync --recursive`. Keying on the POMs means a dependency
change invalidates the cache and nothing else does. See
[`action.yml`](action.yml). The `v1` tag appears with the first release; until
then the action can only be used from a branch ref.

---

## What jv does not do

- **It does not build anything.** No compile, test, package, or deploy. jv is
  not a build tool: it resolves and downloads, then gets out of Maven's way.
  The pairing is `jv sync && mvn -o verify`, not a replacement for `mvn`.
- **It follows Maven 3.9, not Maven 4.** The two differ in ways that change
  resolved versions. `docs/spec/` records both, and says which one jv follows
  wherever they diverge.
- **Gradle projects are out of scope** for v0.x: no `build.gradle`, no Gradle
  Module Metadata. A Gradle-built *dependency* resolves fine; its POM is what
  Maven reads too.
- **`<proxies>` in `settings.xml` is parsed but not yet applied.** Set
  `HTTPS_PROXY`/`NO_PROXY` in the environment, which jv does honour. Mirrors,
  `<servers>` credentials, and profiles work.
- **No JDK management, and no daemon.** SDKMAN, mise and jenv already do the
  first well, and jv has no angle they lack. The second is deliberate: a
  single-shot process fast enough to make a daemon pointless.

---

## Correctness

Compatibility *is* the product, so jv is measured against Maven rather than
against its own expectations:

- `jv tree` matches `mvn dependency:tree` byte for byte on every fixture in the
  differential harness, each chosen for a resolution behaviour (nearest-wins,
  managed transitives, BOM imports, exclusions, the scope matrix, optional
  dependencies, conflict ordering) in all five output formats, and again under
  `-Dverbose`, which keeps the losers and annotates the survivors.
- And on real projects nobody wrote for jv's benefit: `scripts/ring3.sh` diffs
  every module of spring-petclinic, dropwizard, jackson-databind, commons-lang
  and maven-dependency-plugin at pinned commits. 46 modules, 0 differing.
- `jv sync && mvn -o verify` builds offline, tests and all, against a repository
  jv populated and nothing else.
- `jvx` launches eleven real published tools, and refuses the nine libraries
  among them with a message naming why.
- Version ordering agrees with maven-resolver's own `GenericVersion`, compiled
  from source and driven as an oracle, across 50,862 generated inputs.
- Effective POMs match `mvn help:effective-pom` from Maven 3.9.9 exactly.
- The POM parser reads every POM in the Maven, maven-resolver and
  maven-dependency-plugin repositories.

Where jv has no Maven command to compare against (`jv resolve`'s line format,
`jvx`'s endpoint syntax), it is tested against transcribed upstream corpora
instead, and the tests say which of the two they are.

---

## Progress

| Crate | | |
|---|---|---|
| `jv-version` | Version ordering, ranges, constraints | ✅ |
| `jv-model` | POM, `settings.xml`, `maven-metadata.xml` | ✅ |
| `jv-model-builder` | Effective POMs: inheritance, profiles, interpolation, BOM imports | ✅ |
| `jv-resolver` | Dependency collection, nearest-wins conflict resolution | ✅ |
| `jv-repo`, `jv-cache` | Repositories, downloads, integrity, caching | ✅ |
| `jv-tree`, `jv-driver`, `jv-cli` | `jv tree`, `jv resolve` | ✅ |
| `jv-driver` sync | `jv sync` — populate `~/.m2` so `mvn -o` works | ✅ |
| `jv-exec` | `jvx` — run a tool from its coordinates | ✅ |

Next: private-repository and `.mvn/` compatibility, then `jv add` and a
lockfile. [`ROADMAP.md`](ROADMAP.md) holds the architecture and the milestones;
[`docs/development.md`](docs/development.md) explains how to run the tests.

---

## License

Apache-2.0. See [LICENSE](LICENSE).
