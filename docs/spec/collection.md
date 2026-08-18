# Dependency collection (the dirty graph) — compatibility specification

> **Provenance.** This document was derived by reading the Apache Maven Resolver and Apache Maven
> sources, which are licensed under the **Apache License, Version 2.0**. No source code is reproduced
> verbatim beyond short identifiers, expressions and literal constants required to specify behaviour.
>
> | Clone | Path | Commit |
> |---|---|---|
> | Maven Resolver | `_reference/maven-resolver` | `ed4a939a850b73d9a85722c277da9de14b64f1e0` |
> | Maven | `_reference/maven` | `945813a7d4d91f32fe92d2c5a81d0a8223bc10b9` |
>
> Primary sources (paths relative to the respective clone root):
>
> | Area | Clone | Path |
> |---|---|---|
> | Shared collector skeleton | resolver | `maven-resolver-impl/src/main/java/org/eclipse/aether/internal/impl/collect/DependencyCollectorDelegate.java` |
> | Breadth-first collector | resolver | `.../internal/impl/collect/bf/BfDependencyCollector.java` |
> | Skip optimization | resolver | `.../internal/impl/collect/bf/DependencyResolutionSkipper.java` |
> | Per-node work item | resolver | `.../internal/impl/collect/bf/DependencyProcessingContext.java` |
> | Depth-first collector | resolver | `.../internal/impl/collect/df/DfDependencyCollector.java` |
> | Managed-state capture | resolver | `.../internal/impl/collect/PremanagedDependency.java` |
> | Interning / memoization | resolver | `.../internal/impl/collect/DataPool.java` |
> | Version filter context | resolver | `.../internal/impl/collect/DefaultVersionFilterContext.java` |
> | Version filter expression parser | resolver | `.../internal/impl/collect/DefaultVersionFilterBuilder.java` |
> | Derivation context | resolver | `.../internal/impl/collect/DefaultDependencyCollectionContext.java` |
> | Type registry cache | resolver | `.../internal/impl/collect/CachingArtifactTypeRegistry.java` |
> | Cycle reporting | resolver | `.../internal/impl/collect/DefaultDependencyCycle.java` |
> | Dependency managers | resolver | `maven-resolver-util/src/main/java/org/eclipse/aether/util/graph/manager/{AbstractDependencyManager,ClassicDependencyManager,TransitiveDependencyManager,DefaultDependencyManager,DependencyManagerUtils}.java` |
> | Selectors (util, deprecated) | resolver | `maven-resolver-util/src/main/java/org/eclipse/aether/util/graph/selector/{And,Exclusion,Optional,Scope,Static}DependencySelector.java` |
> | Selectors (impl, current) | resolver | `maven-resolver-impl/src/main/java/org/eclipse/aether/internal/impl/scope/{Optional,Scope}DependencySelector.java` |
> | Traversers | resolver | `maven-resolver-util/src/main/java/org/eclipse/aether/util/graph/traverser/{Fat,And,Static}*.java` |
> | Version filters | resolver | `maven-resolver-util/src/main/java/org/eclipse/aether/util/graph/version/*.java` |
> | Artifact id helpers | resolver | `maven-resolver-util/src/main/java/org/eclipse/aether/util/artifact/ArtifactIdUtils.java` |
> | Narrative docs | resolver | `src/site/markdown/{how-resolver-works,transitive-dependency-resolution,common-misconceptions}.md` |
> | POM → descriptor bridge | maven | `impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/DefaultArtifactDescriptorReader.java` |
> | Model dep → aether dep | maven | `impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/ArtifactDescriptorUtils.java` |
> | Relocation | maven | `impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/RelocatedArtifact.java`, `.../resolver/relocation/{DistributionManagement,UserProperties}ArtifactRelocationSource.java` |
> | Session defaults (Maven 4) | maven | `impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/MavenSessionBuilderSupplier.java` |
> | Session defaults (Maven 3 lineage) | maven | `compat/maven-resolver-provider/src/main/java/org/apache/maven/repository/internal/{MavenRepositorySystemUtils,MavenSessionBuilderSupplier}.java` |
> | Collect request construction | maven | `impl/maven-core/src/main/java/org/apache/maven/project/DefaultProjectDependenciesResolver.java` |

## How to read this document

Scope: **collection only** — building the raw ("dirty") dependency graph. Conflict resolution,
scope derivation, flattening, and artifact download are *out of scope*; they are performed by the
`DependencyGraphTransformer` chain that `collectDependencies` runs after collection, and are
specified elsewhere.

The Rust implementation targets **Maven 3.9 behaviour** with the **breadth-first (BF)** collector.
Every place where the Maven 4 code in these clones differs from the Maven 3 lineage is flagged
**`[M3≠M4]`** inline and collected in [§13](#13-maven-39-vs-maven-4-divergences).

### Depth conventions — read this first

Three independent depth counters appear in the sources. They do **not** agree. Getting them mixed up
is the single most common way to produce a subtly different graph.

| Counter | Lives in | 0 | 1 | 2 | 3 |
|---|---|---|---|---|---|
| **Manager depth** | `AbstractDependencyManager.depth` | session/factory instance | instance that manages the **root's direct dependencies** | instance that manages **children of direct dependencies** | … |
| **Selector depth** | `OptionalDependencySelector.depth`, `ScopeDependencySelector.depth` | session instance | instance that selects **direct dependencies** | instance that selects **their children** | … |
| **Skipper depth** | `parents.size() + 1` in `DependencyResolutionSkipper` | — | the root node (never assigned a coordinate) | **direct dependencies** | their children |

Throughout this document, "**graph level 1**" means the direct dependencies of the root node,
"graph level 2" their dependencies, and so on. Mapping: graph level *n* ⇒ manager depth *n*,
selector depth *n*, skipper depth *n+1*.

---

## 1. The collect request and its defaults

### 1.1 Request fields

`CollectRequest` carries:

| Field | Meaning |
|---|---|
| `rootArtifact` | Artifact for the root node when there is **no** root dependency. The root node then has `getDependency() == null`. |
| `root` | A `Dependency` for the root node. Mutually exclusive in practice with using only `rootArtifact`; if both are set, `root` wins for graph construction and `rootArtifact` is used only in the initial derivation context. |
| `dependencies` | The initial (level-1) dependency list. |
| `managedDependencies` | The initial dependency-management list. |
| `repositories` | Remote repositories. |
| `requestContext` | String stamped onto every node (`"project"` for Maven project resolution). |
| `resolutionScope` | Optional; when non-`null`, the session's selector is **replaced** by `scopeManager.getDependencySelector(session, resolutionScope)` and `scopeManager.postProcess` runs at the end. |
| `trace` | Diagnostics only; no effect on the graph. |

**Maven's project resolution** (`DefaultProjectDependenciesResolver`) sets `rootArtifact` (never
`root`), `requestContext = "project"`, `dependencies` from the effective POM's `<dependencies>`
(skipping entries with a blank groupId/artifactId/version), and `managedDependencies` from the
effective POM's `<dependencyManagement>` (skipping entries whose groupId/artifactId/version still
contain `${`). It also sets `aether.dependencyManager.verbose = true` when debug logging is on.

### 1.2 Root handling (`DependencyCollectorDelegate.collectDependencies`)

```
session  := copy(session) with ArtifactTypeRegistry wrapped in CachingArtifactTypeRegistry
            and, if request.resolutionScope != null, selector replaced by the scope manager's
```

**If `request.root != null`:**

1. Resolve the version range of `root.artifact`; on failure, add the exception and **throw
   `DependencyCollectionException` immediately** (no graph).
2. `versions := filterVersions(root, rangeResult, session.versionFilter, new DefaultVersionFilterContext(session))`.
3. `version := versions.get(versions.size() - 1)` — **the last (highest) candidate. The root becomes
   exactly one node even for a range.**
4. `root := root.setArtifact(root.artifact.setVersion(version))`.
5. Read the artifact descriptor, unless `isLackingDescriptor` (see §10.4), in which case an empty
   `ArtifactDescriptorResult` is synthesized. Apply all registered `ArtifactDecorator`s to
   `descriptorResult.artifact`. On `ArtifactDescriptorException`, **throw immediately**.
6. `root := root.setArtifact(descriptorResult.artifact)` — relocation applies to the root too.
7. Unless `session.isIgnoreArtifactDescriptorRepositories()`, aggregate
   `request.repositories` with `descriptorResult.repositories` (request dominant).
8. `dependencies := mergeDeps(request.dependencies, descriptorResult.dependencies)`,
   `managedDependencies := mergeDeps(request.managedDependencies, descriptorResult.managedDependencies)`.
