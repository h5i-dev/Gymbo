# Developing jv

## Prerequisites

- A recent stable Rust toolchain (`rust-toolchain.toml` pins the channel).
- A **JDK** (not just a JRE) for the differential tests: they compile parts of
  Maven Resolver with `javac` and, from M5 onward, run real Maven. Temurin 21 is
  what CI uses.
- Shallow clones of the upstream projects jv mirrors, under `_reference/`.

## Setting up `_reference/`

`_reference/` is git-ignored. It holds the upstream sources that define
compatible behavior, and the differential tests compile out of it directly, so
nothing upstream is vendored into this repository.

```sh
./scripts/fetch-reference.sh
```

That script is the single definition of which clones the tests need; CI runs the
same one. Adding a clone requirement means editing it, not editing two places.

`_reference/` may also be a symlink to clones kept elsewhere.

See `ROADMAP.md` §4 for which behavior is defined where, and §8 for the full
clone inventory.

## Running the tests

```sh
cargo test --workspace
```

Tests come in three layers, and it is worth knowing which one caught a failure:

1. **Unit tests** — behavior stated in prose in each module's docs.
2. **Corpus tests** (`crates/*/tests/corpus.rs`) — transcriptions of upstream's
   own test assertions, kept as data files under `tests/corpus/`. Each file
   records which upstream test each directive came from.
3. **Oracle tests** (`crates/*/tests/oracle.rs`) — jv compared against the
   upstream *implementation* over generated inputs. These cover the shape of the
   input space rather than the cases someone thought to write down.

Oracle tests **skip themselves** when a JDK or `_reference/` is missing, so a
fresh clone still runs green. To require them instead — which CI does, and which
you want before trusting a green run:

```sh
JV_REQUIRE_ORACLE=1 cargo test --workspace
```

Override source discovery with `JV_MAVEN_RESOLVER_SRC=/path/to/maven-resolver`.

## The Maven oracle

Four tests run real Maven and demand agreement. They are the ones that decide
whether jv is actually compatible, so they are worth knowing individually:

| Test | What it asks Maven |
|---|---|
| `jv-model-builder/tests/mvn_oracle.rs` | `mvn help:effective-pom` on 10 projects, plugins included |
| `jv-cli/tests/mvn_tree_oracle.rs` | `mvn dependency:tree` in all five output types, on 8 projects — 40 pairs |
| `jv-driver/tests/sync_offline_maven.rs` | whether `mvn -o verify` succeeds against a repository `jv sync` populated |
| `jv-cli/tests/jvx_launch.rs` | whether `jvx` actually launches a real tool from Maven Central |

Put a Maven 3.9 on `PATH`, or point `JV_MVN` at one:

```sh
curl -sSLo maven.tar.gz https://archive.apache.org/dist/maven/maven-3/3.9.9/binaries/apache-maven-3.9.9-bin.tar.gz
curl -sSLo maven.tar.gz.sha512 https://archive.apache.org/dist/maven/maven-3/3.9.9/binaries/apache-maven-3.9.9-bin.tar.gz.sha512
echo "$(cat maven.tar.gz.sha512)  maven.tar.gz" | sha512sum -c -
tar xzf maven.tar.gz
export JV_MVN="$PWD/apache-maven-3.9.9/bin/mvn"
```

Pin the version rather than taking whatever is installed: the point of the test
is that jv agrees with a *known* Maven, and a floating oracle turns a
compatibility regression into a mystery. Maven 3.9 specifically — jv targets its
behavior, and Maven 4 differs in ways that would show up as failures here.

The first run downloads the help plugin and the fixtures' BOMs into `~/.m2`;
after that it needs no network. `JV_LOCAL_REPO` overrides where jv looks for
them. The tree, sync and jvx oracles each build their own throwaway repository
and cache, so they take a minute or two apiece on a cold machine and need
network access — which is deliberate. A test that let either tool reuse a warm
repository the other filled would be measuring the wrong thing.

A note on reading Maven's sources: **run the version you are targeting, do not
trust the source you cloned**. Two byte-parity bugs got through review because
the upstream code says one thing and the shipped plugin does another — the dot
renderer's `endVisit` appends a line separator that never reaches the file, and
`maven-dependency-plugin` 3.6.1 silently renders text for an output type it does
not recognise. Both were caught only by diffing against a real run.

## Known upstream divergences

Maven has two version implementations that do not agree with each other:
`maven-resolver`'s `GenericVersion` (used at runtime) and `maven-artifact`'s
legacy `ComparableVersion`. jv follows Maven Resolver.

The corpus files isolate the disagreements in sections marked
`maven-artifact-only`, `CONTRADICT`, `EXTENSION` or `UNEXPRESSIBLE`, and
individual conflicting directives carry a `# DISAGREEMENT?:` comment. The
harness skips these and asserts that it skipped *something*, so silently
dropping a marker fails the build rather than quietly asserting semantics jv
deliberately rejects.

The concrete disagreements found so far, both from `ComparableVersion` treating
`-` as a sub-list separator where `GenericVersion` treats it as just another
delimiter:

- `2.0-1 < 2.0.1` under `ComparableVersion`; equal under `GenericVersion`.
- `ComparableVersionTest`'s `VERSIONS_NUMBER` chain orders `2-1` between `2.0.a`
  and `2.0.2`, which `GenericVersion` reads differently for the same reason.

## Adding a compatibility claim

When implementing behavior that has to match Maven:

1. Find the authoritative upstream source (ROADMAP.md §4 is the index) and cite
   its path in a module-level doc comment.
2. Port the *behavior*, not the code. Both jv and upstream are Apache-2.0, so
   this is a quality rule rather than a licensing one: reasoning from the
   behavior is what surfaces the quirks worth reproducing.
3. Bring the upstream test cases across as a corpus data file, noting the source
   test for each directive.
4. Where the upstream implementation is cheap to isolate, add an oracle test.
   Being able to diff against the real thing is worth more than any number of
   hand-written assertions.
