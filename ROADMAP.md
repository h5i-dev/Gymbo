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

**Every v0.1 command is read-only.** `tree`, `resolve` and `sync` inspect the
POM or populate a cache; not one of them edits it. That is the line between an
*inspector* and a *package manager*, and it is why v0.2 leads with `jv add`
(§3.11).

### 1.1 What jv replaces, command by command

The uv comparison is useful for reasoning about *sequencing* (§1.2) and
misleading as a feature map — uv's `venv` and `run` have no Java counterpart,
because Maven already builds the classpath and Java has no activate-the-
environment step. The design basis is this table instead: what a Maven user
runs today, and what jv gives them for it.

| Today | jv | What jv adds beyond speed |
|---|---|---|
| `mvn dependency:tree` | `jv tree` | nothing but speed; low frequency |
| `mvn dependency:list` | `jv resolve` | no JVM start, script-friendly |
| `mvn dependency:build-classpath` | `jv resolve --classpath` | usable from scripts and agents |
| `mvn dependency:go-offline` | `jv sync` | parallel fetch; isolated repositories (§3.8) |
| `mvn dependency:tree -Dincludes=g:a` | `jv why g:a` | direct answer instead of a filtered tree |
| `mvn dependency:add -Dgav=g:a:v` | `jv add g:a` | **resolves the version**; see §3.11 |
| `mvn dependency:remove -Dgav=g:a` | `jv remove g:a` | speed only |
| `mvn versions:display-dependency-updates` | `jv outdated` | dependencies, properties and BOMs in one view |
| `mvn versions:use-next-releases` | `jv upgrade` | edit, re-resolve and re-lock in one step |
| `dependency:get` → classpath → `java -cp` | `jvx g:a -- …` | one command, no install |
| *(no Maven equivalent)* | `jv lock` | checksums over direct, transitive and plugin artifacts |
| *(no Maven equivalent)* | `jv sync --frozen` | POM/lock drift detection |

Three tiers fall out, and they are worth separating because they carry
different amounts of weight:

1. **Faster Maven commands** — `tree`, `resolve`, `why`, `outdated`. Low
   migration cost, but speed is the only argument, and every one of them is
   infrequent.
2. **Several Maven operations fused into one** — `add`, `remove`, `upgrade`,
   `jvx`. Adding a dependency today means `dependency:add` (with a version you
   had to go look up), then `go-offline`, then a `tree` to check what it pulled
   in. `jv add org.postgresql:postgresql` is all three.
3. **Things Maven has no answer for** — `lock`, `sync --frozen`. Maven-Lockfile
   and similar exist outside core, but there is no first-class lockfile. This
   tier is the only one a Maven plugin cannot take back.

### 1.2 The frequency problem, stated plainly

**jv has no equivalent of `pip install`: no high-frequency operation it
replaces.** This is the central risk and it is not fixable by adding features.

In Python, installing dependencies *is* a daily step, which is why uv could win
by making one narrow thing 80–115× faster. In Java, `./mvnw test` resolves
dependencies implicitly. Nobody runs `dependency:go-offline` by hand, and
`dependency:tree` comes out a few times a week at most. So `jv tree`'s 69× is
the demo rather than the product, and `jv sync` is genuinely useful in CI and
containers while being invisible to a local developer whose build already
resolves for them.

Two consequences run through the rest of this document. Tier 2 and tier 3 above
are where the argument has to be won, because tier 1 is real work that changes
nobody's day. And CI, containers and agent sandboxes matter more to jv than
they would to a tool with a daily local hook — that is where fresh dependency
resolution is a cost someone is actually paying.

**Deliberate non-goals for v0.1** (each is a focus hazard):
- JDK management (mise/sdkman/jenv territory) → post-v0.2, and **not** because
  it is hard. An earlier revision of this document pulled it forward to v0.2 on
  the strength of `uv python install`; that argument does not survive contact
  with the competition. uv owned Python version management because nothing else
  did it well and uv already sat on the install path. SDKMAN, mise and jenv are
  established, and jv brings no angle to JDKs that they lack. Being second-best
  at somebody else's category buys nothing.
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