9. Root node fields: dependency = `root`; `requestContext`; `relocations` =
   `descriptorResult.relocations`; `versionConstraint` = `rangeResult.versionConstraint`;
   `version`; `aliases` = `descriptorResult.aliases`; **`repositories` = `request.getRepositories()`**
   (the *un*aggregated list — a deliberate asymmetry with step 7).

**Else (`rootArtifact` only):** node = `DefaultDependencyNode(request.rootArtifact)` with
`requestContext` and `repositories = request.repositories`. `dependencies` and
`managedDependencies` are used exactly as given; no descriptor is read for the root.

`mergeDeps(dominant, recessive)`: if `dominant` is null/empty return `recessive` (same list
object); if `recessive` is null/empty return `dominant`; otherwise emit all of `dominant` in order,
then those entries of `recessive` whose key is not already present. **Key = `groupId:artifactId:classifier:extension`** (note: classifier *before* extension here, unlike `ArtifactIdUtils`).

Then:

```
traverse := (root == null) || (traverser == null) || traverser.traverseDependency(root)
if traverse && !dependencies.isEmpty():
    doCollectDependencies(...)          # §2, the BF algorithm
apply DependencyGraphTransformer chain  # out of scope
if errorPath != null: throw DependencyCollectionException(result, "Failed to collect dependencies at " + errorPath)
if !result.exceptions.isEmpty(): throw DependencyCollectionException(result)
```

The whole body runs inside a `while (!finished)` loop driven by
`session.getDependencyCollectionChecker()` (default `DependencyCollectionChecker.NOOP` ⇒ exactly one
run). `aether.dependencyCollector.maxRuns` (default 5) caps it. A Rust port targeting Maven can
implement a single run.

### 1.3 Extension points Maven installs

`MavenSessionBuilderSupplier.configureSessionBuilder` (Maven 4) / `MavenRepositorySystemUtils.newSession`
(Maven 3 lineage):

| Extension point | Maven 3.9 target | Maven 4 default |
|---|---|---|
| `DependencyTraverser` | `FatArtifactTraverser` | `FatArtifactTraverser` |
| `DependencyManager` | **`ClassicDependencyManager`** | `TransitiveDependencyManager` **`[M3≠M4]`** |
| `DependencySelector` | `AndDependencySelector(…)` — see below | same composition |
| `VersionFilter` | **none** (`null`) | **none** unless `maven.session.versionFilter` is set **`[M3≠M4]`** |
| `ArtifactDescriptorPolicy` | `SimpleArtifactDescriptorPolicy(true, true)` | same |
| `ArtifactTypeRegistry` | Maven's type registry (wrapped in `CachingArtifactTypeRegistry` by the collector) | same |
| `ScopeManager` | — | `ScopeManagerImpl(Maven3ScopeManagerConfiguration)` when `maven.maven3Personality=true`, else `Maven4ScopeManagerConfiguration` |

**Exact selector composition order** (`AndDependencySelector` preserves insertion order in a
`LinkedHashSet`; `selectDependency` short-circuits on the first `false`):

```
AndDependencySelector(
    ScopeDependencySelector.legacy(/*included=*/ null, /*excluded=*/ ["test", "provided"]),
    OptionalDependencySelector.fromDirect(),          #  == from(2)
    ExclusionDependencySelector()                     #  starts with zero exclusions
)
```

`maven.maven3Personality` (`Constants.MAVEN_MAVEN3_PERSONALITY`) defaults to `false`; setting it to
`true` switches the manager to `ClassicDependencyManager` and the scope model to the Maven 3 one.

### 1.4 Collector selection and configuration properties

| Property | Default | Effect |
|---|---|---|
| `aether.dependencyCollector.impl` | `bf` | `bf` or `df` |
| `aether.dependencyCollector.bf.skipper` | `versionless` | `versionless` (key = `G:A:E[:C]`), `versioned` (key = `G:A:E[:C]:V`), `false` (never skip) |
| `aether.dependencyCollector.bf.threads` | 5 | descriptor prefetch parallelism; **must not** affect the graph |
| `aether.dependencyCollector.maxExceptions` | 50 | exceptions beyond this are swallowed (negative = unlimited) |
| `aether.dependencyCollector.maxCycles` | 10 | cycles beyond this are not reported (negative = unlimited) |
| `aether.dependencyCollector.maxRuns` | 5 | re-collection attempts |
| `aether.dependencyManager.verbose` | `false` | records premanaged state on nodes (§5.5) |
| `aether.dependencyCollector.pool.internArtifactDescriptorManagedDependencies` | `true` | interns managed-dependency lists — matters because the manager memoizes by list *identity* |
| `aether.dependencyCollector.pool.internArtifactDescriptorDependencies` | `false` | — |
| `maven.session.versionFilter` | unset | version filter expression (§7.2) |
| `maven.relocations.entries` | unset | user relocations (§8.4) |

---

## 2. The BF algorithm

### 2.1 What a processing context carries

`DependencyProcessingContext` is the queue element. It is a *work item for one not-yet-created node*:

| Field | Content |
|---|---|
| `depSelector` | selector to apply to this dependency's **children** and already applied to this dependency |
| `depManager` | manager used to build `premanagedDependency` and to derive the child manager |
| `depTraverser` | traverser asked whether to descend past this dependency |
| `verFilter` | version filter for this dependency's range |
| `trace` | diagnostics |
| `repositories` | repositories in effect for resolving this dependency |
| `managedDependencies` | the `<dependencyManagement>` list of the *parent* descriptor (informational; the manager was already derived from it) |
| `parents` | the full path of `DependencyNode`s from the **root node (index 0)** down to the immediate parent, inclusive. `getParent()` = last element. |
| `dependency` | mutable: starts as the declared dependency, is replaced by the managed one before enqueueing, and again by the version/relocation-resolved one during processing |
| `premanagedDependency` | the `PremanagedDependency` computed at enqueue time (§5.5) |

`withDependency(d)` mutates in place and returns `this`. `copy()` produces a shallow copy sharing
`parents`.

### 2.2 The algorithm, step by step

Queue = FIFO (`ArrayDeque`). One shared `DataPool`, one shared skipper, one shared descriptor
prefetch map keyed by `ArtifactIdUtils.toId(artifact)` = `G:A:E[:C]:V`.

1. **Pick the skipper.** From `aether.dependencyCollector.bf.skipper`: `versionless` ⇒ key function
   `toVersionlessId` (`G:A:E[:C]`), `versioned` ⇒ `toId`, `false` ⇒ the never-skipper. Anything else
   is a hard configuration error.

2. **Derive the level-1 extension points.** Build
   `ctx0 = DependencyCollectionContext(session, request.rootArtifact, request.root /*may be null*/, managedDependencies)`
   and derive `rootDepSelector`, `rootDepManager`, `rootDepTraverser`, `rootVerFilter` from the
   session instances by calling their `deriveChildXxx(ctx0)` once each (skipping any that is `null`).
   Note `ctx0.getArtifact()` is `root.getArtifact()` when `root != null`, else `request.rootArtifact`.

3. **Seed the queue.** `parents = [rootNode]`. For each dependency in the merged direct-dependency
   list, **in list order**:
   * if `rootDepSelector != null && !rootDepSelector.selectDependency(dep)` → drop it silently;
   * `pm = PremanagedDependency.create(rootDepManager, dep, disableVersionManagement = false, verbose)`;
   * build the context with the four level-1 extension points, `repositories`, the merged
     `managedDependencies`, `parents`, `dep`, `pm`; then `withDependency(pm.getManagedDependency())`;
   * start asynchronous descriptor resolution for `ctx.dependency.artifact` (§2.3) — deduplicated by
     `G:A:E[:C]:V`, so the *first* requester's context supplies the repositories/version-filter used;
   * enqueue the context.

4. **Drain the queue.** While non-empty: `processDependency(queue.remove(), relocations = [],
   disableVersionManagement = false)`. Steps 5–12 are that procedure. Because children are enqueued
   rather than recursed into, the graph is built level by level, and within a level in left-to-right
   declaration order.

5. **Traversal decision.** `noDescriptor := isLackingDescriptor(dependency.artifact)` (§10.4);
   `traverse := !noDescriptor && (depTraverser == null || depTraverser.traverseDependency(dependency))`.
   Both are computed from `ctx.dependency` — i.e. **after** dependency management, **before** version
   and relocation resolution.

6. **Await the descriptor result.** Look up the prefetch future for `dependency.artifact` and block on
   it. If it throws (including a wrapped `VersionRangeResolutionException`), record the exception
   against `ctx.parents` (§10.1) and **return without creating any node**.

7. **Iterate candidate versions.** The result holds an ordered `Version → ArtifactDescriptorResult`
   map. For a plain version it has one entry. **For a range it holds every accepted version, ordered
   newest-first** (BF reverses the ascending list to maximize skipper hits). For each entry, in that
   order: `d := dependency.setArtifact(dependency.artifact.setVersion(version))`; remember
   `originalArtifact := d.artifact`.

