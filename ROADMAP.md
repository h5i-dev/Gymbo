# jv — Roadmap & Architecture

> **jv** — an extremely fast JVM package and toolchain manager, written in Rust.
> "uv for the JVM."

This document is the engineering roadmap for v0.1 → v0.3. It is grounded in a
survey of the reference clones under `_reference/` (apache/maven `4.1.0-SNAPSHOT`,
apache/maven-resolver `2.0.22-SNAPSHOT`, maven-dependency-plugin `3.11.1-SNAPSHOT`,
coursier, ymbuild/yummy, linux-china/wukong, apposed/jgo). Exact source paths are
cited throughout so an implementing agent can jump straight to the authoritative
behavior instead of rediscovering it.

---

## 1. Vision & positioning

**One sentence:** a single static binary that resolves, downloads, and runs Maven
dependencies with Maven-identical semantics, 10–100× faster than `mvn` for every
operation dominated by JVM startup and resolver initialization.

**The honest 100× lives in warm-cache, metadata-bound operations** — not cold
downloads (network-bound, 2–5× at best). The launch demo is therefore
`jv tree` vs `mvn dependency:tree` (~10s on a mid-size Spring project → <100ms),
not a download race.

**v0.1 surface (in priority order):**

| Command | Role |
|---|---|
| `jv tree` | The README gif. Byte-compatible with `mvn dependency:tree`. |
| `jv sync` | `dependency:go-offline` equivalent → enables `mvn -o` builds; the CI adoption channel (ships with a `setup-jv` GitHub Action). |
| `jvx` / `jv x` | Run any JVM tool from Maven coordinates without installing it (uvx analogue; the agent-hot-path feature). |
| `jv resolve` | Print a resolved classpath (script/agent building block, powers `jvx`). |

**Deliberate non-goals for v0.1** (each is a focus hazard):
- JDK management (mise/sdkman territory, weak differentiation) → v0.3.
- Gradle projects / Gradle Module Metadata → v0.3+. Initial target is Maven
  users: Spring / enterprise Java. Demo repos must be Maven-built
  (spring-petclinic, netty, dropwizard, camel — **not** Spring Boot itself,
  Kafka, or Elasticsearch, which are Gradle builds).
- Building/compiling anything. jv is not a build tool in v0.x; it feeds `mvn -o`
  and runs tools. (yummy already occupies "Rust build tool" and it is not the
  winnable hill; resolution compatibility is.)
- A daemon. Single-shot process must be fast enough that a daemon is pointless —
  that *is* the pitch.
- Windows support. v0.x targets Linux and macOS only — the initial audience
  (CI runners, Show HN readers, Spring/backend devs on dev containers) is
  overwhelmingly Linux/macOS, and rv launched the same way. Windows lands
  post-v0.3; until then, avoid gratuitous portability blockers (no Unix-only
  path assumptions baked into `jv-cache`/`jv-install` core types).

**Positioning rule:** marketed against `mvn` (and mvnd), never as "Coursier in
Rust". Coursier appears only in the benchmark table and as a test oracle. FAQ
preempts the inevitable HN comment: "cs is excellent but carries JVM startup;
jv is a single binary built for Java developers."

---

## 2. Competitive landscape (from the reference survey)

| Project | What it is | Why jv is different |
|---|---|---|
| `mvn` / mvnd | The incumbent. | JVM startup + resolver init on every invocation; mvnd needs a daemon. |
| coursier | Full-featured Scala resolver/launcher. | JVM startup; **latest-wins** reconciliation (documented in `coursier/doc/docs/other-version-handling.md`), so it cannot reproduce `mvn dependency:tree`; invisible to non-Scala devs. |
| yummy (`ym`) | Rust build tool with a real Maven resolver (`src/workspace/resolver.rs`, 4.7 kLOC: parent POMs, BOM import, transitive BFS). | Deliberately **Gradle latest-wins** (their ADR-016); no profile activation; no version ranges; invents its own cache layout incompatible with `~/.m2`; resolution and I/O entangled in one file. jv's wedge: exact Maven semantics + `.m2` interop. |
| wukong | Rust CLI wrappers (jbang/sdkman/jenv clones). | Zero resolution logic in Rust — `gav` shells out to `mvn`, `jbang` runs the real jbang jar. Reusable ideas only: foojay JDK provisioning, size-tuned release profile, cargo-dist setup. |
| jgo | Python launcher from Maven coords. | Right UX model (endpoint syntax, env hashing, `.m2`-compatible cache, hardlinks) but Python-slow and resolver optionally shells to `mvn`. jv ports its *design*, not its speed. |

**The differentiation triangle jv must own:** (1) Maven-exact nearest-wins
semantics, (2) `~/.m2/repository` interoperability, (3) single-binary sub-100ms
startup. No existing tool has all three; each competitor is missing at least two.

---

## 3. Architecture

### 3.1 Design principles