**What jv claims to be**, which is narrower than "package manager for the JVM"
and truer for it: **the dependency and toolchain layer, without the JVM.**
Maven-the-build-tool owns the inner loop and jv has deliberately declined to
fight it. Framed that way, not compiling anything reads as a decision rather
than a gap, and `add` / `lock` / `jdk` become the obvious core rather than scope
creep. jv also stays POM-native permanently: `pom.xml` and `~/.m2` interop *are*
the moat, and a jv-specific manifest would trade it away for nothing.

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

### 3.6.1 Where jv is actually fast, measured

Not resolution, and not builds. `jv sync` versus a plain `mvn verify` from
nothing is **0.92x** — jv is slower. Warm it is 0.84x. Those numbers are in
`scripts/benchmark-build.sh` and they are not going to improve much, because a
build is dominated by compilation and jv does not compile.

What jv avoids is Maven's fixed cost. Every `mvn` invocation pays roughly a
second before it does anything: JVM start is only 157ms of that, the rest is
classworlds, the Plexus container and plugin loading. For any operation whose
real work is small, that fixed cost *is* the operation.

    mvn -o -N validate, empty POM        ~990ms   <- Maven's floor, any goal
    jv tree, whole graph                   16ms

    versions:display-dependency-updates  1637ms
    jv outdated                            47ms      34.8x

    mvn exec:java (google-java-format)   1129ms
    jvx           (same tool)             139ms       8.1x
    bare java -cp, classpath known         93ms

The `outdated` comparison is equalised: the plugin's `<dependencyManagement>`
pass is turned off, because jv does not report managed entries. Leaving it on
gave 42.4x, which measured a difference in scope as much as speed.

One number there is worth more than the ratios. Turning off that pass removed
19 of the plugin's 29 lookups and saved **16ms** of 1653ms. The lookups cost
about a millisecond each; everything else is the host. That is the whole
argument for this project in one measurement — and it is also why replacing
Maven's *resolver* was abandoned (§3.13): resolution is 3.5% of a warm build.

### 3.7 `jvx` execution model (`jv-exec`)

**Measured, and this is where the speed story actually is.** The profiler
(§3.13) showed a warm Maven build spends 3.5% of its time resolving and 65%
executing mojos, which killed the idea of replacing Maven's resolver. But that
same build carries ~1s of JVM startup and Maven bootstrap — classworlds, the
Plexus container, extension loading — which a build amortises over real work and
a *tool run* does not. Running one formatter over one file is almost entirely
that overhead.

    google-java-format --version, warm, medians of 5-7

    bare `java -cp` with the classpath already known      93ms
    jvx                                                  139ms   (+51ms)
    mvn exec:java                                       1129ms   (+1036ms)
                                                        -> 8.1x

jvx lands within 51ms of the floor: that is what resolving the tool, building
the classpath and launching costs when Maven is not in the way. Maven's tax on
the same work is twenty times larger.

`mvn -v` alone is 157ms, so this is not a JVM story — it is a Maven story. The
JVM is not what makes running a Java tool slow; the build system wrapped around
it is.

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
  via foojay Disco API (wukong `src/foojay.rs` is a working Rust client) is
  deferred with the rest of JDK management; see §1 non-goals.

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

#### Isolated repositories — a CI, container and sandbox feature

`--local-repository DIR` already populates somewhere other than `~/.m2`, which
makes a per-job, per-container or per-sandbox Maven repository possible:

```
jv sync --frozen --local-repository .jv/m2
./mvnw -o -Dmaven.repo.local=.jv/m2 verify
```

This matters where a *fresh* repository is a cost someone pays — CI runners,
Docker builds, reproducible builds, untrusted builds, and h5i sandboxes, where
every agent box otherwise re-downloads a dependency set the host already has.
Sharing jv's cache across boxes while keeping each `.m2` separate is the
integration worth building; a sandbox must not be able to corrupt the shared
store, so raw writable hardlinks into it are not an option (reflink, or
read-only materialisation, or copy).

Two scope decisions, both deliberate:

**No `jv run`.** A wrapper like `jv run -- ./mvnw test` was considered and
rejected. It is a straight import of `uv run` that does not survive the
translation: Python needs it because a virtualenv must be selected before
execution, and Java has no such step — Maven already assembles the classpath,
and the local repository is not an execution environment. Wrapping `./mvnw`
lengthens the command and buys nothing. If a future need appears, it will come
from lockfile drift checking, not from environment selection.