8. **Missing descriptor.** If the map's value for this version is `null` (the descriptor could not be
   read and the policy did not tolerate it), append a node built from `d` with **no aliases and no
   children** to `ctx.getParent().getChildren()` and continue with the next version. No interning, no
   cycle check, no expansion.

9. **Adopt the descriptor artifact and check for a cycle.** `d := d.setArtifact(descriptorResult.getArtifact())`.
   Then `cycleEntry := find(ctx.parents, d.artifact)` (§10). If `cycleEntry >= 0`: record the cycle;
   if `ctx.parents[cycleEntry].getDependency() != null`, append a node for `d` whose **children list
   is the ancestor's children list object** and continue with the next version. (If the matching
   ancestor is a dependency-less root node, fall through and process normally.)

10. **Relocation.** If `descriptorResult.getRelocations()` is non-empty:
    * re-test `d` against `ctx.depSelector` (a relocation can move a dependency into an excluded
      coordinate); if rejected, `return`;
    * `disableVersionManagementSubsequently := originalArtifact.groupId == d.artifact.groupId &&
      originalArtifact.artifactId == d.artifact.artifactId`;
    * build a fresh `PremanagedDependency` from `ctx.depManager`, `d` and that flag, and a fresh
      context that is identical to `ctx` except `managedDependencies = descriptorResult.getManagedDependencies()`,
      `dependency = pm.getManagedDependency()`;
    * start descriptor resolution for it and **recurse into `processDependency` inline** with
      `relocations = descriptorResult.getRelocations()` and the flag;
    * **`return`** — the remaining candidate versions of the outer loop are abandoned.

11. **Create the node.** Intern `d` and its artifact. `repos := getRemoteRepositories(rangeResult.getRepository(version), ctx.repositories)`
    — a single-element list when the version came from a specific remote repository, an **empty**
    list when it came from a non-remote (e.g. local/workspace) repository, and `ctx.repositories`
    when the range result records no repository. Build the node with: dependency `d`,
    `premanaged.applyTo(node)` (§5.5), `relocations`, `versionConstraint` from the range result,
    `version`, `aliases` from the descriptor, `repos`, `requestContext`. Append to
    `ctx.getParent().getChildren()`.

12. **Expand or register.**
    `recurse := traverse && !descriptorResult.getDependencies().isEmpty()`.
    * If `recurse` → `doRecurse` (§2.4).
    * Else → `if (!skipper.skipResolution(node, ctx.parents)) skipper.cache(node, ctx.parents + [node])`.
      A childless node therefore still participates in the skipper's winner bookkeeping.

### 2.3 Asynchronous descriptor resolution

`resolveArtifactDescriptorAsync(ctx)` registers, under key `toId(ctx.dependency.artifact)`, a task
that:

1. resolves the version range for `ctx.dependency` (memoized in the `DataPool` by
   *artifact + repositories + request context*);
2. `versions := filterVersions(dependency, rangeResult, ctx.verFilter, versionContext)` (§8);
3. **reverses** `versions` in place (newest first);
4. for each version, resolves the descriptor (in parallel when there is more than one version),
   storing `null` for versions whose descriptor could not be read;
5. builds an insertion-ordered map in the reversed order;
6. if the range produced more than one version, additionally registers each
   `(version, descriptor)` pair under its own `G:A:E[:C]:V` key so that a later exact-version request
   is served from cache.

Registration is *first-wins* (`computeIfAbsent`): if two different paths reach the same
`G:A:E[:C]:V` with different repository lists or version filters, **the first one to register
determines the result for both.** Threads only affect latency, never content.

### 2.4 `doRecurse` — deriving the child level and enqueueing grandchildren

Given the just-created node `child` and its context:

1. `ctx' := DependencyCollectionContext(session, d.artifact, d, descriptorResult.getManagedDependencies())`
   where `d` is the **post-management, post-relocation, post-version** dependency.
2. `childSelector := parent.depSelector?.deriveChildSelector(ctx')`,
   `childTraverser := …deriveChildTraverser(ctx')`, `childFilter := …deriveChildFilter(ctx')`.
3. **Speculative pool probe.** `speculativeKey := (d.artifact, childRepos, childSelector,
   parentManager, childTraverser, childFilter)` where
   `childRepos := ignoreRepos ? parent.repositories : aggregate(parent.repositories, descriptorResult.repositories)`.
   If the pool has children for that key → `child.setChildren(pooledList)` and **return**. (The
   speculative key is correct whenever `deriveChildManager` returns `this`, which is the common case.)
4. `childManager := parent.depManager?.deriveChildManager(ctx')`. If it is not the same instance as
   the parent manager, recompute the key with `childManager` and probe the pool again; on a hit,
   `setChildren` and return.
5. **Only now** consult the skipper: `if (skipper.skipResolution(child, parent.parents)) return;`
   — the node keeps its empty children list, nothing is put in the pool, nothing is cached.
6. Otherwise `parents' := parent.parents + [child]`, and for each dependency of the descriptor **in
   declaration order**:
   * skip it if `childSelector` rejects it;
   * `pm := PremanagedDependency.create(childManager, dep, disableVersionManagement, verbose)`;
   * build a context with `childSelector/childManager/childTraverser/childFilter`, `childRepos`,
     `descriptorResult.getManagedDependencies()`, `parents'`, `dep`, `pm`, then
     `withDependency(pm.getManagedDependency())`;
   * start descriptor resolution and enqueue.
7. `pool.putChildren(key, child.getChildren())` — the **still-empty, mutable list object** is stored;
   it is filled in later as the queued items are processed, and every subsequent pool hit shares
   that same list.
8. `skipper.cache(child, parents')`.

> **Pool before skipper.** A duplicate whose full graph key matches an already-expanded node is
> served from the pool and *never reaches the skipper*. The skipper only sees nodes whose graph key
> is new — typically because the accumulated exclusions, repositories or manager differ.

The `GraphKey` compares `(artifact, repositories, selector, manager, traverser, filter)` by
`equals`. **All five extension points must therefore have value equality**, and the manager's
`equals` deliberately excludes `managedLocalPaths`.

---

## 3. `DependencyResolutionSkipper` — exact rules

State (all per collection run):

| Structure | Key | Value |
|---|---|---|
| `results` | `DependencyNode` (**identity**) | `DependencyResolutionResult { skippedAsVersionConflict, skippedAsDuplicate, resolve, forceResolution }` |
| `winners` | `Artifact` (**full equality**: groupId, artifactId, version, extension, classifier, path/file, properties) | winning `DependencyNode` |
| `winnerGAs` | `keyFunction(artifact)` — `G:A:E[:C]` by default, `G:A:E[:C]:V` in `versioned` mode | the winning `Artifact` |
| `sequenceGen` | depth (int) | monotonically increasing counter |
| `coordinateMap` | `DependencyNode` (identity) | `Coordinate { depth, sequence }` |
| `leftmostCoordinates` | `Artifact` (full equality) | `Coordinate` of the most recently *resolved* node for that artifact |

### 3.1 `skipResolution(node, parents) -> bool`

```
result := new DependencyResolutionResult(node);  results[node] := result
depth  := parents.size() + 1                      # root == 1, direct dependency == 2
coordinate[node] := Coordinate(depth, ++sequenceGen[depth])

if isVersionConflict(node):        result.skippedAsVersionConflict := true
elif isDuplicate(node):
    if isLeftmost(node, parents):  result.forceResolution := true
    else:                          result.skippedAsDuplicate  := true
else:                              result.resolve := true

if result.resolve || result.forceResolution:
    leftmostCoordinates[node.artifact] := coordinate[node]
    return false          # do resolve
return true               # skip
```

* `isVersionConflict(node)` — `winnerGAs` contains `keyFunction(node.artifact)` **and** the stored
  winner's `version` differs from the node's `version`. In `versioned` mode the key includes the
  version, so this predicate is always false and only the duplicate rule fires.
* `isDuplicate(node)` — `winners` contains `node.artifact` under full artifact equality. Two nodes
  for the same GAV but with different `properties` (e.g. one carries `localPath`) are **not**
  duplicates.
* `isLeftmost(node, parents)`:
  ```
  lm := leftmostCoordinates[node.artifact]
  if lm == null || lm.depth > parents.size(): return false
  ancestor := parents[lm.depth - 1]     # node's own ancestor at skipper-depth lm.depth
  return coordinate[ancestor].sequence < lm.sequence
  ```
  `parents[0]` is the root node; it is never given a coordinate, but it can never be selected here
  because `lm.depth >= 2` for every node the skipper has ever seen. The condition
  `lm.depth <= parents.size()` also guarantees the recorded winner is **strictly shallower** than
  `node`.

**Plain-language rules.**