1. **Pure resolver core, effectful shell.** Copy coursier's `ResolutionProcess`
   split (`modules/core/.../ResolutionProcess.scala`): the resolver is a
   deterministic state machine that emits "I need these POMs/metadata" and
   consumes answers; all HTTP/disk lives in a driver layer. This is what makes
   fixture-based differential testing trivial, keeps the core `Send`-free and
   simple, and lets the driver batch/parallelize freely. yummy's monolithic
   `resolve_inner` is the anti-pattern.
2. **Oracle-driven correctness.** Every semantic component is validated against
   ported upstream test corpora (§5) plus live differential tests against `mvn`.
   Compatibility bugs are the existential risk; "fast but wrong tree" kills the
   launch on HN day one.
3. **Maven 3.9 behavior is the compatibility target.** That is what users run
   and diff against. Concretely: classic conflict resolution semantics,
   depth-1 dependencyManagement (`ClassicDependencyManager`), the Maven 3 scope
   table, Maven 3 profile activators (skip Maven 4's `<condition>` language).
   Maven 4 divergences are tracked but not chased in v0.x.
4. **Own cache, `.m2` interop.** jv's source of truth is its own
   content-addressed store; `~/.m2/repository` is a hardlink materialization
   target (for `mvn -o`) and an opportunistic read source. Never corrupt or
   depend exclusively on `.m2`.

### 3.2 Workspace layout (Cargo crates)

```
crates/
  jv-version        Version ordering, ranges, constraints (GenericVersion port)
  jv-model          POM data model + streaming parser; settings.xml, maven-metadata.xml models
  jv-model-builder  Effective POM pipeline (inheritance, interpolation, profiles, BOM import)
  jv-resolver       Pure dependency collection + graph transformation (nearest-wins)
  jv-repo           Repository abstraction: layout (GAV→URL), metadata, snapshots, mirrors, auth
  jv-cache          CAS store, HTTP download driver, checksums, locks, negative cache, TTL
  jv-install        Materialization: hardlink into ~/.m2 (+ _remote.repositories), env dirs
  jv-lock           Lockfile read/write/verify (v0.2)
  jv-exec           jvx: env construction, main-class detection, java process launch
  jv-tree           Tree/list renderers (text byte-compatible with mvn, json, dot, tgf, graphml)
  jv-cli            clap-based CLI binary (`jv`, plus `jvx` as a hardlinked alias)
  jv-testkit        Ported corpus parsers: DependencyGraphParser DSL, .ini artifact
                    descriptors, coursier resolution fixtures, mvn-output differ
```

Baseline crates (validated by the prior-art survey): `tokio` + `reqwest`
(rustls, HTTP/2 — multiplexing many small metadata GETs against Central is a
real win; yummy/wukong use blocking reqwest and it shows in their fetch
batching), `quick-xml` for streaming POM parsing (coursier's SAX
`PomParser.scala` proves DOM is unnecessary; POMs are numerous and small),
`clap`, `sha1`/`sha2`, `zip`, `fs4` file locks, `dirs`, `rayon` for CPU-side
work. The release profile deliberately departs from wukong's size-tuned one:
jv's product claim is latency, so it optimizes for speed (`opt-level=3`,
`lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip`) rather than for
`opt-level="z"`. Distribution via `cargo-dist` (+ `cargo binstall`, Homebrew
tap, `curl | sh`).

### 3.3 Effective POM pipeline (`jv-model-builder`)

Authoritative reference: `maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelBuilder.java`
(2786 LOC; phases `readFileModel → readRawModel → readEffectiveModel → buildEffectiveModel`).

Pipeline stages, each with its upstream truth source:

| Stage | Port from |
|---|---|
| Raw POM parse | streaming parser modeled on coursier `PomParser.scala` (tag-dispatch, no DOM) |
| Parent resolution (repo + `relativePath`, cycle detection to bounded depth) | `DefaultModelBuilder.readParent/readParentLocally`; cycle tests in `impl/maven-impl/.../model/ParentCycleDetectionTest.java` |
| Inheritance assembly | `DefaultInheritanceAssembler.java` (346 LOC, incl. child-path-adjusted URL rules) + **`MavenModelMerger.java` (654 LOC) — the per-field precedence table; port this table verbatim** |
| Interpolation (`${...}`, cycle-safe, `project.*`/properties/env/`basedir`) | `DefaultModelInterpolator.java` + `DefaultInterpolator.java` |
| Profile activation (property, JDK, OS, file, packaging) | `impl/maven-impl/.../model/profile/*` activators; **skip Maven-4-only `ConditionProfileActivator`** |
| Profile injection | `DefaultProfileInjector.java` |
| BOM import (`<scope>import</scope>`, recursive flattening) | `DefaultDependencyManagementImporter.java` |
| depMgmt injection onto dependencies (version/scope/exclusions/optional) | `DefaultDependencyManagementInjector.java` |
| Normalization (dependency dedup) | `DefaultModelNormalizer.java` |

