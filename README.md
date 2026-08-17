# jv

**An extremely fast JVM package and toolchain manager, written in Rust.**

`mvn dependency:tree` spends seconds starting a JVM and initializing a resolver
before it answers. jv gives the same answer — Maven's, exactly — from a single
binary, in milliseconds.

```console
$ jv tree
com.example:demo:jar:1.0
+- com.fasterxml.jackson.core:jackson-databind:jar:2.17.1:compile
|  +- com.fasterxml.jackson.core:jackson-annotations:jar:2.17.1:compile
|  \- com.fasterxml.jackson.core:jackson-core:jar:2.17.1:compile
\- org.junit.jupiter:junit-jupiter:jar:5.10.2:test
   ...
```

> **Status: early development.** `jv tree`, `jv resolve`, `jv sync` and `jvx`
> work. There are no prebuilt binaries yet; build with `cargo build --release`
> and the binaries land in `target/release/`.

## Speed

|  | jv | `mvn dependency:tree` | |
|---|---|---|---|
| Warm | **53 ms** | 1,532 ms | 29× |
| Cold | **4.5 s** | 15.7 s | 3.5× |

Median of five warm runs on one project (Jackson, HttpClient 5, Guava, JUnit 5;
23 nodes), 10-core aarch64 Linux, Maven 3.9.9. Both tools start from an empty
cache for the cold row. `scripts/benchmark.sh` produces this table, and refuses
to report a time unless the two tools' output matches first — a benchmark
against wrong output measures nothing. Cold timings are network-bound and will
differ on your machine; the warm row is the one that reflects the tool.

## Commands

```console
$ jv tree                     # the dependency tree, byte-identical to Maven's
$ jv resolve --classpath      # a classpath you can paste into java -cp
$ jv sync && mvn -o verify    # jv downloads, Maven builds, no network
$ jvx com.google.googlejavaformat:google-java-format -- --version
```

## Why

Maven-exact resolution, `~/.m2` interoperability, and a fast single binary.
Existing tools have at most two: `mvn` and Coursier carry JVM startup, and the
Rust alternatives pick versions by Gradle's latest-wins rule, so they cannot
reproduce `mvn dependency:tree`.

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

`ROADMAP.md` holds the architecture and the milestones.

## Correctness

Compatibility *is* the product, so jv is measured against Maven rather than
against its own expectations:

- `jv tree` matches `mvn dependency:tree` byte for byte on every fixture in the
  differential harness, each chosen for a resolution behaviour — nearest-wins,
  managed transitives, BOM imports, exclusions, the scope matrix, optional
  dependencies, conflict ordering.
- Version ordering agrees with maven-resolver's own `GenericVersion` — compiled
  from source and driven as an oracle — across 50,862 generated inputs.
- Effective POMs match `mvn help:effective-pom` from Maven 3.9.9 exactly.
- The POM parser reads every POM in the Maven, maven-resolver and
  maven-dependency-plugin repositories.

jv follows **Maven 3.9**, which differs from Maven 4 in ways that change
resolved versions; `docs/spec/` records both. `docs/development.md` explains how
to run the tests.

## License

Apache-2.0.