| Situation | Outcome | Effect on the graph |
|---|---|---|
| A different version of the same `G:A:E[:C]` already won | **skip** (`skippedAsVersionConflict`) | node exists as a childless leaf; conflict resolution will mark it "omitted for conflict" |
| The exact same artifact already won, and this node's path is **not** to the left of the winner's | **skip** (`skippedAsDuplicate`) | node exists as a childless leaf; "omitted for duplicate" |
| The exact same artifact already won, but this node branches off to the **left** of the winner at the winner's depth | **force resolution** | subtree is expanded again — needed because Maven picks the widest scope among conflicting nodes, and the leftmost path must retain its scope information |
| Nothing seen before | **resolve** | subtree expanded, node becomes the winner |

"Leftmost" is measured by the per-depth sequence counter, which increments in BFS processing order.
Because the queue is FIFO and each level is enqueued in declaration order, a smaller sequence at a
given depth means "earlier in the level-order walk".

### 3.2 `cache(node, parents)`

```
if any n in parents has results[n].forceResolution:   do nothing
else:                                                 winners[node.artifact] := node
                                                      winnerGAs[key(node.artifact)] := node.artifact
```

`parents` here is `parentContext.parents + [node]` — **the node itself is included**. Therefore:

* a **force-resolved** node never becomes a winner (it is a known duplicate);
* nothing in the subtree of a force-resolved node ever becomes a winner either, so the force-resolved
  subtree does not displace the original winner's bookkeeping.

### 3.3 Consequences a naive implementation gets wrong

1. `skipResolution` **mutates** state (it always assigns a coordinate and always records a result)
   even when it returns `true`. It must be called exactly once per node, in BFS order.
2. It is consulted **after** the `DataPool` graph-key probe in `doRecurse`, and it is also consulted
   for childless nodes in step 12 of §2.2. Skipping either call site changes the winner set.
3. A skipped node is still **present in the graph**, with an empty children list; it is not removed.
4. The skipper is *only* in the BF collector. The DF collector has no equivalent, which is why the
   two produce different dirty graphs (§12).
5. `winners` is keyed by full artifact equality including `properties`; `winnerGAs` by the versionless
   id. Using one key for both breaks the conflict/duplicate distinction.

---

## 4. Traversers

`FatArtifactTraverser.traverseDependency(dep)`:

```
prop := dep.getArtifact().getProperty("includesDependencies", "")
return !Boolean.parseBoolean(prop)          # case-insensitive "true" only
```

* Property name: `ArtifactProperties.INCLUDES_DEPENDENCIES` = **`"includesDependencies"`**.
* It comes from the artifact **type** (`ArtifactType.getProperties()`), which is merged into the
  artifact's properties by `DefaultArtifact(…, ArtifactType)`. Maven's `DefaultType` sets it from
  the type descriptor; the standard types with `includesDependencies = true` are the "fat" ones
  (`war`, `ear`, `rar`, `par`, uber-jars declared as such, …).
* `deriveChildTraverser` returns `this` — it is depth-independent and stateless.
* Returning `false` sets `traverse = false`, so the node is created but **its children are never
  expanded**. The descriptor is still read (the node needs its artifact, aliases and relocations).

`AndDependencyTraverser` — logical AND, same derive/collapse structure as `AndDependencySelector`
(§6.5). `StaticDependencyTraverser(bool)` — constant.

Independently of the traverser, `isLackingDescriptor` (a `system`-scoped artifact, §10.4) forces
`traverse = false`.

---

## 5. `ClassicDependencyManager` — exact depth semantics

### 5.1 Parameters

`ClassicDependencyManager(scopeManager)` ⇒ `AbstractDependencyManager(deriveUntil = 2, applyFrom = 2)`,
`depth = 0`, all management maps `null`.

* `isDerived()` ⇔ `depth < deriveUntil` ⇔ `depth < 2` — *may collect rules from this context*.
* `isApplied()` ⇔ `depth >= applyFrom` ⇔ `depth >= 2` — *may apply version/scope/optional/system-path*.
* `isInheritedDerived()` for `Classic` = `isDerived()`, so scope and optional are collected wherever
  versions are.

### 5.2 The derivation chain

| Call | Instance before | What happens | Instance after |
|---|---|---|---|
| session → level-1 manager | depth 0, empty | `depth != 1` ⇒ `super.deriveChildManager`; `isDerived()` true ⇒ **collect the root's `managedDependencies`** into new maps | **depth 1**, carrying the root's rules in its *own* maps |
| level-1 → level-2 manager | depth 1 | `depth == 1` ⇒ the **MNG-4720 "hop"**: `newInstance(managedVersions, managedScopes, managedOptionals, managedLocalPaths, managedExclusions)` — the *same* maps are passed straight through, and the direct dependency's own `<dependencyManagement>` is **ignored entirely** | **depth 2**, own maps = root's rules, parent = the depth-1 instance (whose maps become the ancestor layer) |
| level-2 → level-3 and deeper | depth 2 | `depth != 1` ⇒ `super.deriveChildManager`; `isDerived()` is now **false** ⇒ `return this` | **the same depth-2 instance**, forever |

(The reuse shortcut in `AbstractDependencyManager.deriveChildManager` — "return `this` when nothing
new was collected" — cannot fire for `Classic` at depth 0, because it additionally requires
`isApplied()`, which is false there. So a depth-1 instance is always created, even for a root with an
empty `<dependencyManagement>`. The depth-1 hop bypasses the memoization ring buffer entirely, so a
distinct depth-2 object is allocated per expanded direct dependency; they all compare `equals`
because they share the same maps and the same parent object, which is what keeps the `DataPool`
graph key stable. `MMap` implements value-based `equals`/`hashCode`, and `AbstractDependencyManager`
excludes `managedLocalPaths` from both.)

Consequences, stated exactly:

* **`ClassicDependencyManager` collects dependency-management rules from exactly one place: the root
  context.** No intermediate POM's `<dependencyManagement>` is ever collected. (This is Maven 2.x /
  Maven 3.x behaviour: "only obeys root management".)
* **Management begins to apply at graph level 2** — the dependencies *of* the direct dependencies —
  because the manager that sees them has `depth == 2` and `applyFrom == 2`. **Exception:** managed
  *exclusions* apply from graph level 1 (§5.4).
* Because deeper derivation returns `this`, the depth-2 instance is reused for the whole rest of the
  graph. A Rust port must ensure the reused manager is *equal* (not merely equivalent) so the
  `DataPool` graph key matches; the level-1 hop creates a fresh instance per direct dependency, but
  all such instances are `equals` (same depth, same maps, same parent).

### 5.3 Why a POM's own `<dependencyManagement>` does not manage its own direct dependencies

The model builder has already applied a POM's `<dependencyManagement>` to that POM's own
`<dependencies>` when it produced the effective POM. Re-applying the same rules in the resolver would
be redundant at best and would override the model builder's work at worst (the model builder's rules
include import-scope BOM flattening and interpolation the resolver cannot see). `applyFrom = 2`
encodes "apply only rules that came from *strictly above* the node being managed":

* the depth-1 manager holds the root's rules but has `isApplied() == false` ⇒ the root's own
  `<dependencyManagement>` is *not* re-applied to the root's own `<dependencies>`;
* the depth-2 manager holds the root's rules in its ancestor layer and has `isApplied() == true` ⇒
  the root's rules *do* reach transitive dependencies, which is exactly what the model builder could
  not do.