The POM→resolver bridge (effective model → dependency list + managed deps +
**relocation** handling) is `impl/maven-impl/.../resolver/DefaultArtifactDescriptorReader.java`;
relocations must be supported in v0.1 (Central relies on them, e.g. old groupIds).

### 3.4 Resolution core (`jv-resolver`)

Two phases, both pure, mirroring maven-resolver:

**Collection** — breadth-first with the skip optimization
(`maven-resolver-impl/.../collect/bf/BfDependencyCollector.java` +
`DependencyResolutionSkipper.java`): BF ordering is what makes nearest-wins
cheap, since a node at depth N can be skipped if its GA already resolved at
depth < N. Collection-time behaviors to port:

- Exclusion propagation: `util/graph/selector/ExclusionDependencySelector.java`
  (+ scope/optional/static selectors composed via `AndDependencySelector`).
- dependencyManagement application with **Maven-classic depth-1 semantics**:
  `util/graph/manager/ClassicDependencyManager.java` (a node's own depMgmt does
  not apply to itself, only from depth 2 — a notorious compat trap; yummy gets
  parts of this wrong per its own ADR log).
- Version ranges: resolve candidates from `maven-metadata.xml`
  (`DefaultVersionRangeResolver.java`), then `HighestVersionFilter` /
  `SnapshotVersionFilter` from `util/graph/version/`.
- Optional deps: excluded transitively, kept when direct.
- Cycle handling: `DefaultDependencyCycle.java`; corpus includes
  `cycle-big/`, `versionless-cycle/` cases.

**Graph transformation** — the canonical chain order is wired in
`MavenSessionBuilderSupplier.java` (read it before deviating):

1. `ConflictMarker` — group nodes into conflict ids (GA + classifier/extension).
2. `ConflictIdSorter` — topo-order conflict ids (deterministic cyclic conflicts).
3. Conflict resolution — port the **semantics of `ClassicConflictResolver`**
   (current default) with `NearestVersionSelector` (nearest-wins),
   `JavaScopeSelector` (winner scope), `JavaScopeDeriver` (parent×child scope
   table), `SimpleOptionalitySelector`. Keep `PathConflictResolver` (the O(N)
   rewrite, 1045 LOC) in view as the performance blueprint — same semantics,
   linear time.
4. `JavaDependencyContextRefiner`.

Scope derivation truth table: `maven/impl/maven-impl/.../resolver/scopes/Maven3ScopeManagerConfiguration.java`
(jv targets Maven 3 behavior; the Maven 4 table sits alongside for later).

Verbose mode (`jv tree --verbose`) requires retaining loser nodes and
premanaged state: `ConflictResolver.Verbosity` + `PremanagedDependency.java`.
Collect this from day one (cheap) even if rendering ships in v0.2.

### 3.5 Version ordering (`jv-version`)

**Port `GenericVersion`, not `ComparableVersion`.** Maven 4's runtime path is
`maven-resolver-util/.../version/GenericVersion.java` (via the thin
`MavenVersionScheme` delegate); `ComparableVersion` survives only in the
deprecated `compat/maven-artifact`. Port `GenericVersion` +
`GenericVersionScheme/Range/Constraint` + `UnionVersionRange`, and validate
against both corpora:
- `maven-resolver-util/src/test/java/.../version/` — 7 classes, ~1500 LOC
  (`GenericVersionTest.java` alone: 635 LOC).
- `maven/compat/maven-artifact/src/test/java/.../versioning/ComparableVersionTest.java`
  — 488 LOC of ordered-array corpora.

**The two implementations genuinely disagree**, so "pass both corpora" is not a
reachable goal and must not be written as one. `ComparableVersion` treats `-` as
a sub-list separator; `GenericVersion` treats `-`, `.` and `_` as equivalent
delimiters. Hence `2.0-1 < 2.0.1` in the legacy implementation but `2.0-1 ==
2.0.1` in the one Maven actually runs. The legacy corpus is still worth carrying
for its breadth — the overwhelming majority of it agrees — but the conflicting
directives are marked and excluded rather than reconciled.

Because `GenericVersion` and `GenericQualifiers` depend on almost nothing (a
one-method interface, `java.util`, `java.math`), the real implementation can be
compiled straight out of the reference clone and used as a live **oracle**. jv
does this: `crates/jv-version/tests/oracle.rs` drives both sides over generated
inputs and compares tokenization, comparison sign, and qualifier detection. This
is a far stronger claim than any transcription, and the same trick applies
wherever an upstream component can be isolated cheaply.

### 3.6 Repositories, metadata, network (`jv-repo`, `jv-cache`)

- GAV→URL layout incl. checksum/signature/metadata URIs:
  `maven-resolver-impl/.../Maven2RepositoryLayoutFactory.java`.