**Content-addressed storage with reflink materialisation is not v0.x
architecture.** It would make many isolated repositories nearly free, and it is
the one capability no Maven plugin could retrofit. It is recorded here as a
future direction rather than scheduled, on the judgement that near-term effort
belongs in real-project compatibility and the package-manager surface. Revisit
when h5i's Java workload makes the copy cost measurable.

### 3.8.1 How complete `jv sync` actually is

Measured, not asserted. The last full corpus run was **10 BUILD SUCCESS and 16
BUILD FAILURE** — the "0 failures attributable to jv" that circulated for a
while was wrong, and came from reading a summary instead of the logs. The
failures fall into three classes, and only the third is open.

**Coordinates inside plugin `<configuration>` — 10 projects. Fixed.** Plugins
resolve artifacts named in their own configuration when they run, and those
appear in no `<dependencies>` block: `maven-compiler-plugin`'s
`<annotationProcessorPaths>`, `animal-sniffer`'s `<signature>`,
`maven-remote-resources`' `<resourceBundles>`, japicmp's previous release. The
parser now scans configuration for coordinates rather than discarding it.

Checked against the 11,778 POMs in the corpus cache rather than fixtures, which
is what caught the first attempt inventing artifacts out of OSGi bundle
instructions and `maven-enforcer-plugin` ban patterns — both deliberately
coordinate-shaped, neither naming anything to download.

**A `<plugin>` with no `<version>` — 3 projects. Fixed.** Maven resolves one from
`maven-metadata.xml` at build time: `<release>`, then `<latest>`, then greatest.
jv now does the same, and records the file so Maven reaches its own answer
offline instead of trusting jv's.

**A default compiled into the plugin — 5 projects. Open, and not fixable by
reading.** `spotless` picks a formatter whose version is a constant inside the
spotless jar:

    spotless 2.40.0  ->  palantir-java-format 2.38.0
    spotless 2.43.0  ->  palantir-java-format 2.39.0
    jetty            ->  sortpom-sorter 3.2.1  (a different step entirely)

No amount of reading a project finds a version that exists only inside a jar,
and a table of every plugin's defaults would be wrong the next time any of them
ships a release. `mvn dependency:go-offline` fails on exactly this too — it is
why go-offline is documented as incomplete.

So `jv sync --also group:artifact:version` is the escape hatch: name what the
build turns out to need, and it is fetched with its closure like anything else.
The general fix, when there is one, is to learn the set from a build that
succeeded rather than to predict it — which is another argument for §3.10's
lockfile.

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

### 3.10 Lockfile (`jv-lock`, v0.2)

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

**Lock the plugin closures, not just the dependency tree.** The speed argument
for a lockfile is usually "skip resolution", and on jv that argument is close to
worthless: resolving commons-io's own dependency tree from a warm cache takes
18ms. Where a warm sync actually goes, measured with both the store and the
local repository populated:

    plugin closures   442ms    95%
    placing files      35ms
    dependency tree    18ms

Each plugin gets its own full `resolve_request`, and a reactor resolves the same
plugin once per module. Resolving them in parallel and collapsing identical
declarations took warm sync to 1.85x–3.40x (commons-io 420→213ms, log4j2
2100→795ms, surefire 735→216ms) with byte-identical output trees — so the
remaining warm cost is *still* plugin closures, roughly 190ms of a 213ms sync.

A closure is a pure function of the plugin's coordinates and its
`<plugin><dependencies>` block, which is exactly what a lockfile can record. So
the lockfile's performance case is memoising plugin closures across runs; a lock
that captures only the dependency tree would leave 95% of the warm cost in
place. Correctness and supply-chain auditability are still the headline reasons
to build it — this is the reason it also makes `jv sync` fast.