The `depth == 1` special case inside `getManagedVersion` / `getManagedScope` / `getManagedOptional`
(consulting the instance's *own* maps in addition to the ancestor layers) exists only for
`DefaultDependencyManager`, whose `applyFrom` is 0. It is unreachable for `Classic` and `Transitive`.

### 5.4 What is managed, and how rules are recorded

Rules are keyed by **GACE** (`groupId : artifactId : extension : classifier`) — no version.

Collection (`deriveChildManager`, only while `isDerived()`), iterating the context's managed
dependency list in order:

| Rule | Collected when | Precedence |
|---|---|---|
| version | `artifact.version` non-empty and no version rule for that key exists yet (own maps, ancestor layers, or the in-progress map) | **first wins** ⇒ nearest-to-root wins |
| scope | `isInheritedDerived()` and `scope` non-empty and not already present | first wins |
| optional | `isInheritedDerived()` and `getOptional() != null` (tri-state — absent `<optional>` is `null`, not `false`) and not already present | first wins |
| local path | `systemDependencyScope.getSystemPath(artifact) != null` and not already present | first wins |
| exclusions | `!exclusions.isEmpty()` | **additive** — appended to any existing entry for the same key at this level, and unioned across levels |

Application (`manageDependency(dep)`), returning a `DependencyManagement` or `null`:

| Field | Applied when |
|---|---|
| `version` | `isApplied()` and a rule exists in an ancestor layer |
| `scope` | `isApplied()` and a rule exists. Additionally, if the new scope is **not** the system scope but the artifact still carries a `localPath` property, the properties are copied with `localPath` **removed** and set as `management.properties` |
| `properties` (system path) | `isApplied()` and (the managed scope is the system scope, or there is no managed scope and the dependency's own scope is the system scope) and a managed local path exists ⇒ properties copied with `localPath` set. This aligns the system path of the same artifact across the whole graph |
| `optional` | `isApplied()` and a rule exists |
| `exclusions` | **outside the `isApplied()` guard** — always applied. Value = `LinkedHashSet(dependency.getExclusions())` ∪ (all layers' exclusions for the key). Order: the dependency's own exclusions first, then ancestor layers oldest-first, then this level's |

So at graph level 1 under `Classic`, a direct dependency can only be modified by **managed
exclusions**; its version, scope and optional flag pass through untouched.

### 5.5 `PremanagedDependency` — recording the managed state

`PremanagedDependency.create(manager, dependency, disableVersionManagement, verbose)`:

```
depMngt := manager?.manageDependency(dependency)
bits := 0
if depMngt != null:
    if depMngt.version != null && !disableVersionManagement:
        premanagedVersion := dependency.artifact.version
        dependency := dependency with artifact.version = depMngt.version
        bits |= MANAGED_VERSION      (0x01)
    if depMngt.properties != null:
        premanagedProperties := dependency.artifact.properties
        dependency := dependency with artifact.properties = depMngt.properties
        bits |= MANAGED_PROPERTIES   (0x08)
    if depMngt.scope != null:
        premanagedScope := dependency.scope
        dependency := dependency.setScope(depMngt.scope)
        bits |= MANAGED_SCOPE        (0x02)
    if depMngt.optional != null:
        premanagedOptional := dependency.isOptional()
        dependency := dependency.setOptional(depMngt.optional)
        bits |= MANAGED_OPTIONAL     (0x04)
    if depMngt.exclusions != null:
        premanagedExclusions := dependency.exclusions
        dependency := dependency.setExclusions(depMngt.exclusions)
        bits |= MANAGED_EXCLUSIONS   (0x10)
```

The order matters: version is applied before properties, so `premanagedProperties` is captured from
the already-version-managed artifact.

`applyTo(node)` always sets `node.managedBits = bits`. When `aether.dependencyManager.verbose` is
`true` it additionally stores, in the node's custom data map:

| Data key | Value |
|---|---|
| `premanaged.version` | original version, or `null` |
| `premanaged.scope` | original scope, or `null` |
| `premanaged.optional` | original optional flag (`Boolean`), or `null` |
| `premanaged.exclusions` | original exclusions (unmodifiable copy), or `null` |
| `premanaged.properties` | original artifact properties (unmodifiable copy), or `null` |

`DependencyManagerUtils.getPremanagedXxx(node)` returns `null` unless the corresponding managed bit
is set *and* verbose mode stored the value. Maven turns verbose mode on automatically when debug
logging is enabled, which is why `mvn -X dependency:tree` shows "version managed from …".

`disableVersionManagement` is set only by the relocation path (§8.5) and, **in BF only**, propagates
to the relocated node's children (§12).

---

## 6. Selectors

A selector is asked `selectDependency(dep)` for each *declared* dependency of a node — before
dependency management is applied — and a **child selector is derived once per expanded node**, from
`DependencyCollectionContext(session, d.artifact, d, descriptorResult.managedDependencies)` where `d`
is the fully resolved (managed, relocated, versioned) dependency of that node.

### 6.1 `ExclusionDependencySelector` — exclusion accumulation

* State: a sorted, duplicate-free `Exclusion[]`, ordered by `(artifactId, groupId, extension, classifier)`.
* `selectDependency(dep)` → `false` iff any exclusion matches the artifact on **all four** of
  `artifactId`, `groupId`, `extension`, `classifier`, where the pattern `"*"` matches anything and
  any other pattern must match exactly. There is no glob/prefix matching.
* `deriveChildSelector(ctx)` merges `ctx.getDependency().getExclusions()` into the sorted array. If
  the dependency is `null` or has no exclusions, **`this` is returned** (identity preserved — this
  matters for the `DataPool` graph key).
* Exclusions therefore **accumulate monotonically down a path** and are never removed. Because the
  context dependency is the post-management one, **managed exclusions** (§5.4) participate.
* Maven's `<exclusion>` elements are converted to `Exclusion(groupId, artifactId, "*", "*")` by
  `ArtifactDescriptorUtils.convert`, i.e. classifier and extension are always wildcards.

### 6.2 `OptionalDependencySelector` (impl variant, the one Maven installs)

Constructed as `OptionalDependencySelector.fromDirect()` = `from(2)` ⇒ `depth = 0`, `applyFrom = 2`.

```
selectDependency(dep) = (depth < applyFrom) || !dep.isOptional()
deriveChildSelector(ctx) = (depth >= applyFrom) ? this : new(depth + 1, …)
```

| Selector depth | Selects | Meaning |
|---|---|---|
| 0 | session instance, never used to select | — |
| 1 | everything | **optional direct dependencies of the root are kept** |
| ≥ 2 | non-optional only | **optional dependencies are dropped from graph level 2 onward** |

The derived instances at depth ≥ 2 return `this`, so the selector stabilizes and the graph key stays
stable. The optional bit tested is `dep.isOptional()` on the *declared* dependency — so an
`<optional>` value injected by dependency management does **not** affect selection at level 1
(management is applied after selection there) but does affect the node's recorded optional flag.

The deprecated `util` variant is behaviourally identical for the default configuration
(`depth < 2 || !isOptional()`); the impl variant merely makes the threshold configurable and adds
optional session-data bookkeeping (`ignoredKeys` / `unselectedKeys`) that is unused by default.

### 6.3 `ScopeDependencySelector` (impl variant, `legacy` factory)

`ScopeDependencySelector.legacy(included = null, excluded = ["test", "provided"])` ⇒
`shiftIfRootNull = true`, `depth = 0`, `applyFrom = 1`, `applyTo = Integer.MAX_VALUE`.

```
selectDependency(dep):
    if depth < applyFrom || depth > applyTo: return true
    return (included == null || included.contains(dep.scope))
        && (excluded == null || !excluded.contains(dep.scope))

deriveChildSelector(ctx):
    if depth == 0 && shiftIfRootNull && ctx.getDependency() == null:
        return new(depth + 1, applyFrom + 1, …)          # the "shift"
    if depth >= applyFrom && depth != applyTo: return this
    return new(depth + 1, applyFrom, …)
```

The `shiftIfRootNull` branch reproduces Resolver 1.x behaviour, where scope filtering became active
one level later when the collect request had no root `Dependency`:

| Collect request | Level-1 selector | Scope filtering starts at |
|---|---|---|
| `rootArtifact` only (what Maven's project resolution uses) | `depth = 1, applyFrom = 2` | **graph level 2** — direct dependencies of *any* scope are kept |
| `root` `Dependency` set | `depth = 1, applyFrom = 1` | **graph level 1** — `test`/`provided` direct dependencies are dropped |

This is the mechanism behind "the test graph is not a superset of the runtime graph": when a
downstream project depends on artifact *X*, the collect request for *X*'s subtree is effectively
"root dependency" shaped, so *X*'s `test` and `provided` dependencies **never enter the graph at
all** — they are not "skipped", they do not exist and cannot participate in conflict resolution.

Scopes are compared as opaque strings; no scope hierarchy is assumed. The excluded set is
`{"test", "provided"}`; the included set is `null` (everything).

### 6.4 `StaticDependencySelector`

Constant `true`/`false`; `deriveChildSelector` returns `this`.

### 6.5 `AndDependencySelector` — composition and derivation

* Holds a `LinkedHashSet<DependencySelector>` — **insertion order is preserved and duplicates are
  collapsed**.
* `selectDependency` = logical AND, short-circuiting on the first `false`.
* `deriveChildSelector(ctx)`:
  * derive each constituent;
  * if **every** constituent returned itself (identity comparison, not `equals`) → return `this`;
  * otherwise collect the derived selectors, in the original order, dropping any that derived to
    `null`;
  * 0 remaining → return `null`; 1 remaining → return that selector *unwrapped*; otherwise a new
    `AndDependencySelector`.
* `AndDependencySelector.newInstance(a, b)`: `null` handling plus `b.equals(a) ⇒ a`.

Derivation for the default Maven composition, per level:

| Level | Scope selector | Optional selector | Exclusion selector |
|---|---|---|---|
| session (depth 0) | `legacy`, `depth 0`, `applyFrom 1` | `depth 0`, `applyFrom 2` | `[]` |
| 1 | shifted (rootArtifact case): `depth 1`, `applyFrom 2` | `depth 1` | `[]` ∪ root's exclusions |
| 2 | `depth 2`, `applyFrom 2` (**applies**) | `depth 2` (**applies**) | ∪ level-1 dependency exclusions |
| ≥ 3 | `this` (stable) | `this` (stable) | grows along the path |

From level 3 down, only the exclusion selector can change, so the `AndDependencySelector` returns
`this` for any path with no further exclusions — which is what makes `DataPool` graph-key hits
frequent.

---

## 7. Version ranges

### 7.1 Expansion to candidates

For every dependency (including the root), the collector calls
`VersionRangeResolver.resolveVersionRange` with the dependency's artifact and the in-scope
repositories. For a **plain version** the result contains exactly that version and no range
(`rangeResult.getVersionConstraint().getRange() == null`). For a **range** the resolver reads the
`maven-metadata.xml` of the `groupId:artifactId` in each remote repository (plus the local repository
and any workspace reader) and returns every `<version>` that falls inside the constraint, **in
ascending version order**, together with the repository each version was found in.

`DataPool` memoizes range results by `(artifact, repositories, requestContext)`; the stored form
retains the ascending order and the per-version repository.

### 7.2 Filtering (`DependencyCollectorDelegate.filterVersions`)

```
if rangeResult.versions.isEmpty():
    throw VersionRangeResolutionException("No versions available for <artifact> within specified range")

if verFilter != null && rangeResult.versionConstraint.range != null:
    vctx := versionFilterContext.initialize(dependency, rangeResult)   # a fresh, single-threaded copy
    verFilter.filterVersions(vctx)                                     # RepositoryException -> VersionRangeResolutionException
    versions := vctx.get()
    if versions.isEmpty():
        throw VersionRangeResolutionException("No acceptable versions for <artifact>: <versions>")
else:
    versions := rangeResult.versions
```

**The version filter is only consulted when the constraint is an actual range.** A plain version is
never filtered — you cannot ban a directly declared snapshot with a version filter.

The filter mutates the candidate list through the context's `Iterator.remove()`. Ascending order is
preserved.

Built-in filters:

| Filter | Rule | `deriveChildFilter` |
|---|---|---|
| `HighestVersionFilter(count = 1)` | keep only the last `count` versions; removes the first `size - count` | `this` |
| `LowestVersionFilter(count = 1)` | mirror image | `this` |
| `SnapshotVersionFilter` | removes every version whose artifact `isSnapshot()` | `this` |
| `ContextualSnapshotVersionFilter` | if `aether.snapshotFilter` is `true`, always bans snapshots. Otherwise at derive time: root artifact `null` ⇒ `this` (re-check at level 1); root artifact **is** a snapshot ⇒ **`null`** (filter removed, snapshots allowed all the way down); root artifact is a release ⇒ the plain `SnapshotVersionFilter` for the whole subtree | see rule |
| `ChainedVersionFilter` | runs constituents in order, stopping early when the candidate count hits 0; derives each and collapses to `this` / a single filter / `null` like `AndDependencySelector` | see rule |

### 7.3 Which candidate becomes a node

**All of them, except at the root.**

* **Root:** `versions.get(versions.size() - 1)` — the highest surviving candidate. Exactly one root
  node.
* **Every other node:** the collector loops over *all* surviving versions and appends **one sibling
  child node per version** under the same parent. Each such node carries the same
  `versionConstraint` (the whole range) but its own `version`, its own descriptor, and its own
  subtree. Conflict resolution later picks one and marks the rest.

**Order of the siblings differs between collectors:**

| Collector | Sibling order for a range |
|---|---|
| **BF** | **descending (newest first)** — `Collections.reverse` is applied so the skipper sees the newest candidate first |
| DF | ascending (oldest first) |

Each candidate node's `repositories` come from `rangeResult.getRepository(version)`: a singleton list
if that version was found in a `RemoteRepository`, an empty list if it was found in some other
repository type, and the inherited repository list if the range result has no repository for it.

### 7.4 No match

* Empty range result, or a filter that removed everything ⇒ `VersionRangeResolutionException`.
  * For the **root**, this is thrown out of `collectDependencies` immediately — no graph at all.
  * For any other node, the exception is recorded against the node's path and **no node is created**;
    at the end of collection this makes `collectDependencies` throw `DependencyCollectionException`
    with the partial graph attached.
* A version that resolves but whose descriptor cannot be read is a different case — see §10.

---

## 8. Relocation

### 8.1 Where it happens

Entirely inside `DefaultArtifactDescriptorReader.loadPom`, before the collector sees anything. The
collector only observes two things: `descriptorResult.getArtifact()` (possibly a
`RelocatedArtifact`) and `descriptorResult.getRelocations()` (the list of *pre*-relocation artifacts).

### 8.2 The reader's loop

```
visited := ordered set of "G:A:baseVersion"
a := request.artifact
loop:
    pomArtifact := DefaultArtifact(a.groupId, a.artifactId, "pom", a.version)   # classifier dropped, extension forced
    a           := a.setVersion(versionResolver.resolveVersion(a))
    pomArtifact := pomArtifact with resolved version
    if !visited.add(a.groupId + ':' + a.artifactId + ':' + a.baseVersion):
        -> relocation cycle: dispatch ARTIFACT_DESCRIPTOR_INVALID; honour IGNORE_INVALID
    resolve the POM artifact; result.setRepository(...)
    if workspace reader has a model for pomArtifact: return that model      # relocation NOT evaluated
    model := build effective model
    reloc := first non-null result from the relocation sources, in priority order
    if reloc == null:                 return model
    if withinSameGav(reloc, a):       result.setArtifact(reloc); return model   # NO addRelocation
    result.addRelocation(a)           # the artifact *before* this hop
    a := reloc
    result.setArtifact(a)
    continue                          # chained relocations are followed
```

* `withinSameGav` compares groupId **and** artifactId **and** version. A relocation that changes only
  the classifier or extension short-circuits: the artifact is rewritten but
  `getRelocations()` stays **empty**, so the collector's relocation branch does **not** fire.
* Cycle protection uses `baseVersion` and is applied *after* version resolution, so a self-relocation
  through a snapshot is caught on the second pass. A cycle is an "invalid descriptor" and is
  therefore swallowed under Maven's default policy (returns an empty descriptor result).

### 8.3 What the resulting node looks like

`RelocatedArtifact` wraps the original artifact and overrides only the coordinate components that the
`<relocation>` element actually specified; `null`/empty overrides **delegate to the wrapped
original**. That is the "absent element inherits from the original coordinate" rule. `getPath()`,
`getFile()`, `getProperties()` and `getProperty()` always delegate.

`RelocatedArtifact` exposes **no accessor for the wrapped original** — there is no `getArtifact()`
and no `getRelocatedFrom()`. The only extra accessor is `getMessage()`. **The original coordinate is
visible to callers exclusively through `DependencyNode.getRelocations()`**, which the collector
populates from `descriptorResult.getRelocations()`.

In the graph:

* the node's **dependency artifact is the relocation target** (the `RelocatedArtifact` instance, so
  its `getGroupId()`/`getArtifactId()`/`getVersion()` report the target);
* `node.getRelocations()` is the ordered list of pre-relocation artifacts, one per hop, oldest first;
* the node's children, aliases and managed dependencies all come from the **target's** descriptor;
* the node is created only once — the pre-relocation coordinate never gets its own node.

Maven logs a WARN for relocated **direct** dependencies only, using `getRelocations().get(0)` and
`RelocatedArtifact.getMessage()`.

### 8.4 Relocation sources

Consulted in `@Priority`-descending order:

| Priority | Name | Behaviour |
|---|---|---|
| 50 | `userProperties` | Driven by `maven.relocations.entries`. Comma-separated `source>target` (project-scoped, applies only when the request context starts with `"project"`) or `source>>target` (global). Coordinate syntax `g:a[:ext[:classifier]]:v` with `*` wildcards and trailing-`*` prefix matching, matched against groupId, artifactId, **baseVersion**, extension, classifier. First match wins. A missing target means **ban**: `ArtifactDescriptorException("The artifact … has been banned from resolution: User global/project ban")`. Parsed once and memoized in session data. |
| 5 | `distributionManagement` | Reads `model.getDistributionManagement().getRelocation()` and builds `RelocatedArtifact(result.getRequest().getArtifact(), relocation.groupId, relocation.artifactId, null, null, relocation.version, relocation.message)`. Note the base is the **original request artifact**, not the current hop's artifact. |

### 8.5 What the collector does with a relocation

See §2.2 step 10. Two subtleties:

1. `disableVersionManagementSubsequently` is `true` iff the relocation preserved both groupId and
   artifactId (a version-only relocation). It suppresses `MANAGED_VERSION` for the relocated
   dependency, so dependency management cannot immediately undo the relocation's version change.
2. In **BF only**, that flag is then handed to `doRecurse` and applied to the relocated node's
   *children* as well. DF resets it to `false` for children. **`[BF≠DF]`**

---

## 9. Cycles

### 9.1 Detection

`DefaultDependencyCycle.find(parents, artifact)` walks `parents` from the **deepest** element toward
the root and returns the index of the first node whose artifact matches on **groupId, artifactId,
extension and classifier — the version is deliberately ignored**. It returns `-1` if there is no
match, and **stops the walk (returns `-1`) as soon as it hits a node whose artifact is `null`**.

Version-insensitivity is intentional: `a:2 -> b:2 -> a:1` is treated as a cycle because the producing
projects form one, and conflict resolution would always make `a:1` a loser anyway.

The check is performed in step 9 of §2.2 — after the descriptor has been read and the artifact
replaced by the descriptor's (possibly relocated) artifact, and before the relocation branch.

### 9.2 What the graph holds at the cycle point

```
cycleEntry := find(parents, d.artifact)
if cycleEntry >= 0:
    results.addCycle(parents, cycleEntry, d)
    cycleNode := parents[cycleEntry]
    if cycleNode.getDependency() != null:
        child := createDependencyNode(relocations, preManaged, rangeResult, version, d,
                                      descriptorResult, cycleNode)
        child.setChildren(cycleNode.getChildren())        # SHARED list object
        parent.getChildren().add(child)
        continue                                          # next candidate version
    # else: fall through and process the node normally
```

So the graph gets a **new node** for the repeated coordinate whose `children`, `repositories` and
`requestContext` are taken from the ancestor node — and whose children list is the *same mutable list
object* as the ancestor's. Traversing the graph naively from that node therefore loops forever; the
graph is genuinely cyclic until the transformer chain breaks it.

If the matching ancestor is a **dependency-less root node** (the `rootArtifact`-only case), the cycle
is still *recorded* but the node is built and expanded normally.

### 9.3 `DefaultDependencyCycle` reporting

Constructed as `new DefaultDependencyCycle(nodes, cycleEntry, dependency)`:

```
offset := (cycleEntry > 0 && nodes[0].getDependency() == null) ? 1 : 0
dependencies := [ nodes[offset..].map(n -> n.getDependency() ?? Dependency(n.getArtifact(), null)),
                  dependency ]
```

* `getPrecedingDependencies()` = `dependencies[0 .. cycleEntry)`
* `getCyclicDependencies()` = `dependencies[cycleEntry .. end]`
* `toString()` joins with `" -> "` using `toVersionlessId` (`G:A:E[:C]`).

Cycles are appended to `CollectResult.getCycles()` and are capped by
`aether.dependencyCollector.maxCycles` (default 10; beyond that they are silently dropped). **A cycle
is not an error** — it never contributes to `getExceptions()` and never fails collection.

---

## 10. Errors

### 10.1 Recording (`DependencyCollectorDelegate.Results`)

```
addException(dependency, e, nodes):
    if maxExceptions < 0 || exceptionCount < maxExceptions:
        exceptionCount++
        result.addException(e)
        if errorPath == null:
            errorPath := nodes.map(n -> n.getDependency()?.getArtifact()).filter(non-null)
                              .join(" -> ") + " -> " + dependency.getArtifact()
```

Only the **first** exception sets `errorPath`. At the end of `collectDependencies`:

```
if errorPath != null: throw DependencyCollectionException(result, "Failed to collect dependencies at " + errorPath)
if !result.getExceptions().isEmpty(): throw DependencyCollectionException(result)
```

`DependencyCollectionException.getResult()` carries the **partial graph**, and Maven's
`DefaultProjectDependenciesResolver` uses it (`result.setDependencyGraph(e.getResult().getRoot())`).
So "fatal" means "the call throws", not "no graph was produced".

### 10.2 Classification

| Condition | Where | Outcome |
|---|---|---|
| Root version range unresolvable / empty | root handling | **Immediately fatal**; no graph is built at all |
| Root descriptor unreadable (and not tolerated by policy) | root handling | **Immediately fatal**; no graph |
| Non-root version range unresolvable / no acceptable version | prefetch task, surfaced at `future.get()` | exception recorded against the path; **no node is created**; fatal at the end of collection |
| Non-root descriptor unreadable and **not** tolerated by policy | `resolveCachedArtifactDescriptor` | exception recorded; the descriptor map value is `null`; a **childless node is still created** (§2.2 step 8); fatal at the end of collection. The failure is memoized in the `DataPool` as `BadDescriptor`, so a second occurrence returns `NO_DESCRIPTOR` **without recording a second exception** |
| Non-root descriptor missing/invalid and **tolerated** by policy (Maven's default) | descriptor reader | an **empty** `ArtifactDescriptorResult` is returned: same artifact, no dependencies, no managed dependencies, no repositories, no relocations. A childless node is created. **Not an error** |
| Missing parent or import POM (`ModelResolverException` inside the model build) | descriptor reader | **Always** `ArtifactDescriptorException`, regardless of `IGNORE_INVALID` |
| `VersionResolutionException` while resolving the descriptor's own version | descriptor reader | always `ArtifactDescriptorException`; policy is not consulted |
| Relocation cycle | descriptor reader | dispatched as `ARTIFACT_DESCRIPTOR_INVALID`; honours `IGNORE_INVALID` |
| Dependency cycle in the graph | collector | **never an error** (§9.3) |
| Thread interruption | collector | `DependencyCollectionException("Collection interrupted")` |

### 10.3 `ArtifactDescriptorPolicy`

Bits: `IGNORE_MISSING = 0x01`, `IGNORE_INVALID = 0x02`, `STRICT = 0x00`. When the session has no
policy, `STRICT` applies. Maven installs `SimpleArtifactDescriptorPolicy(true, true)` — both ignored
— and `DefaultMavenExecutionRequest` defaults `ignoreMissingArtifactDescriptor` and
`ignoreInvalidArtifactDescriptor` to `true`. `--strict-artifact-descriptor-policy` flips both to
`false`. Plugin dependency resolution overrides this to `(true, false)`.

`IGNORE_MISSING` is only consulted when the `ArtifactResolutionException`'s cause is an
`ArtifactNotFoundException`; any other resolution failure always throws.

### 10.4 `isLackingDescriptor`

```
isLackingDescriptor(session, artifact) =
    session.getSystemDependencyScope() != null
    && session.getSystemDependencyScope().getSystemPath(artifact) != null
```

i.e. the artifact carries a non-null `localPath` property (a `system`-scoped dependency). Such
artifacts get a synthesized empty `ArtifactDescriptorResult` (whose `artifact` is the request
artifact), never a repository lookup, and `traverse` is forced to `false`.

---

## 11. `ArtifactDescriptorResult` — the collector's whole view of a POM

| Field | Populated from | Used by the collector for |
|---|---|---|
| `artifact` | the request artifact, rewritten by relocation, plus a `downloadUrl` property from `<distributionManagement><downloadUrl>` | the node's artifact |
| `dependencies` | `model.getDependencies()`, converted | children to enqueue |
| `managedDependencies` | `model.getDependencyManagement().getDependencies()`, converted (import-scope BOMs already flattened by the model builder) | deriving the child `DependencyManager` |
| `repositories` | `model.getRepositories()` | aggregating the child repository list |
| `relocations` | one entry per relocation hop | §8 |
| `aliases` | **never populated by Maven** | node aliases (always empty) |
| `properties` | `prerequisites.maven`, `license.count`, `license.<i>.{name,url,comments,distribution}` | nothing in collection |
| `repository` | the repository the POM was resolved from | nothing in collection |

Nothing else from the effective POM reaches the collector — not packaging, not plugins, not
properties beyond the above.

**Model dependency → aether dependency** (`ArtifactDescriptorUtils.convert`):

* type lookup: `artifactTypeRegistry.get(dependency.getType())`; on miss, a synthetic type whose
  extension equals the type id and whose classifier is empty;
* artifact = `DefaultArtifact(groupId, artifactId, classifier, /*extension=*/ null, version,
  properties, type)` — extension is deliberately `null` so the **type's** extension wins; a non-empty
  model classifier wins over the type's;
* `properties` = `{"localPath": systemPath}` when `<systemPath>` is set, else `null`; type properties
  (`type`, `language`, `includesDependencies`, `constitutesBuildPath`) are merged underneath;
* exclusions become `Exclusion(groupId, artifactId, "*", "*")`;
* `optional` is **tri-state**: `null` when `<optional>` is absent;
* dependencies (and repositories) whose coordinates still contain `${` are dropped with a debug log.

---

## 12. BF vs DF — observable differences

Both collectors share `DependencyCollectorDelegate` (root handling, premanaged capture, range
filtering, cycle detection, error recording) and produce the same node *content*. They differ in
graph *shape* and *ordering*:

| # | Aspect | BF | DF |
|---|---|---|---|
| 1 | **Skipper** | Present. Duplicate / version-conflict-losing nodes are left as childless leaves (§3) | Absent. Every node is expanded unless the `DataPool` graph key hits |
| 2 | **Which occurrence owns a shared subtree** | the **shallowest** occurrence (level order) | the **leftmost depth-first** occurrence |
| 3 | **Range sibling order** | newest version first (`Collections.reverse`) | oldest version first |
| 4 | **`disableVersionManagement` after a relocation** | propagates to the relocated node's **children** | reset to `false` for children |
| 5 | **Pool insertion timing** | `putChildren` after the children are enqueued | `putChildren` before recursing |
| 6 | **Graph-key probe** | speculative probe with the *parent* manager first, then the derived one | single probe with the derived manager |
| 7 | **Descriptor fetching** | prefetched in parallel, deduplicated by `G:A:E[:C]:V`, first requester's repositories/version-filter win | fetched lazily, per node |
| 8 | **Traversal order of the queue/stack** | FIFO across the whole graph | LIFO/recursive |

Differences 1–4 change the emitted graph and are the reason a golden corpus needs separate `_BF` and
`_DF` fixtures. Differences 5–8 are performance/threading details that must not change the graph.

---

## 13. Maven 3.9 vs Maven 4 divergences

| # | Area | Maven 3.9 | Maven 4 |
|---|---|---|---|
| 1 | **`DependencyManager`** | `ClassicDependencyManager` (`deriveUntil = 2`, `applyFrom = 2`) — only the root's `<dependencyManagement>` is ever collected, and it applies from graph level 2 | `TransitiveDependencyManager` (`deriveUntil = MAX_VALUE`, `applyFrom = 2`) — **every** POM's `<dependencyManagement>` is collected and applies to its descendants, with nearest-to-root winning. `isInheritedDerived()` is narrowed to `depth == 0`, so **scope and optional are still only collected from the root**; only versions, local paths and exclusions become transitive |
| 2 | **`ClassicDependencyManager` level-1 hop** | present and load-bearing (MNG-4720): a direct dependency's own `<dependencyManagement>` is discarded | `TransitiveDependencyManager` has no hop; the level-1 POM's management is collected normally |
| 3 | **Scope model** | Maven 3 scope set / `Maven3ScopeManagerConfiguration` (reachable in Maven 4 via `maven.maven3Personality=true`) | `Maven4ScopeManagerConfiguration` |
| 4 | **Default collector** | `bf` (since Resolver 1.9) | `bf` |
| 5 | **Version filter** | none | none by default; `maven.session.versionFilter` accepts an expression (`h(n)`, `l(n)`, `s`, `nosnapshot`, `norelease`, `nopreview`, `noprerelease`, `noqualifier`, `e(V)`, `i(V)`, each optionally scoped with `@groupId[:artifactId]`, semicolon-separated) |
| 6 | **User relocations** | not available | `maven.relocations.entries` (§8.4), including coordinate **bans** |
| 7 | **Selector composition** | `AndDependencySelector(ScopeDependencySelector("test","provided"), OptionalDependencySelector(), ExclusionDependencySelector())` using the `util` classes | the `impl` classes via `ScopeDependencySelector.legacy(null, ["test","provided"])` and `OptionalDependencySelector.fromDirect()`. **Behaviourally identical** for the default configuration |
| 8 | **`ArtifactDecorator`s** | none | descriptor results may be decorated after reading (`Utils.getArtifactDecorators`) |
| 9 | **`ResolutionScope` on the request** | not available | when set, replaces the session selector with the scope manager's and runs `scopeManager.postProcess` on the result |
| 10 | **`DependencyCollectionChecker` / `maxRuns`** | not available | collection can be re-run; default `NOOP` ⇒ one run |
| 11 | **Descriptor reader** | `compat/maven-resolver-provider` `DefaultArtifactDescriptorReader` — same relocation loop and `convert` logic, but no `${`-placeholder filtering pass | `impl/maven-impl` reader with the placeholder filter and pluggable relocation sources |

The Rust port targets column "Maven 3.9": `ClassicDependencyManager`, no version filter, no user
relocations, single collection run.

---

## 14. Implementation checklist for the Rust port

**Data model**

- [ ] `Artifact` equality must include groupId, artifactId, version, extension, classifier, path/file
      **and the properties map** — the skipper's `winners` map depends on it.
- [ ] Provide `to_id` = `G:A:E[:C]:V` and `to_versionless_id` = `G:A:E[:C]` exactly (extension before
      classifier, classifier omitted when empty).
- [ ] `Dependency.optional` is tri-state in the descriptor (`Option<bool>`) but a plain `bool` on a
      graph node.
- [ ] Node children must be a **shared, mutable** list (`Rc<RefCell<Vec<..>>>` or an arena index) —
      the pool, the cycle branch and `putChildren` all rely on sharing the *same* list object.
- [ ] The graph is genuinely cyclic after collection; use indices/`Weak`, not owning recursion.

**Extension points**

- [ ] All five extension points need **structural equality and hashing** (the `DataPool` graph key
      compares them), and `derive_child` must return "unchanged" identity information so the
      `AndDependencySelector`/`ChainedVersionFilter` collapse rules work.
- [ ] `ExclusionDependencySelector`: sorted by `(artifactId, groupId, extension, classifier)`,
      deduplicated, `"*"` wildcard on all four fields, return self when nothing merges.
- [ ] `OptionalDependencySelector`: `depth < 2 ⇒ select all`, else `!optional`; stabilizes at depth 2.
- [ ] `ScopeDependencySelector`: implement the `shiftIfRootNull` shift — filtering starts at graph
      level 2 for a `rootArtifact`-only request and at level 1 for a root `Dependency`.
- [ ] `FatArtifactTraverser`: read the `includesDependencies` property, parse as a boolean.
- [ ] `ClassicDependencyManager`: `deriveUntil = 2`, `applyFrom = 2`, the `depth == 1` hop passing the
      same maps through, and `return this` for `depth >= 2`. GACE keys, first-wins for
      version/scope/optional/localPath, additive for exclusions, exclusions applied outside the
      `isApplied()` guard.

**Collector**

- [ ] FIFO queue; enqueue level-1 in declaration order; process one context at a time.
- [ ] Compute `noDescriptor` and `traverse` from the **managed, pre-version** dependency.
- [ ] Reverse the candidate list for ranges (newest first) — this is BF-specific and observable.
- [ ] Emit **one node per surviving candidate version**, except at the root, which takes the last
      (highest) candidate only.
- [ ] Relocation branch: re-test the selector, recompute the premanaged dependency, recurse inline,
      then `return` (abandoning remaining candidate versions), and propagate
      `disableVersionManagement` to children (BF semantics).
- [ ] Cycle check before the relocation branch, using GACE (version-insensitive), walking parents
      deepest-first and stopping at a `null` artifact.
- [ ] `doRecurse` order: derive selector/traverser/filter → speculative pool probe with the *parent*
      manager → derive manager → second probe if it changed → **skipper** → enqueue children →
      `putChildren` → `skipper.cache`.
- [ ] Consult the skipper for childless nodes too (step 12 of §2.2).

**Skipper**

- [ ] `skipResolution` always assigns a coordinate and records a result, even when it skips.
- [ ] Depth = `parents.len() + 1`; per-depth sequence counters increment in call order.
- [ ] Version conflict (versionless key hit with a different version) is checked **before** duplicate.
- [ ] `isLeftmost` compares the sequence of *this node's ancestor at the winner's depth* against the
      winner's sequence, and requires the winner to be strictly shallower.
- [ ] `cache` includes the node itself in the `parents` scan, so a force-resolved node and everything
      beneath it is never cached as a winner.

**Descriptor bridge**

- [ ] Convert model dependencies with the type registry: extension from the type, classifier from the
      model when non-empty, `localPath` property for `<systemPath>`, exclusions wildcarded on
      classifier and extension.
- [ ] Follow chained relocations with a `G:A:baseVersion` visited-set; short-circuit
      classifier/extension-only relocations *without* recording a relocation entry.
- [ ] `getRelocations()` is the only place the pre-relocation coordinate survives.
- [ ] Default `ArtifactDescriptorPolicy` = ignore missing **and** ignore invalid ⇒ a missing POM
      yields an empty descriptor and a childless node, not an error.
- [ ] A missing parent/import POM is always an error, regardless of the policy.

**Errors**

- [ ] Record only the first `errorPath`; cap exceptions at 50 and cycles at 10.
- [ ] Memoize descriptor failures so the same coordinate does not record the exception twice.
- [ ] Root-level failures abort immediately; non-root failures are collected and thrown at the end
      with the partial graph attached.
