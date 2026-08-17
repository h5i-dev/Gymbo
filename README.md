# jv

**An extremely fast JVM package and toolchain manager, written in Rust.**

`mvn dependency:tree` spends seconds starting a JVM and initializing a resolver
before it answers. jv gives the same answer — Maven's, exactly — from a single
binary, in milliseconds.

> **Status: early development.** The crates marked below are implemented and
> verified. There is no CLI yet, so there is nothing to install.

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
| `jv-resolver` | Dependency collection, nearest-wins conflict resolution | |
| `jv-repo`, `jv-cache` | Repositories, downloads, integrity, caching | |
| `jv tree`, `jv sync`, `jvx` | The commands | |

`ROADMAP.md` holds the architecture and the milestones.

## Correctness

Compatibility *is* the product, so jv is measured against Maven rather than
against its own expectations:

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