**Done, as a cache rather than a lockfile** (`jv-driver/src/plugin_memo.rs`).
Closures are remembered between runs in jv's cache, keyed on the plugin's
coordinates, its `<dependencies>` block and the repository set. Expiry reuses
`UpdatePolicy` rather than inventing a second notion of stale, so a remembered
closure can never be staler than what Maven's own daily metadata check would
have used, and `-U` bypasses it. Degraded resolves are never written: a cached
warning is a warning that never prints again, because the run that reads the
memo does no work.

    warm sync, offline, alternated, medians
                        this morning   parallel   +memo
    commons-io                 368ms      213ms    99ms   3.72x
    dropwizard  (38 mod)      1724ms      854ms   775ms   2.22x
    log4j2      (37 mod)      2029ms      772ms   638ms   3.18x
    surefire    (15 mod)       711ms      232ms   216ms   3.29x
    maven-resolver (15 mod)    649ms      216ms   169ms   3.84x
    byte-buddy  (15 mod)       641ms      206ms   150ms   4.27x

The lockfile itself is still worth building for the reasons it was always worth
building; it no longer has to carry the performance argument alone.

**Then the reactor, which was the next thing along.** With closures remembered,
a large reactor's warm sync is its own modules: 706ms of dropwizard's 774ms
across 38 modules, where one module costs 151ms. Resolving them in parallel gave
almost nothing at first — 1.23x, with CPU at 207% of one core on a ten-core
machine and scaling that stopped dead at two threads. The modules were
serialising on the caches they share: `poms`, `descriptors`, `versions`,
`repositories` and the reactor map were each behind a `Mutex`, so on a warm run,
where every lookup is a hit, readers were queueing behind readers. Behind an
`RwLock` they are not:

    dropwizard, 38 modules, warm            threads  1     2     4    10
    Mutex                                            735   603   602   606
    RwLock                                           737   486   384   390

    warm sync, alternated, medians, trees byte-identical
    dropwizard      (38 mod)   766ms -> 352ms   2.18x
    logging-log4j2  (37 mod)   566ms -> 368ms   1.54x
    maven-surefire  (15 mod)   185ms -> 155ms   1.19x
    byte-buddy      (15 mod)   142ms -> 136ms   1.04x
    commons-io       (1 mod)    89ms ->  89ms   1.00x

Scaling still flattens after four threads and CPU sits at 366%, so contention
remains — `unreachable`, `snapshots` and `range_metadata` are still `Mutex`, and
the resolves block on one shared runtime. Worth another look if large reactors
matter more later.

**What is *not* left is placement.** The premise of §3.12 below was that
materialising into `~/.m2` is a step Maven does not pay. It is, but it is
already free: with a populated local repository, skipping placement entirely
(`--cache-only`) is not measurably faster than doing it, because placement is
stats that find the file already there.

    with a populated local repository    full   no-place
    commons-io                           48ms       55ms
    dropwizard                          724ms      796ms
    byte-buddy                          105ms      107ms

So §3.12 is still worth doing, for the reason that survives measurement — one CI
cache instead of two — and not for speed.

### 3.12 Store the artifacts where Maven reads them (v0.2)

jv keeps a URL-keyed store and then materialises it into `~/.m2` so Maven can
read it. Maven's cache *is* the repository it reads, so on a warm run Maven has
no prepare step at all and jv has one.

**Measured, and there is no gain. Do not pick this up without re-measuring.**
Every benefit claimed for it evaporated when checked:

- *Speed.* Placement is already free. With a populated local repository,
  skipping it entirely (`--cache-only`) is not measurably faster than doing it,
  because placement is stats that find the file already there — commons-io 48ms
  vs 55ms, dropwizard 724ms vs 796ms, byte-buddy 105ms vs 107ms.
- *Local disk.* jv places by hardlink, so the "second tree" shares inodes with
  the first. A placed jar reports 33 links and the same inode as its store copy,
  and `du` over the store and the repository together reports 2.4G — the same as
  the store alone.
- *CI cache.* There is no second tree to drop. A runner caches jv's store and
  materialises from it; measured on commons-io that store is 328MB against the
  386MB `~/.m2` Maven caches, so jv already restores less than Maven does.

What remains is the architectural argument — one layout instead of two, and jv
no longer representing a state Maven cannot. That is real but it is not worth
what it costs: the change lands on the negative cache, the download locks, the
atomic writes and multi-repository semantics all at once. In particular, keying
content by GAV would collapse two repositories' URLs onto one path, and the
`.error` sidecar that records a 404 from one repository would then suppress
lookups against the other — a correctness bug in exchange for no measured
benefit.