- `maven-metadata.xml` (schema: `maven/api/maven-api-metadata/src/main/mdo/metadata.mdo`):
  SNAPSHOT → timestamped version (`DefaultVersionResolver.java`),
  `RELEASE`/`LATEST`, range candidates.
- Update policies `daily` / `interval:X` / `never` + `.lastUpdated` negative
  tracking: `DefaultUpdateCheckManager.java` / `DefaultUpdatePolicyAnalyzer.java`.
- Checksums: sha1 (mandatory), sha256/sha512 opportunistic; validation flow in
  `maven-resolver-connector-basic/.../ChecksumValidator.java`.
- **settings.xml (minimal but real, v0.1):** mirrors (`DefaultMirrorSelector.java`
  incl. `mirrorOf=*,!repo` syntax), server credentials, offline flag, local repo
  path. Enterprise CI (the sync adoption channel) is unusable without mirror
  support — Nexus/Artifactory is the default reality there. Encrypted passwords
  (`settings-security.xml`) and proxies → v0.2.
- HTTP driver: reqwest + HTTP/2, bounded concurrency, retry with backoff,
  Range resume for large jars, conditional requests for metadata. Transport
  behavior reference: `maven-resolver-transport-jdk-parent/.../JdkTransporter.java`
  (the closest analogue to a reqwest port).

**Cache design (three stores under `~/.cache/jv` / XDG):**

1. **Remote store (source of truth):** URL-keyed layout borrowed from coursier
   (`CachePath.java`: `<cache>/https/repo1.maven.org/maven2/...`) — the only
   scheme unambiguous across multiple repositories (yummy's flat layout has a
   multi-repo aliasing bug by construction). Sidecars borrowed from coursier's
   `Downloader.scala`: `.part` (resume), `.error` (negative cache w/ TTL),
   `.checked` (TTL marker), `.sha1`/`.sha256`. Concurrency-safe via file locks
   + intra-process interning (`CacheLocks.scala` pattern).
2. **`~/.m2/repository` materialization (interop):** `jv sync` hardlinks
   (fallback: reflink → copy) artifacts into Maven-standard layout **and writes
   `_remote.repositories` tracking files** (`EnhancedLocalRepositoryManager.java`
   + `TrackingFileManager`) — without them, `mvn -o` can reject present
   artifacts as "not available in offline mode". Also used as an opportunistic
   read source: if `.m2` already has a checksum-valid artifact, link it back
   instead of downloading.
3. **Env store (`jvx`):** `~/.cache/jv/envs/<key>/` with hardlinked jars, key =
   SHA-256 over the sorted resolved coordinate set — jgo's proven design
   (`jgo/src/jgo/env/_builder.py` `cache_key`, `_linking.py` hard→soft→copy
   strategy ladder).

Every store versioned with a `CACHE_FORMAT_VERSION` marker from day one.

### 3.7 `jvx` execution model (`jv-exec`)

Endpoint syntax ported from jgo (`src/jgo/parse/_endpoint.py`), the cleanest
prior art:

```
jvx com.google.googlejavaformat:google-java-format -- -i Main.java
jvx org.scijava:scijava-common+org.scijava:scripting-jython@ScriptREPL
jvx g:a:1.2.3@com.example.Main -- args...
```

- `+` joins coordinates; `@` names the main class; version optional (resolve
  `RELEASE` from metadata).
- Main-class detection ladder (jgo `_jar.py`): explicit `@` → manifest
  `Main-Class` → scan jar entries for unique `*Main.class`-style candidates →
  error listing candidates. (coursier's `MainClass.scala` is the second
  reference.)
- v0.1 launches with `-cp` only. JPMS module-path splitting (jgo's
  `exec/_runner.py` auto/classpath/module-path modes) → v0.3.
- If no JDK is found on PATH/JAVA_HOME: clear error in v0.1; auto-provisioning
  via foojay Disco API (wukong `src/foojay.rs` is a working Rust client) → v0.3.

### 3.8 `jv sync` semantics

Spec source: `maven-dependency-plugin/.../resolvers/GoOfflineMojo.java` (308 LOC).
Two passes, both required for a true `mvn -o` guarantee:

1. **Plugins:** every `<build><plugins>` plugin artifact **plus each plugin's own
   transitive dependencies** (this is what naive go-offline clones miss), with
   reactor exclusion.
2. **Project dependencies:** full transitive closure per module with
   dependencyManagement applied, all scopes by default (`requiresDependencyCollection=TEST`
   equivalence), reactor modules excluded.

Known upstream gaps to inherit-and-document (parity, not perfection): build
extensions and toolchains are not prefetched by upstream go-offline either.
`jv sync` must be multi-module aware (reactor discovery from `<modules>`).

### 3.9 `jv tree` output parity

