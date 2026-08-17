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
mkdir -p _reference && cd _reference
git clone --depth 1 https://github.com/apache/maven-resolver
git clone --depth 1 https://github.com/apache/maven
git clone --depth 1 https://github.com/apache/maven-dependency-plugin
git clone --depth 1 https://github.com/apache/maven-dependency-tree
```

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
2. Port the *behavior*, not the code — upstream is Apache-2.0 and jv is
   MIT/Apache-2.0 dual, so verbatim translation is avoided by policy, not just
   by licence arithmetic.
3. Bring the upstream test cases across as a corpus data file, noting the source
   test for each directive.
4. Where the upstream implementation is cheap to isolate, add an oracle test.
   Being able to diff against the real thing is worth more than any number of
   hand-written assertions.