It is a remapping rather than a rewrite. The store path is already the
repository base followed by the Maven layout path:

    store:  cache/https/repo.maven.apache.org/maven2/  commons-io/commons-io/2.16.1/commons-io-2.16.1.jar
    m2:                                        ~/.m2/  commons-io/commons-io/2.16.1/commons-io-2.16.1.jar

so `path_for` changes from "scheme/host/path" to "strip the repository base,
root at the local repository", and the materialisation pass disappears. Four
things need care:

- **Two repositories serving one GAV.** Maven layout holds one, with
  `_remote.repositories` recording which — a file jv already writes. jv can
  currently represent a state Maven cannot, and collapses it during
  materialisation anyway.
- **`maven-metadata.xml`** becomes repo-qualified (`maven-metadata-<id>.xml`),
  which is Maven's own scheme.
- **Negative caching and freshness** move from URL-keyed sidecars to Maven's
  `.lastUpdated` markers, which improves `mvn -U` interop.
- **`jvx --no-local-repository`** points at a scratch Maven-layout directory.

Beyond the prepare step, this halves what CI caches: a runner currently restores
jv's store *and* a materialised local repository, two trees holding the same
bytes.

### 3.11 POM mutation (`jv-edit`, v0.2)

`jv add`, `jv remove`, `jv upgrade`, `jv outdated`.

**Read this before designing it: Apache got here first.**
maven-dependency-plugin 3.11 ships `AddDependencyMojo` and
`RemoveDependencyMojo`, and they are good. Verified by reading
`_reference/maven-dependency-plugin/.../AddDependencyMojo.java` (822 LOC), not
from the changelog:

- Format-preserving edits through `eu.maveniverse.domtrip.maven.PomEditor` —
  comments, indentation and encoding survive.
- `dependencyManagement`-aware: omits `<version>` when a parent or BOM already
  manages the artifact.
- Detects the surrounding convention and follows it, including whether versions
  go through `${…}` properties **and what the property naming pattern is**.
- Duplicate detection that is type- and classifier-aware; `<profile>` targeting.

An earlier revision of this section claimed format preservation, BOM awareness
and property-convention following as jv's differentiators, with
`versions-maven-plugin` as the anti-model. That is now wrong on both counts:
Apache does all three, and the anti-model has been superseded. Reproducing
them is table stakes, not an argument.

**What is actually left**, which is narrower and worth being honest about:

1. **Version resolution.** `AddDependencyMojo` contains no version-metadata
   lookup at all — it fails with *"No version specified and no managed version
   found"* unless a BOM covers the artifact. `mvn dependency:add
   -Dgav=org.postgresql:postgresql` is an error; you must know the version
   before you can add it. `jv add org.postgresql:postgresql` resolving the
   latest release is a real gap, and it is the whole reason tier 2 in §1.1 is
   a tier.
2. **Fusing the workflow.** Edit, re-resolve, download, and update the lock in
   one command, versus `dependency:add` → `go-offline` → `tree`.
3. **Latency.** Sub-100 ms against a plugin invocation that pays JVM start,
   plugin resolution and project build before it edits a line.
4. **CLI shape.** `jv add g:a` against `mvn dependency:add -Dgav=g:a:v`.

Design constraints that follow:

- Format-preserving text editing through byte spans, never re-serialising a
  `Model`. This needs source positions `jv-model`'s parser does not retain
  today, and that is the prerequisite work.
- Version selection takes the latest release: never a snapshot, never a version
  a range excludes. `jv add g:a:v` takes the version as given.
- Match the surrounding convention for managed versions and version properties,
  because a POM that does not look hand-written will be rejected in review.
- Atomic write with the pre-image held until resolution succeeds, so a
  dependency that does not resolve leaves no half-edited POM.
- Multi-module: `-f` / `--recursive` select the module; adding at an aggregator
  root is refused with a message naming the modules it could have meant.

`cargo add`'s span-preserving `toml_edit` remains the implementation model.
Where behaviour is ambiguous, match `AddDependencyMojo` — being different from
the plugin in a way users notice is a bug, not a feature.

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

### M5 — `jv tree` + `jv resolve` + differential harness ✅
Text renderer at byte parity (via `maven-dependency-tree` port), json/dot/tgf/
graphml, scope filtering, multi-module.

**Done:** all five output formats, each ported from its upstream visitor;
`jv-driver`, which is where the pure crates meet the machine; and `jv-cli` with
`jv tree` and `jv resolve`. The differential harness
(`crates/jv-cli/tests/mvn_tree_oracle.rs`) runs real Maven 3.9.9 against eight
POMs chosen for resolution behaviours rather than popularity — nearest-wins,
managed transitives, BOM import, exclusion, the scope matrix, optional
dependencies, and a wide graph where conflict ordering decides the outcome —
in every output format. **All 40 fixture/format pairs match.** `text`, `dot` and
`json` byte for byte; `tgf` and `graphml` with node ids renumbered, because
upstream ids them by `Object.hashCode()`, a JVM identity hash that differs
between runs of Maven itself.

Two bugs the harness found that reading the sources had not. `dot` ended with a
newline, because upstream's `endVisit` appends a line separator — but the plugin
writes the visitor's output verbatim and every released version through 3.8.1
produces a file whose last byte is a space. And the harness had been comparing
jv's `json` against Maven's *text*: plugin 3.6.1 silently falls back to text for
an unrecognised `-DoutputType`, which is the same silent fallback jv's own
`Format::from_str` refuses to copy. The pin moved to 3.7.0, which is both what
Maven 3.9.9's super POM selects and the first version implementing json.

Two divergences are recorded rather than hidden. `<repositories>` are scoped per
node by Maven; jv accumulates them into one ordered list, because
`DescriptorSource` has no node context to hang the scoping on. This finds
strictly more artifacts than Maven, never fewer. And graphml and tgf id their
nodes by JVM identity hash upstream, which cannot be reproduced; jv numbers them
sequentially in visit order.

**Open:** the six Ring-3 projects end to end; the harness is built and the
fixtures are synthetic. The benchmark table landed — `scripts/benchmark.sh`,
which refuses to report a time unless the two tools agree first. Warm `jv tree`
is 53ms against Maven's 1,532ms, so the sub-100ms gate is met.

### M6 — `jv sync` + `setup-jv` GitHub Action ✅
Both go-offline passes (§3.8), `_remote.repositories` writing, multi-module
reactor, hardlink materialization.

**The gate is met.** `crates/jv-driver/tests/sync_offline_maven.rs` runs real
Maven 3.9.9 with `--offline` against a repository jv populated and nothing else,
on a project that compiles against Jackson and runs a JUnit 5 test. The build
succeeds, tests included.

Getting there turned up four things that a design on paper would not have:

1. **The lifecycle's plugins are in no POM.** `maven-resources-plugin` and its
   friends come from `default-bindings.xml` inside `maven-core`, so `jv sync`
   needs lifecycle-bindings injection — which is why that gap closed here.
2. **Every POM's parent chain has to travel with it.** Maven re-reads each POM in
   the local repository and walks its parents, so a jar whose grandparent POM is
   absent fails offline even though the jar is right there. jv places every POM
   it read during resolution, which is a superset of any per-artifact parent walk
   and also covers imported BOMs.
3. **Surefire resolves its provider at execution time.** It inspects the test
   classpath and picks `surefire-junit-platform`, `surefire-testng` or another
   from coordinates that appear nowhere. `mvn dependency:go-offline` misses this
   too — a repository it populated fails `mvn -o verify` at the test phase, which
   was confirmed by running it. jv fetches every provider at the plugin's own
   version rather than matching a tool that does not work.
4. **The JUnit Platform launcher is version-aligned to the graph.** Surefire
   matches it to the platform version on the test classpath, so it can only be
   computed after collection.

`_remote.repositories` is written, and `docs/spec/local-repository.md` records
why it is written the way it is. The short version: Maven accepts a file that is
mentioned *nowhere* in the tracking file, and rejects one that is mentioned but
not under a repository the build has configured. So the dangerous state is a
*partial* tracking file, and since a mirrored build's effective repository id is
the mirror's rather than `central`, jv writes the unconditional
locally-installed `<name>>=` form for everything it places.

`action.yml` is the `setup-jv` composite action and `scripts/install.sh` is what
it installs with.

**Snapshots** were the last gap and are closed. The trap is that the metadata a
download produces carries the *effective* repository id in its file name — the
mirror's, when the user has one — which jv cannot know the next `mvn` will be
configured with, and guessing wrong leaves the artifact present and
unresolvable. The way out is not to imitate a download at all: `mvn install`
writes a layout with no repository id anywhere in it, base-version file names
plus a `maven-metadata-local.xml` declaring `<localCopy>true</localCopy>`, and
Maven accepts that from any configuration. jv writes that, which is also honest
— jv put the file there, so it *is* locally installed. Verified end to end
against real Maven with `--offline`.

Finding that required fixing something else: `<activeProfiles>` in
`settings.xml` was parsed and never read, so a profile turned on that way never
activated. Since that is how most people attach a corporate repository, its
artifacts came back "not in any configured repository" with no hint why.

**Ring 3** is `scripts/ring3.sh`: real projects at pinned commits, every module,
`jv tree` diffed against `mvn dependency:tree`. Not a `cargo test` — it clones
gigabytes — so it runs before a release or nightly. Eight projects are pinned,
five of them in the default set, and that set currently reports **46 modules
compared, 0 differing** (spring-petclinic, dropwizard's 42-module reactor,
jackson-databind, commons-lang, maven-dependency-plugin). camel, quarkus and
netty are behind `-a` and have not been run.

**Open:** the `setup-jv` CI-minutes number, which needs a published release and
a real CI run on a public repository.

### M7 — `jvx` ✅
Endpoint parsing, env store, main-class ladder, arg passthrough.

**Done.** `crates/jv-exec/` holds the pure parts — endpoint grammar, manifest
reading, the main-class ladder, version selection — and `jvx` is a second binary
in `jv-cli` over the same argument plumbing as `jv exec`, so the two cannot
drift apart. On Unix the JVM *replaces* the jvx process via `exec`, which
removes a process from the tree and makes signal handling correct.

Two decisions worth recording. The endpoint grammar reads a trailing field as a
main class only when it is a dotted, capitalised Java class name — every
`.`-separated token a Java identifier, the last one uppercase — which rules out
`1.36.1`, `4.1.100.Final` and `natives-linux`. It is stricter than jgo's because
the failure modes are not symmetric: misreading a classifier as a class produces
a wrong resolve and a confusing error seconds later, while refusing to read a
class produces an immediate error naming the `@` spelling. And version selection
computes the greatest non-snapshot, non-prerelease version from the merged
`<versions>` list rather than trusting `<release>`, which is a single string one
repository wrote at deploy time and is frequently absent from mirrors.

**Gate: met.** `jvx com.google.googlejavaformat:google-java-format -- --version`
prints `Version 1.36.1` from a cold cache in 2.7s and from a warm one in 151ms,
having picked the version itself.

**The smoke matrix is green.** `crates/jv-cli/tests/jvx_smoke.rs` runs twenty
real published artifacts covering the shapes that break a launcher: a shaded
uber-jar, a thin jar with a deep transitive classpath, Kotlin and Scala
toolchains, a generator whose usage goes to stderr, three spellings of the
version flag, and tools that exit non-zero when asked to describe themselves.
Half the entries are libraries on purpose — `jvx` is a command people point at
the wrong coordinates, so "refuses clearly and says why" is as much the
behaviour under test as "launches". Each entry records the shape it covers, so a
failure names the class of tool rather than only the tool. Six run by default;
`JV_SMOKE_ALL=1` runs all twenty in 39s warm.

### M8 — v0.1 launch — mechanics done, launch not run
**Done:** the benchmark table, from `scripts/benchmark.sh`, which refuses to
report a time unless jv and Maven agree byte for byte first — 29x warm, 3.5x
cold on the reference machine. `curl | sh` install with checksum verification,
and a release workflow building the four supported targets.

**Open:** Homebrew and binstall, the docs site, and the launch itself.

Original scope, for the record:
README (tree gif as `diff`-proof; honest cold/warm/startup benchmark table),
`curl | sh` + Homebrew + binstall, docs site (own the search landing page),
Show HN "jv: uv for the JVM", deep-dive blog post ("Why Maven has no lockfile —
and how we built one" teasing v0.2), Coursier FAQ entry, then r/java →
newsletters → Kotlin Slack over the following week (different reaction time
constants).
**Gate:** a stranger on a clean machine goes install → `jv tree` wow-moment in
under 60 seconds.

### v0.2 — Real-project compatibility, then the package manager

Sequenced after uv's actual history rather than its final feature set. uv
launched in Feb 2024 with `pip install` / `pip compile` / `pip sync` / `venv`
and *none* of `add`, `lock`, `run`, `uvx` or Python management — a narrow
drop-in whose post-launch priorities were compatibility, performance and
stability. The manager surface arrived about six months later, on top of an
already-trusted primitive. Ordering here follows that: make `jv sync` survive
real projects first, then build on it.

**1. `jv sync` compatibility on real projects.** The launch claim is that jv
resolves what Maven resolves; every gap here is worth more than any new
command. Known missing, verified against the tree:

- `<proxies>` from `settings.xml` — parsed, never applied (env vars work).
- `settings-security.xml` — encrypted passwords are parsed, not decrypted.
- `.mvn/` entirely — no `extensions.xml`, and no `maven.config`, which is the
  quiet one: a project with `-D` flags there resolves differently under jv than
  under `mvn`, with nothing to warn the user.
- Toolchains.

Private repositories, mirrors, authentication, SNAPSHOTs, parent/BOM chains,
multi-module reactors, profile activation and lifecycle plugins already work
and are covered; they need corpus breadth, not new code.

**2. Corpus breadth.** Ring 3's 46 modules are five projects. The target is
hundreds of public Maven projects run continuously as
`jv sync --local-repository … && mvn -o verify`, which tests resolution,
download and the offline guarantee together. Enterprise-shaped projects matter
more here than famous ones.

**3. `jv add` / `jv remove`** (§3.11). Read §3.11 first: Apache shipped
format-preserving add/remove in dependency-plugin 3.11, so the differentiator
is version resolution, workflow fusion and latency — not POM editing.

**4. `jv lock` / `sync --frozen` / `sync --check`** (§3.10). Tier 3 of §1.1:
the only surface with no Maven equivalent, and the one a plugin cannot take
back. Deliberately *after* `sync` and `add` have users, as uv shipped
`pip compile` well before a universal lockfile.

**5. `jv upgrade` / `jv outdated`.** Absorbs versions-maven-plugin, handling
dependencies, plugins, properties and BOMs in one pass.

**6. `jv why <ga>`** (path-recording visitor). Cheap, and a better answer than
`dependency:tree -Dincludes=`.

Carried over: sha256 enforcement mode, `jv purge` / cache GC.

`jv tree --verbose` sits here as a **correctness** item, not a feature. The
oracle harness compares `-Dverbose` against real Maven as of M8, which found
two divergences — one annotation jv emitted and Maven never does, and one
premanaged-version case that turned out to be a resolution bug rather than a
rendering one (§3.9). Few users pass `-Dverbose`; a wrong parity claim is still
worse than a missing one.

### v0.3 — Breadth
Windows support (see §1 non-goals — the largest single enterprise gap, and
still deferred); `jv install` (persistent tool installs à la `uv tool install`
/ coursier `InstallDir`); JPMS module-path launching for `jvx`; Gradle Module
Metadata + variant selection (coursier `GradleModule.scala` /
`VariantSelector.scala`) — the gateway to Kotlin/Android users.

### Later — once there are users to serve
`jv audit` against OSV, and `jv check` (convergence, duplicate classes, JDK
compatibility) absorbing Enforcer. Both reuse the resolved graph, so neither is
hard; both are worth little without adoption. uv shipped `uv audit` more than
two years after launch, and building a security surface before anyone depends
on the resolver would be the same mistake in a different order. JDK management
belongs in this bucket too, for the competitive reason in §1. Beyond that:
remote CAS and the content-addressed store of §3.8, enterprise policy and SBOM
output.

---

## 7. Risks & open questions

| Risk | Mitigation |
|---|---|
| **jv has no high-frequency operation to replace (§1.2)** | The structural risk, not a bug to fix. `./mvnw test` resolves implicitly, so a local developer never feels the cost jv removes. Mitigations, in order: aim at CI, containers and agent sandboxes, where fresh resolution *is* a paid cost; win on tier 2 and tier 3 of §1.1 (fused workflows, and a lockfile Maven has no answer for) rather than on raw speed; and treat any benchmark of an infrequent command as a demo, never as the pitch. |
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