`TreeMojo.java` dispatches text rendering to the **external**
`org.apache.maven.shared:maven-dependency-tree` artifact (`SerializingDependencyNodeVisitor`
+ `GraphTokens`), which is *not* in the current clones.

**Action item:** `git clone --depth 1 https://github.com/apache/maven-dependency-tree
_reference/maven-dependency-tree` — byte-parity of the default text output is a
launch requirement (the gif shows `diff <(mvn ... ) <(jv tree)` returning empty).
Non-text formats (`json`, `dot`, `tgf`, `graphml`) are local to the plugin clone
(`tree/*DependencyNodeVisitor.java`) and easy. The resolver's own
`DependencyGraphDumper.java` format is *not* the same and must not be shipped as
the default.

### 3.10 Lockfile (`jv-lock`, v0.2 flagship)

Maven has no native lockfile — this is the durable differentiator and the
supply-chain-security story (its own blog post). Design:

- `jv.lock` (TOML), committed. Per artifact: GAV, packaging/classifier,
  resolved repo URL, sha256, effective scope, dependency edges (enough to
  reconstruct the tree offline).
- `jv lock` generates from the current POM state; `jv sync --frozen` installs
  exactly the lock (hash-verified, no resolution) and fails on drift;
  `jv sync --check` diffs lock vs POM reality (CI gate).
- Env-independent: lock records the full profile-activation inputs it assumed
  (os/jdk properties) so CI and laptop agree or fail loudly.
- Prior art to read: yummy `ym-lock.json` + `src/workspace/lockfile_diff.rs`;
  coursier `ArtifactsLock.scala`; uv's lockfile docs for UX conventions.

---

## 4. Compatibility spec map (where the truth lives)

Quick-reference table for implementers; all paths relative to `_reference/`.

| Behavior | Authoritative source |
|---|---|
| Effective POM pipeline | `maven/impl/maven-impl/.../model/DefaultModelBuilder.java` |
| Inheritance merge precedence | `maven/impl/maven-impl/.../model/MavenModelMerger.java` |
| Interpolation | `.../model/DefaultModelInterpolator.java`, `DefaultInterpolator.java` |
| Profile activation | `maven/impl/maven-impl/.../model/profile/*` (Maven 3 activators only) |
| BOM import / depMgmt injection | `.../model/DefaultDependencyManagementImporter.java`, `...Injector.java` |
| POM model schema | `maven/api/maven-api-model/src/main/mdo/maven.mdo` |
| Version ordering/ranges | `maven-resolver/maven-resolver-util/.../version/GenericVersion*.java` |
| Conflict resolution | `maven-resolver-util/.../graph/transformer/{ConflictResolver,ClassicConflictResolver,NearestVersionSelector,ConflictMarker,ConflictIdSorter}.java` |
| Scope selection/derivation | `.../transformer/{JavaScopeSelector,JavaScopeDeriver}.java` + `maven/impl/maven-impl/.../resolver/scopes/Maven3ScopeManagerConfiguration.java` |
| depMgmt at collection time | `maven-resolver-util/.../graph/manager/ClassicDependencyManager.java` |
| Exclusions | `maven-resolver-util/.../graph/selector/ExclusionDependencySelector.java` |
| BF collection + skipping | `maven-resolver-impl/.../collect/bf/{BfDependencyCollector,DependencyResolutionSkipper}.java` |
| Transformer chain order | `maven/impl/maven-impl/.../resolver/MavenSessionBuilderSupplier.java` (canonical wiring) |
| POM→deps bridge, relocation | `maven/impl/maven-impl/.../resolver/DefaultArtifactDescriptorReader.java` |
| GAV→URL layout | `maven-resolver-impl/.../Maven2RepositoryLayoutFactory.java` |
| SNAPSHOT / RELEASE / LATEST | `maven/impl/maven-impl/.../resolver/DefaultVersionResolver.java` |
| Update policy / offline | `maven-resolver-impl/.../DefaultUpdateCheckManager.java`, `offline/OfflineRepositoryConnector.java` |
| Local repo tracking | `maven-resolver-impl/.../EnhancedLocalRepositoryManager.java` |
| Mirrors / auth | `maven-resolver-util/.../repository/{DefaultMirrorSelector,AuthenticationBuilder}.java` |
| Checksum validation | `maven-resolver-connector-basic/.../ChecksumValidator.java` |
| go-offline scope | `maven-dependency-plugin/.../resolvers/GoOfflineMojo.java` |
| tree rendering | `maven-dependency-plugin/.../tree/TreeMojo.java` + external `maven-dependency-tree` (clone needed) |
| Design prose | `maven-resolver/src/site/markdown/{how-resolver-works,dependency-graph,common-misconceptions}.md` |

License note: jv and all the upstream sources it mirrors are Apache-2.0, so
copying would be permitted. The rule for agents is nonetheless
**extract behavior and test cases as a spec; do not translate code verbatim** —
a port that reasons from the behavior catches the quirks that matter, where a
transliteration inherits structure jv has no use for. Test fixtures (data files)
are carried across with attribution.

---

## 5. Testing strategy

Correctness is the product. Four rings, inside-out:

**Ring 0 — oracle tests against isolated upstream components.**
Where an upstream class can be compiled without dragging in its whole module,
compile it from `_reference/` and diff jv against it over generated inputs. This
is strictly stronger than any transcription and catches what nobody thought to
assert; `jv-version` establishes the pattern (§3.5). Candidates for the same
treatment: `GenericVersionRange`/`VersionSchemeSupport`, `ConflictMarker`,
`ConflictIdSorter`, and the `DependencyGraphParser` DSL.

**Ring 1 — ported unit corpora (offline, in `cargo test`).**
Build `jv-testkit` parsers first; they unlock everything else:
- Port `maven-resolver-test-util/.../DependencyGraphParser.java` (the compact
  graph DSL) → unlocks the **54 transformer cases** in
  `maven-resolver-util/src/test/resources/transformer/` (version-resolver 18,
  scope-calculator 13, conflict-id-sorter 4, conflict-marker 4, optionality 3,
  strategy-divergence 3) plus the `visitor/` tree-rendering goldens.
- Port `IniArtifactDataReader.java` → unlocks the **624-file corpus** in
  `maven-resolver-impl/src/test/resources/artifact-descriptions/` (collection +
  conflict semantics with `_BF`/`_DF` goldens) — the single largest portable
  resolution corpus.
- Version corpora from §3.5 (~2000 LOC of ordered arrays and range cases).
- Effective-POM corpus: `maven/impl/maven-core/src/test/resources-project-builder/`
  (**106 project dirs** behind `PomConstructionTest.java`'s 128 tests) +
  `impl/maven-impl/src/test/resources/poms/` (113 XML) +
  `compat/maven-model-builder/src/test/resources/poms/` (117 XML — the
  inheritance golden files live only here).

**Ring 2 — coursier fixtures as a contrast oracle.**
`coursier/modules/tests/shared/src/test/resources/resolutions/` — 222 plain-text
fixtures (`org:name:version:config` per line), directly parseable from Rust.
Expect and *assert* divergence on nearest-vs-latest diamonds; agreement
everywhere else is a strong signal. (Input POM corpora are in unchecked-out
submodules `coursier/test-metadata` + `handmade-metadata` — clone if hermetic
replay is wanted.)

**Ring 3 — live differential harness vs real Maven.**
`jv-testkit` drives `mvn -q dependency:tree -DoutputType=text` and
`dependency:list` against a pinned corpus of real Maven projects and diffs
byte-for-byte against `jv tree` / `jv resolve`:
spring-petclinic (demo project), dropwizard (clean mid-size), netty (large
multi-module), camel + flink + hadoop (deep Apache parent chains), quarkus
(BOM/depMgmt stress test). Pin exact commits; cache `.m2` in CI. This harness
doubles as jgo's proven pattern (its `MvnResolver` exists precisely to be the
oracle for its `PythonResolver`).

**Ring 4 — behavioral spec of last resort.**
`maven/its/core-it-suite/` (751 IT classes, 666 `mng-*` resource dirs).
Not ported wholesale; grep-selected when a Ring-3 diff needs adjudication
(`*Inheritance*`, `*Interpolation*`, `*ProfileActivation*`, `*DepMgmt*`).

CI gates from M2 onward: all rings green on Linux/macOS, plus a
benchmark job (criterion + hyperfine) tracking `jv tree` warm latency so
performance regressions are caught like correctness regressions.

---

## 6. Milestones

Sequenced by dependency, not calendar. Each has a hard acceptance gate.

### M0 — Scaffolding & test infrastructure ✅
Workspace layout (§3.2), CI matrix (Linux + macOS, fmt/clippy/test/doc),
release profile, `_reference/` discovery for spec sources, `maven-dependency-tree`
cloned. Corpus and oracle harness conventions established and documented in
`docs/development.md`.
**Gate:** met for the version corpora; `jv-testkit`'s remaining parsers (graph
DSL, `.ini` descriptors, coursier fixtures, mvn-output differ) land with the
milestones that consume them, since a parser with no consumer cannot be shown to
round-trip anything. `cargo-dist` configuration is deferred to M5, when there is
a binary to ship — dist config for a workspace of libraries would be untestable
ceremony.

### M1 — `jv-version` ✅
GenericVersion + qualifiers + ranges/constraints/unions.
**Gate:** met, and raised. 323 corpus directives pass (expanding to ~1500
ordering assertions) with the 2 known legacy disagreements marked and excluded;
115 range/constraint/qualifier directives pass; and 50,862 generated checks agree
with the real `GenericVersion` compiled from the reference clone. The original
gate ("100% on both corpora") was unreachable as written — see §3.5.

### M2 — `jv-model` + `jv-model-builder` ✅
Streaming POM parser; full effective-POM pipeline (§3.3); settings.xml and
maven-metadata.xml models.
**Gate:** met by a stronger route than planned. Rather than adapting
`PomConstructionTest`'s harness, jv is diffed against **real Maven 3.9.9**:
`mvn help:effective-pom` emits a POM, jv's own parser reads it, and the two
`Model` values are compared field by field. Nine fixture projects agree exactly,
covering multi-module inheritance, a real Central BOM import with local override,
`activeByDefault` suppression against file activation, `${revision}` across a
parent boundary, and a three-level chain carrying a version from a grandparent
property through management into a declaration. The parser is separately verified
against every POM in the reference clones (2865 of 2867; the two exceptions are
not POMs).

Two ROADMAP assumptions were corrected while implementing this. Maven 3.9 and
Maven 4 order interpolation sources differently — 3.9 ranks `${project.*}` above
both user and POM properties, Maven 4 below both — so following the wrong one
would silently change resolved versions. And Maven 4 removed `central` from the
super POM, so the super POM has to come from a real 3.9 distribution rather than
from the reference clone, or no project would have any repository at all.

### M3 — `jv-resolver` (pure core) — conflict resolution ✅, collection next
BF collection + skipper, exclusions, classic depth-1 depMgmt, ranges,
optional handling, cycles; transformer chain (marker → sorter → nearest-wins +
scope tables); verbose-mode data retention.

**Done:** the transformer chain, ported against
`docs/spec/conflict-resolution.md`. `ConflictMarker`, `ConflictIdSorter` and
`ClassicConflictResolver` with its scope and optionality selectors all pass
Maven's own corpus — 32 cases ported from `NearestVersionSelectorTest`,
`JavaScopeSelectorTest`, `ConflictIdSorterTest`, `SimpleOptionalitySelectorTest`
and `ConflictMarkerTest`, three of which are templates covering 48 scope
combinations between them. `jv-testkit` reads both corpus formats: the graph DSL
(45 files) and the `.ini` artifact descriptors (616 files).

**Also done:** collection — the BF walk, the resolution skipper,
`ClassicDependencyManager`'s depth-2 rule, the exclusion/optional/scope
selectors, `FatArtifactTraverser`, range expansion and cycle handling, against
`docs/spec/collection.md`.

**Open:** one collection golden. `cycle.txt` expects a node jv skips as a
duplicate; jv's rule transcribes `DependencyResolutionSkipper.isLeftmost`
directly and disagrees under either depth convention. Disabling the duplicate
rule makes every golden pass and makes the 556-descriptor `cycle-big` case hang,
so the rule is load-bearing and the answer is not to drop it. The test carries
the analysis and is marked `#[ignore]` rather than deleted; settling it needs a
traced run of upstream's own test.

**Gate:** transformer corpus green (met); the artifact-description corpus green
apart from the one case above; coursier contrast suite still to document.

### M4 — `jv-repo` + `jv-cache` (network & store) ✅
Layout, metadata/SNAPSHOT resolution, update policies, checksum validation,
negative cache, remote store with sidecars + locking, `.m2` opportunistic reads,
mirrors + server auth from settings.xml, `--offline`.

**Done.** `jv-repo` holds layout, update and checksum policies, and the mirror
and credential rules from `settings.xml`, including the shallow-merge-by-id of
the installation and user files. `jv-cache` holds a URL-keyed store with
`.part`/`.error`/`.checked`/`.lock` sidecars, a transport that serves `file:`
and HTTP through one path, and the fetcher that orders them: jv's cache, then
`~/.m2` read-only, then each repository in turn.

Three decisions worth recording. An encrypted `settings.xml` password is
*withheld* rather than sent, because ciphertext authenticates as nobody and
produces a baffling 401. A checksum policy of `warn` actually warns, reported
through `Fetched::warnings` — a `warn` that says nothing is `ignore` under
another name. And a broken repository is held back until every repository has
been asked, so one unreachable mirror cannot hide an artifact another one has.

**Open:** a `kill -9` mid-download test. The atomic-rename write makes the
outcome safe by construction, but "safe by construction" is an argument, not a
test.

### M5 — `jv tree` + `jv resolve` + differential harness — mostly done
Text renderer at byte parity (via `maven-dependency-tree` port), json/dot/tgf/
graphml, scope filtering, multi-module.

**Done:** all five output formats, each ported from its upstream visitor;
`jv-driver`, which is where the pure crates meet the machine; and `jv-cli` with
`jv tree` and `jv resolve`. The differential harness
(`crates/jv-cli/tests/mvn_tree_oracle.rs`) runs real Maven 3.9.9 against eight
POMs chosen for resolution behaviours rather than popularity — nearest-wins,
managed transitives, BOM import, exclusion, the scope matrix, optional
dependencies, and a wide graph where conflict ordering decides the outcome —
and **all eight match byte for byte**.

Two divergences are recorded rather than hidden. `<repositories>` are scoped per
node by Maven; jv accumulates them into one ordered list, because
`DescriptorSource` has no node context to hang the scoping on. This finds
strictly more artifacts than Maven, never fewer. And graphml and tgf id their
nodes by JVM identity hash upstream, which cannot be reproduced; jv numbers them
sequentially in visit order.

**Open:** the six Ring-3 projects end to end (the harness is built, the fixtures
are synthetic); the benchmark table and its committed script.

### M6 — `jv sync` + `setup-jv` GitHub Action
Both go-offline passes (§3.8), `_remote.repositories` writing, multi-module
reactor, hardlink materialization.
**Gate:** `jv sync && mvn -o verify` succeeds on all six Ring-3 projects;
`setup-jv` action published with a real before/after CI-minutes number on a
public repo.

### M7 — `jvx`
Endpoint parsing, env store, main-class ladder, arg passthrough.
**Gate:** `jvx com.google.googlejavaformat:google-java-format -- --version`
works from a cold cache; second run < 150ms to JVM exec; 20-tool smoke matrix
(formatters, linters, checkstyle, pmd, jbang-style utilities) green.

### M8 — v0.1 launch
README (tree gif as `diff`-proof; honest cold/warm/startup benchmark table),
`curl | sh` + Homebrew + binstall, docs site (own the search landing page),
Show HN "jv: uv for the JVM", deep-dive blog post ("Why Maven has no lockfile —
and how we built one" teasing v0.2), Coursier FAQ entry, then r/java →
newsletters → Kotlin Slack over the following week (different reaction time
constants).
**Gate:** a stranger on a clean machine goes install → `jv tree` wow-moment in
under 60 seconds.

### v0.2 — Trust & depth
`jv lock` / `sync --frozen` / `sync --check` (§3.10) + supply-chain blog post;
`jv tree --verbose` (conflict/premanaged annotations); `jv why <ga>`
(path-recording visitor); proxies + `settings-security.xml`; sha256 enforcement
mode; `jv purge`/cache GC.

### v0.3 — Breadth
JDK provisioning (foojay Disco; wukong `src/foojay.rs` as reference) +
`jv jdk` subcommands; JPMS module-path launching for `jvx`; Gradle Module
Metadata + variant selection (coursier `GradleModule.scala`/`VariantSelector.scala`
as reference) — the gateway to Kotlin/Android users; `jv install` (persistent
tool installs à la `uv tool install` / coursier `InstallDir`).

---

## 7. Risks & open questions

| Risk | Mitigation |
|---|---|
| Long-tail POM weirdness in the wild breaks parity | Ring 3 corpus is real-world and diverse by construction; every diff becomes a regression fixture; ship `jv tree --debug-model <ga>` early to make user bug reports self-serve. |
| `mvn dependency:tree` output differs across Maven versions | Pin the oracle (Maven 3.9.x) in the harness; document the pinned version in the README benchmark table. |
| Enterprise settings.xml complexity (mirrors-of-mirrors, encrypted creds, proxies) | Minimal-but-real subset in v0.1 (§3.6); loud, specific errors for unsupported constructs rather than silent misresolution. |
| yummy or wukong pivots into the same wedge | Speed to M5/M8; the moat is the corpus-backed compatibility test suite, which is expensive to replicate and compounds. |
| Hardlinks unavailable across volumes / filesystems | Link ladder degrades hard→reflink→copy per volume (jgo pattern). |
| "No Windows?" pushback at launch | State it plainly in the README with a tracking issue; keep core types path-portable so the port is mechanical, not architectural. |
| HN skepticism ("Coursier exists", "benchmarks are cherry-picked") | FAQ pre-emption; benchmarks split cold/warm/startup with a committed reproduction script; never headline cold-download numbers. |

Open questions to settle during M2–M4 (tracked as issues, not blockers):
- Async depth: tokio through `jv-repo`/`jv-cache` only, or full-async CLI?
  (Current lean: async confined to the driver; core stays sync/pure.)
- `jv resolve` machine output format (line-oriented vs JSON) — coordinate with
  agent-tooling use cases before freezing.
- Whether `jv sync` should also write a coursier-compatible cache view (probably
  no; `.m2` is the interop surface that matters).

---

## 8. Reference clone inventory

Already cloned under `_reference/`: `maven`, `maven-resolver`,
`maven-dependency-plugin`, `coursier`, `jgo`, `yummy`, `wukong`.

Still to clone: `apache/maven-dependency-tree` (M0, required for text parity —
§3.9); `astral-sh/uv` (architecture reference: CAS, hardlink strategy, lockfile
UX); optionally `coursier/test-metadata` + `coursier/handmade-metadata`
(hermetic POM corpora for Ring 2) and `spring-projects/spring-petclinic` +
the Ring-3 project pins when M5 starts.
