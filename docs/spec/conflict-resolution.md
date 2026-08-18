# Dependency graph conflict resolution — compatibility specification

> **Provenance.** This document was derived by reading the Apache Maven Resolver and Apache Maven
> sources, which are licensed under the **Apache License, Version 2.0**. No source code is reproduced
> verbatim beyond short identifiers, expressions and literal constants required to specify behaviour.
>
> | Clone | Commit |
> |---|---|
> | `_reference/maven-resolver` | `ed4a939a850b73d9a85722c277da9de14b64f1e0` |
> | `_reference/maven` | `945813a7d4d91f32fe92d2c5a81d0a8223bc10b9` |
>
> Primary sources (paths relative to the `maven-resolver` clone root unless marked otherwise):
>
> | Area | Path |
> |---|---|
> | Facade / SPI / `Verbosity` / node-data keys | `maven-resolver-util/…/util/graph/transformer/ConflictResolver.java` |
> | **The algorithm** | `maven-resolver-util/…/util/graph/transformer/ClassicConflictResolver.java` |
> | Conflict grouping | `…/transformer/ConflictMarker.java` |
> | Topological sort + cycle detection | `…/transformer/ConflictIdSorter.java` |
> | Context keys | `…/transformer/TransformationContextKeys.java` |
> | Version selection | `…/transformer/NearestVersionSelector.java` (M3), `ConfigurableVersionSelector.java` (M4) |
> | Scope rules | `…/transformer/JavaScopeSelector.java`, `JavaScopeDeriver.java` |
> | Optionality | `…/transformer/SimpleOptionalitySelector.java` |
> | Request-context refinement | `…/transformer/JavaDependencyContextRefiner.java` |
> | Chaining | `…/transformer/ChainedDependencyGraphTransformer.java` |
> | Artifact id helper | `maven-resolver-util/…/util/artifact/ArtifactIdUtils.java` |
> | Node copy semantics | `maven-resolver-api/…/aether/graph/DefaultDependencyNode.java` |
> | M4 managed scopes | `maven-resolver-impl/…/internal/impl/scope/ManagedScopeSelector.java`, `ManagedScopeDeriver.java`, `ManagedDependencyContextRefiner.java`, `ScopeManagerImpl.java` |
> | M3 scope truth table (maven clone) | `impl/maven-impl/…/impl/resolver/scopes/Maven3ScopeManagerConfiguration.java` |
> | Canonical chain (maven clone) | `impl/maven-impl/…/impl/resolver/MavenSessionBuilderSupplier.java` |
> | Verbose tree rendering | `_reference/maven-dependency-tree/…/graph/internal/VerboseDependencyNode.java`, `VerboseJavaScopeSelector.java`, `DefaultDependencyCollectorBuilder.java`, `…/graph/DependencyCollectorRequest.java` |
> | Narrative docs | `src/site/markdown/dependency-graph.md`, `src/site/markdown/common-misconceptions.md` |

## How to read this document

jv targets **Maven 3.9 behaviour**. Maven 3.9 ships Resolver 1.9.x, where the algorithm lived
directly in `ConflictResolver`. Resolver 2.0.11 split it out verbatim into
**`ClassicConflictResolver`**, which is the default (`aether.conflictResolver.impl=auto` → `classic`)
and the *only* implementation jv implements. Every statement below about "the resolver" means
`ClassicConflictResolver` unless stated otherwise. Divergences from Maven 4 are flagged **`[M3≠M4]`**
inline and collected in [§11](#11-maven-39-vs-maven-4-differences).

Two conventions used throughout:

* **Identity** means Java reference equality (`==`). The algorithm keys several maps by object
  identity, and this is load-bearing — a Rust port must use pointer/arena-index identity, not value
  equality. Where identity matters it is called out explicitly.
* **Node** means `DependencyNode`. The root node normally has `getDependency() == null`.

---

## 1. The transformer chain

### 1.1 Maven 4 (`MavenSessionBuilderSupplier.getDependencyGraphTransformer()`)

`ChainedDependencyGraphTransformer` runs its members in order, feeding each the node returned by the
previous one and sharing one `DependencyGraphTransformationContext` (a `Map<Object,Object>` plus the
session):

1. `TypeCollector`
2. `ConflictResolver(ConfigurableVersionSelector, ManagedScopeSelector(scopeManager), SimpleOptionalitySelector, ManagedScopeDeriver(scopeManager))`
3. `ManagedDependencyContextRefiner(scopeManager)`
4. `TypeDeriver`

### 1.2 Maven 3.9 — **the chain jv implements**

1. `ConflictResolver(NearestVersionSelector, JavaScopeSelector, SimpleOptionalitySelector, JavaScopeDeriver)`
2. `JavaDependencyContextRefiner`

`ConflictResolver` is a facade: `transformGraph` reads `aether.conflictResolver.impl` (default
`auto`) and delegates to a freshly constructed `ClassicConflictResolver` carrying the same four
selectors. `ClassicConflictResolver` in turn *lazily* runs the two preparation transformers:

3. **`ClassicConflictResolver`** reads `SORTED_CONFLICT_IDS`; if absent it runs `ConflictIdSorter`
   inline.
4. **`ConflictIdSorter`** reads `CONFLICT_IDS`; if absent it runs `ConflictMarker` inline.

So the effective order is `ConflictMarker` → `ConflictIdSorter` → `ClassicConflictResolver` →
`JavaDependencyContextRefiner`.

### 1.3 Shared transformation context

| Key (`TransformationContextKeys`) | String value | Written by | Type | Read by |
|---|---|---|---|---|
| `CONFLICT_IDS` | `"conflictIds"` | `ConflictMarker` | `IdentityHashMap<DependencyNode,String>` | `ConflictIdSorter`, `ClassicConflictResolver` |
| `SORTED_CONFLICT_IDS` | `"sortedConflictIds"` | `ConflictIdSorter` | `List<String>` (topological order) | `ClassicConflictResolver` |
| `CYCLIC_CONFLICT_IDS` | `"cyclicConflictIds"` | `ConflictIdSorter` | `Collection<Collection<String>>` (empty set when acyclic) | `ClassicConflictResolver` |
| `STATS` | `"stats"` | caller (optional) | `Map<String,Object>` | all three, write-only |

**Hard requirement:** `ClassicConflictResolver` throws `RepositoryException("conflict id cycles have
not been identified")` if `CYCLIC_CONFLICT_IDS` is *absent* (`null`), and
`"conflict groups have not been identified"` if `CONFLICT_IDS` is absent. An **empty** collection is
the legal "no cycles" marker; `null` is not. A Rust port must therefore always write the key, even
for an acyclic graph.

`STATS` entries, if the map exists: `ConflictMarker.{analyzeTime,markTime,nodeCount}`,
`ConflictIdSorter.{graphTime,topsortTime,conflictIdCount,conflictIdCycleCount}`,
`ConflictResolver.{totalTime,conflictItemCount}`.

`JavaDependencyContextRefiner` reads and writes nothing in the context.

---

## 2. `ConflictMarker` — assigning conflict ids

### 2.1 The key

The grouping key is `Key(artifact)` with **exactly four fields**:

```
Key = (groupId, artifactId, extension, classifier)
```

* Equality: exact `String.equals` on all four, in the order artifactId, groupId, extension,
  classifier. Hash: `Objects.hash(artifactId, groupId, extension, classifier)`.
* **Version is deliberately NOT part of the key** — that is the whole point: nodes that differ only
  in version land in the same group.
* **`classifier` and `extension` ARE part of the key.** They are never normalised away. In
  `DefaultArtifact` an absent classifier is `""` and an absent extension defaults to `"jar"`.
  Consequences:
  * `g:a:jar` and `g:a:jar:tests` are **different** conflict groups (different classifier).
  * `g:a:jar` and `g:a:pom` are **different** conflict groups (different extension).
  * `g:a:jar` and `g:a:test-jar` are different — note that Maven's `test-jar` *type* maps to
    extension `jar` + classifier `tests` via the artifact-type registry, so the mapping must be
    applied before this key is formed.
* `Key.toString()` (debug only) is `groupId:artifactId:classifier:extension` — note the unusual
  field order; do not confuse it with `ArtifactIdUtils.toId`, which is
  `groupId:artifactId:extension[:classifier]:version`.

### 2.2 Relocations and aliases

`getKeys(node)`:

* `node.getDependency() == null` (the root) → empty set; the node gets **no** conflict id.
* otherwise → `{ Key(dependency.artifact) }`, and, if `node.getRelocations()` or `node.getAliases()`
  is non-empty, additionally `Key(r)` for every relocation `r` and `Key(a)` for every alias `a`.

**All keys of one node are forced into a single conflict group**, and this merging is transitive: if
node X ties keys {P,Q} and node Y ties {Q,R}, then P, Q and R end up in one group. So a relocated
artifact conflicts with both its old and new coordinates, and aliases (e.g. the legacy
`org.apache.maven:maven-artifact` ↔ `maven:maven-artifact` style aliasing) merge groups too.

### 2.3 Grouping algorithm (`analyze`)

State: `nodes: IdentityHashMap<DependencyNode,Boolean>`, `groups: HashMap<Key,ConflictGroup>`,
`counter: int` (starts 0). `ConflictGroup = (keys: Set<Key>, index: int)`.

DFS from the root in child order; `nodes.put(node)` returning non-null (already seen, by identity)
short-circuits the whole visit including recursion.

For a node with non-empty `keys`:

```
group = null; fixMappings = false
for key in keys:                       # iteration order of the key set
    g = groups.get(key)
    if group == g: continue
    if group == null:                  # here g != null
        newKeys = merge(g.keys, keys)
        if newKeys is g.keys (same object): group = g; break
        else: group = ConflictGroup(newKeys, counter++); fixMappings = true
    elif g == null:
        fixMappings = true
    else:
        newKeys = merge(g.keys, group.keys)
        if newKeys is g.keys: group = g; fixMappings = false; break
        elif newKeys is not group.keys: group = ConflictGroup(newKeys, counter++); fixMappings = true
if group == null: group = ConflictGroup(keys, counter++); fixMappings = true
if fixMappings: for key in group.keys: groups[key] = group
```

`merge(k1, k2)` returns **the same object** `k2` if `|k1| < |k2|` and `k2 ⊇ k1`; the same object `k1`
if `|k1| >= |k2|` and `k1 ⊇ k2`; otherwise a fresh `HashSet` union. The `newKeys is g.keys` identity
comparisons above therefore mean "no new keys were contributed".

Then recurse into `node.getChildren()` in order.

### 2.4 Marking (`mark`) and what is stored

For every visited node with a dependency:
`conflictIds[node] = String.valueOf(groups[Key(node.dependency.artifact)].index).intern()`.

* The map is an **`IdentityHashMap`** — reference-keyed. Two structurally equal node objects get
  separate entries.
* Conflict ids are **decimal strings of the group index**, e.g. `"0"`, `"1"`, `"7"`. They are
  assigned in DFS pre-order of first encounter, but the counter is also consumed by intermediate
  merged groups, so the final set of ids is **not necessarily contiguous** and a group's id is the
  index of the *last* `ConflictGroup` object created for it.
* Only the node's own artifact key is used for the lookup; that key is guaranteed present in `groups`
  because `fixMappings` maps every key of the surviving group.
* Stored under `CONFLICT_IDS`.

A Rust port may use any opaque conflict-id type (an interned index is natural), but the *grouping*
and the *DFS-first-encounter ordering of group creation* must match, because `ConflictIdSorter`'s
`LinkedHashMap` insertion order (§3) and hence tie-breaking depends on it.

---

## 3. `ConflictIdSorter` — topological order and cycles

If `CONFLICT_IDS` is absent it runs `ConflictMarker` first.

### 3.1 Building the conflict-id DAG

`ids: LinkedHashMap<String, ConflictId>` (insertion-ordered).
`ConflictId = { key: String, children: Set<ConflictId>, inDegree: int, minDepth: int }`; equality and
hash are by `key` alone.

Seed: if the root itself has a conflict id `k`, insert `ConflictId(k, minDepth=0)` and use it as the
current `id`; otherwise the current `id` is `null`.

```
buildConflictIdDAG(ids, node, id, depth, visited, conflictIds):
    if visited.put(node, TRUE) != null: return         # identity-keyed, global
    depth += 1
    for child in node.getChildren():                   # in order
        key = conflictIds[child]
        childId = ids[key]
        if childId == null: childId = ConflictId(key, depth); ids[key] = childId
        else: childId.pullup(depth)
        if id != null: id.add(childId)                 # edge id -> childId
        buildConflictIdDAG(ids, child, childId, depth, visited, conflictIds)
```

* `add(child)`: `children` is a `HashSet`; `inDegree` is incremented **only when the edge is new**.
  Parallel edges between the same pair of ids count once.
* `pullup(depth)`: `if depth < minDepth { minDepth = depth; for c in children: c.pullup(depth+1) }`.
  It terminates because it only recurses on a strict decrease.
* The `visited` short-circuit is checked *before* `depth++`, but the **edge is still added** for an
  already-visited child, so the DAG is complete even though each node's subtree is walked once.
* `minDepth` of an id is the smallest 1-based depth at which any node with that id appears
  (root-with-id gets 0; direct dependencies of the root get 1), as refined by `pullup`.

### 3.2 Topological sort

```
sorted = []
roots = RootQueue()
for id in ids.values():            # LinkedHashMap insertion order
    if id.inDegree <= 0: roots.add(id)
processRoots(sorted, roots)
cycle = sorted.size() < ids.size()
while sorted.size() < ids.size():
    nearest = argmin over {id : id.inDegree > 0} of (id.minDepth, id.inDegree)
              # strict improvement only: first minimal in ids-iteration order wins ties
    nearest.inDegree = 0
    roots.add(nearest)
    processRoots(sorted, roots)
cycles = cycle ? findCycles(ids.values()) : emptySet
context[SORTED_CONFLICT_IDS] = sorted
context[CYCLIC_CONFLICT_IDS] = cycles
```

`processRoots`: pop from the queue, append `root.key` to `sorted`, then for each child decrement
`inDegree` and enqueue it when it reaches exactly 0.

**`RootQueue` ordering** is the crux. It is an array-backed queue with *insertion sort by ascending
`minDepth`, stable*: a new element is shifted left past all existing elements with **strictly
greater** `minDepth`, so it lands **after** every element whose `minDepth` is `<=` its own. Net
effect: the queue always pops the smallest `minDepth` first, and within one `minDepth` it is FIFO in
insertion order.

The resulting order is therefore: **conflict ids nearest to the root first, in a breadth-favouring
topological order, with parents before children.**

### 3.3 Cycle detection and representation

Run only when the topological sort could not consume every id.

```
findCycles(ids):
    cycles: Set<Collection<String>> = {}
    stack: Map<String,Integer> = {}          # key -> position on the current DFS path
    visited: IdentityMap<ConflictId,Boolean> = {}
    for id in ids: findCycles(id, visited, stack, cycles)

findCycles(id, visited, stack, cycles):
    depth = stack.put(id.key, stack.size())
    if depth != null:                        # id.key already on the path -> back edge
        stack.put(id.key, depth)             # restore its original position
        cycles.add({ k for (k,v) in stack if v >= depth })
    else:
        if visited.put(id, TRUE) == null:
            for childId in id.children: findCycles(childId, visited, stack, cycles)
        stack.remove(id.key)
```

* A cycle is represented as an **unordered `Set<String>` of the conflict ids on the back-edge
  segment** — not as an ordered path.
* `visited` prevents re-expanding an id, so this is a *heuristic*: some cycles may be missed, and a
  reported set may be a superset of a minimal cycle. That is acceptable because the only consumer
  uses it to *widen* a pruning set (§4.4) — being too generous costs performance, not correctness.
* The marker `ClassicConflictResolver` requires is `CYCLIC_CONFLICT_IDS` itself: it must be present
  and it is empty (`Collections.emptySet()`) iff the sort succeeded without cycles.

### 3.4 Determinism caveat

`ConflictId.children` is a `java.util.HashSet` keyed on `ConflictId.hashCode() == key.hashCode()`
(the `String` hash of a small decimal). Its iteration order therefore affects
(a) the enqueue order in `processRoots` and hence tie-breaking within one `minDepth`, and (b) which
cycle sets `findCycles` reports. For **bit-exact** reproduction of `sortedConflictIds` on graphs with
ties, a Rust port must emulate `java.util.HashMap` iteration order: table size a power of two
starting at 16 with load factor 0.75, index `= (h ^ (h >>> 16)) & (len-1)`, iteration = ascending
bucket index then per-bucket insertion order, and rehash-on-resize preserving relative order within
a split bucket. See [§12](#12-implementation-checklist-rust-port-targeting-maven-39). A simpler
insertion-ordered set is usually indistinguishable but is not guaranteed to be.

---

## 4. `ClassicConflictResolver`

### 4.1 Top-level driver

```
transformGraph(root, ctx):
    sortedConflictIds = ctx[SORTED_CONFLICT_IDS] ?? (run ConflictIdSorter; re-read)
    conflictIdCycles  = ctx[CYCLIC_CONFLICT_IDS]  # throw if null
    conflictIds       = ctx[CONFLICT_IDS]         # throw if null

    # cyclicPredecessors: for every id in every cycle, the union of that cycle's members
    # (an id in a cycle maps to a set that INCLUDES ITSELF)
    cyclicPredecessors = {}
    for cycle in conflictIdCycles:
        for cid in cycle: cyclicPredecessors.setdefault(cid, set()).update(cycle)

    state = State(root, conflictIds, len(sortedConflictIds), ctx)

    for (i, cid) in enumerate(sortedConflictIds):
        state.prepare(cid, cyclicPredecessors.get(cid))
        gatherConflictItems(root, state)          # DFS; also nukes leftover losers
        state.finish()                            # assign ConflictItem.depth
        if state.items is not empty:
            versionSelector.selectVersion(conflictCtx)
            if conflictCtx.winner is None: throw "conflict resolver did not select winner among …"
            winner = conflictCtx.winner.node
            scopeSelector.selectScope(conflictCtx)
            if verbosity != NONE: winner.data[NODE_DATA_ORIGINAL_SCOPE] = winner.dependency.scope
            winner.setScope(conflictCtx.scope)
            optionalitySelector.selectOptionality(conflictCtx)
            if verbosity != NONE: winner.data[NODE_DATA_ORIGINAL_OPTIONALITY] = winner.dependency.isOptional()
            winner.setOptional(conflictCtx.optional)
            removeLosers(state)
        state.winner()                            # resolvedIds[cid] = winner node or None
        if i == last and conflictIdCycles is not empty and conflictCtx.winner is not None:
            state.prepare("", None)               # "" matches no conflict id
            gatherConflictItems(conflictCtx.winner.node, state)   # final cycle cleanup walk
    return root
```

Ordering details that matter:

* `NODE_DATA_ORIGINAL_SCOPE` is written **after** `selectScope` but **before** `winner.setScope`, so
  it holds the pre-resolution (collected, possibly dependency-managed) scope. Same pattern for
  optionality.
* `setScope` / `setOptional` on `DefaultDependencyNode` replace the immutable `Dependency` with a new
  one; the node object identity is preserved (important, because `resolvedIds` compares by identity).
* The `winner()` call happens even when `items` is empty, recording `resolvedIds[cid] = null`. This
  **inserts the key with a null value**, which later makes `resolvedIds.containsKey(cid)` true (see
  §4.6) while `loser()` stays false. Both behaviours are load-bearing; a Rust port needs
  `Option<NodeId>` values in a map that distinguishes "absent" from "present-but-None".

### 4.2 `State`

| Field | Type | Lifetime | Meaning |
|---|---|---|---|
| `currentId` | `String` | per conflict id | the id being processed |
| `verbosity` | enum | whole run | from `aether.conflictResolver.verbose` |
| `resolvedIds` | `HashMap<String, DependencyNode?>` | whole run | id → winner node (or `null`); presence = "already processed" |
| `potentialAncestorIds` | `HashSet<String>` | whole run, **never cleared** | ids allowed to be descended through |
| `conflictIds` | `IdentityHashMap<Node,String>` | whole run | output of `ConflictMarker` |
| `items` | `List<ConflictItem>` | per conflict id | the conflict items gathered, in DFS order |
| `infos` | `IdentityHashMap<List<Node>, NodeInfo>` | per conflict id | per-parent bookkeeping, **keyed by the node's children list object** |
| `stack` | `IdentityHashMap<List<Node>, Boolean>` | per DFS | nodes currently on the DFS path, same key |
| `parentNodes` / `parentScopes` / `parentOptionals` / `parentInfos` | parallel stacks | per DFS | ancestor chain; `parentInfos` may hold `null` |
| `conflictCtx`, `scopeCtx` | reused objects | whole run | passed to the selectors |
| `versionSelector` … | selector instances | whole run | obtained once via `getInstance(root, ctx)` |

**Why the children list is the identity key.** The dependency collector shares child lists between
node instances that represent the same resolved artifact, and produces "dirty" structures for cycles.
Keying `infos` and `stack` by `node.getChildren()` (the `List` object identity) makes two node objects
that share a child list count as the same graph node for cycle detection and for scope/optionality
memoisation. A Rust port must reproduce this: the identity unit is the child-list, not the node.

`prepare(cid, cyclicPreds)`:
```
currentId = cid
conflictCtx.{conflictId=cid, winner=None, scope=None, optional=None}
items.clear(); infos.clear()
if cyclicPreds is not None: potentialAncestorIds.addAll(cyclicPreds)
```
Note `potentialAncestorIds`, `resolvedIds`, `stack` and the parent stacks are **not** cleared. The
stacks are empty anyway because every `push` that returns true is matched by a `pop`.

### 4.3 The DFS (`gatherConflictItems`)

Returns `false` to tell the caller "remove this child from its parent's child list".

```
gatherConflictItems(node, state) -> bool:
    cid = state.conflictIds.get(node)              # identity lookup; None for the root
    if state.currentId == cid:
        state.add(node)                            # record a ConflictItem; DO NOT recurse
    elif state.loser(node, cid):
        return false                               # leftover loser of an already-processed id
    elif state.push(node, cid):
        for child in node.getChildren():           # mutable iteration
            if not gatherConflictItems(child, state): remove child from the list
        state.pop()
    return true
```

The "do not recurse into a node that carries the current conflict id" rule is why losers can survive
below a conflicting node: they are cleaned up by *later* walks (which now descend through the
now-resolved id) and, for cyclic graphs, by the final cleanup walk.

`loser(node, cid)`: `w = resolvedIds.get(cid); return w != null && w !== node`. Identity comparison.
An id resolved to `null` (empty item set) never marks anything a loser.

### 4.4 Descent pruning (`push`)

```
push(node, cid) -> bool:
    if cid is None:
        if node.dependency is not None:
            if node.data[NODE_DATA_WINNER] is not None: return false   # a loser COPY (see §5)
            throw RepositoryException("missing conflict id for node …")
        # else: the root, which has no dependency -> fall through
    elif cid not in potentialAncestorIds:
        return false                       # not yet resolved and not a cyclic predecessor -> prune

    graphNode = node.getChildren()         # identity key
    if stack.put(graphNode, TRUE) != null: return false      # already on this DFS path -> cycle

    depth = len(parentNodes)
    scope    = deriveScope(node, cid)
    optional = deriveOptional(node, cid)
    info = infos.get(graphNode)
    if info is None:
        info = NodeInfo(minDepth=depth, derivedScopes=scope, derivedOptionalities=bit(optional))
        infos[graphNode] = info
        parentInfos.push(info); parentNodes.push(node); parentScopes.push(scope); parentOptionals.push(optional)
    else:
        changes = info.update(depth, scope, optional)
        if changes == 0:
            stack.remove(graphNode)
            return false                   # nothing new -> do not re-walk the subtree
        parentInfos.push(None)             # <-- suppresses creation of NEW ConflictItems here
        parentNodes.push(node); parentScopes.push(scope); parentOptionals.push(optional)
        if info.children is not None:
            if changes & CHANGE_SCOPE:
                for item in reversed(info.children): item.addScope(deriveScope(item.node, None))
            if changes & CHANGE_OPTIONAL:
                for item in reversed(info.children): item.addOptional(deriveOptional(item.node, None))
    return true
```

`NodeInfo.update(depth, scope, optional)`:
```
if depth < minDepth: minDepth = depth              # ALWAYS, even if nothing else changed
changes = 0
if derivedScopes == scope:            changes = 0          # Set never equals a String
elif derivedScopes is a Collection:   changes = derivedScopes.add(scope) ? CHANGE_SCOPE : 0
else:                                 derivedScopes = {old, scope}; changes = CHANGE_SCOPE
bit = optional ? OPT_TRUE(0x02) : OPT_FALSE(0x01)
if (derivedOptionalities & bit) == 0: derivedOptionalities |= bit; changes |= CHANGE_OPTIONAL
return changes
```
`CHANGE_SCOPE = 0x01`, `CHANGE_OPTIONAL = 0x02`.

Three independent prunes, in this order: (1) the id is not a potential ancestor of the current id;
(2) the node is already on the current DFS path (cycle); (3) the node has already been visited in
this walk with the same derived scope *and* the same derived optionality. `minDepth` is still
lowered in case (3) — this is why depths are minima over all paths even though subtrees are not
re-walked.

`potentialAncestorIds` starts empty and only ever grows: `finish()` adds `currentId` after each
round, and `prepare()` adds the current id's cyclic peers. So when processing conflict id *k* (the
*k*-th in topological order), descent is allowed only through nodes whose conflict id is one of the
already-processed ids, or a member of a cycle containing *k*. Since the order is topological, every
legitimate ancestor id of *k* has already been processed — that is exactly why the sort is required.

`pop()`: pops one entry off each of the four parallel stacks and does `stack.remove(node.getChildren())`.

### 4.5 Building and deduplicating `ConflictItem`s

```
add(node):
    parent = parentNodes.last() or None
    if parent is None:
        items.append(newConflictItem(None, node))
    else:
        info = parentInfos.last()
        if info is not None:                       # None => this parent is being re-visited
            item = newConflictItem(parent, node)
            info.add(item)                         # register under the parent's NodeInfo
            items.append(item)

newConflictItem(parent, node) = ConflictItem(parent, node,
                                             deriveScope(node, None), deriveOptional(node, None))
```

`ConflictItem` fields:

| Field | Value |
|---|---|
| `parent` | `parent.getChildren()` — **the child-list object**, or `null` for a root item |
| `artifact` | `parent.getArtifact()` (debug only), `null` for a root item |
| `node` | the conflicting node; **mutable** — `removeLosers` rewrites it to the loser copy |
| `depth` | assigned later in `finish()`; defaults to 0 |
| `scopes` | one `String`, promoted to a `HashSet<String>` on first divergence |
| `optionalities` | bit field, `OPTIONAL_FALSE = 0x01`, `OPTIONAL_TRUE = 0x02` |

**The deduplication identity key is the parent's child-list object.** One `ConflictItem` is created
per *occurrence of the conflict id inside one parent's child list*, the first time that parent list
is pushed during this walk. Consequences:

* The same node object reached via two different parents yields **two** items (they are not
  siblings).
* Two distinct child nodes with the same conflict id inside one parent's list yield **two** items
  (they *are* siblings — `isSibling` compares the `parent` list objects by identity, and two `null`
  parents count as siblings).
* The same parent reached again via another path yields **zero** new items. Instead `push` walks
  `info.children` (the items already created under that parent, in reverse insertion order) and
  merges the newly derived values into them:
  * **different derived scope** → `item.addScope(s)`: `scopes` becomes a `HashSet` holding both;
    subsequent adds insert into the set. Nothing is removed, so `getScopes()` accumulates every
    derived scope seen along every walked path.
  * **different derived optionality** → `item.addOptional(o)`: `optionalities |= bit`. Both bits can
    be set simultaneously.
  * If neither changed, `push` returns false and the retro-update loop is not even reached.

Note the retro-update recomputes the child's derived scope/optionality *after* the new parent scope
and optional flag have been pushed onto `parentScopes`/`parentOptionals`, so the derivation sees the
new parent context. It passes `conflictId = null`, which disables the "already resolved" shortcut in
§4.6 (but not the managed-bits shortcut).

### 4.6 Scope and optionality derivation

```
deriveScope(node, cid):
    if (node.managedBits & MANAGED_SCOPE) != 0 or (cid is not None and cid in resolvedIds):
        return node.dependency?.scope                 # None when the node has no dependency
    depth = len(parentNodes)
    scopeCtx.parentScope  = depth > 0 ? parentScopes[depth-1] : None
    scopeCtx.childScope   = node.dependency?.scope
    scopeCtx.derivedScope = node.dependency?.scope
    if depth > 0: scopeDeriver.deriveScope(scopeCtx)
    return scopeCtx.derivedScope
```

* The `MANAGED_SCOPE` bit means dependency management set the scope explicitly, which wins over
  derivation.
* `cid in resolvedIds` uses `containsKey`, so an id that resolved to *no* winner still counts as
  resolved and its nodes keep their own scope.
* The `ScopeContext` is a reused mutable object; its fields are assigned **directly** by `scopes()`,
  bypassing the constructor's `null → ""` normalisation. So `getParentScope()` and `getChildScope()`
  can legitimately return `null` here (only for the root, which has no dependency). `JavaScopeDeriver`
  treats `null` and `""` identically.
* At depth 0 the deriver is not called at all; the derived scope is simply the child's own scope.

```
deriveOptional(node, cid):
    optional = node.dependency is not None and node.dependency.isOptional()
    if optional or (node.managedBits & MANAGED_OPTIONAL) != 0 or (cid is not None and cid in resolvedIds):
        return optional
    depth = len(parentNodes)
    return depth > 0 ? parentOptionals[depth-1] : false
```

I.e. a node declared optional is optional; a node whose optional flag was set by dependency
management keeps that flag; a node of an already-resolved conflict id keeps its own flag; otherwise
optionality is **inherited from the parent's derived optionality**.

### 4.7 Depth assignment (`finish`)

```
finish():
    previousParent = None; previousDepth = 0
    totalConflictItems += len(items)
    for item in reversed(items):
        if item.parent is previousParent:            # identity; both may be None
            item.depth = previousDepth
        elif item.parent is not None:
            previousParent = item.parent
            previousDepth = infos[previousParent].minDepth + 1
            item.depth = previousDepth
    potentialAncestorIds.add(currentId)
```

Stated precisely and without the caching:

> **`ConflictItem.depth` = 0 for an item with a `null` parent; otherwise
> `infos[item.parent].minDepth + 1`, where `minDepth` is the minimum, over every path from the root
> that the current walk actually descended, of the number of edges from the root to that parent node.**

So depth is a **minimum over paths**, not the depth of the path on which the item happened to be
created — that is why `NodeInfo.update` lowers `minDepth` unconditionally (§4.4), including on visits
that are otherwise pruned. Because `add()` runs before the whole DFS finishes but `finish()` runs
after it, all minima are final by the time depths are read.

Depth is 0-based: a direct dependency of the root has depth 1 (its parent, the root, has
`minDepth = 0`). Only a conflicting node that *is* the root has depth 0. "Minimum over paths the
walk descended" is not the same as "minimum over all paths in the graph": the three prunes in §4.4
can keep the walk out of some paths. In practice every path to a node of the current conflict id is
reachable, because all ancestor ids are already in `potentialAncestorIds`.

### 4.8 The final cycle-cleanup walk

After the *last* conflict id has been processed, and only if `CYCLIC_CONFLICT_IDS` is non-empty and
the last id produced a winner:

```
state.prepare("", None)                 # "" is deliberately an id that no node carries
gatherConflictItems(lastWinnerNode, state)
```

* `currentId = ""` matches nothing, so `add()` is never called and `items` stays empty.
* By now `potentialAncestorIds` contains every conflict id, so the DFS descends everywhere it can.
* Its only effect is the `return false` → child removal side effect: every leftover loser reachable
  from the last winner is spliced out.
* The walk is rooted at **the last winner node**, not at the graph root, and its return value is
  discarded. No `finish()` or `winner()` follows.

---

## 5. `removeLosers` — the verbosity-dependent surgery

Called once per conflict id, immediately after the winner's scope and optional flag have been
applied. `state.items` is in DFS order, so items sharing a parent are contiguous *within one parent's
sweep*.

```
winner = conflictCtx.winner                            # a ConflictItem
winnerArtifactId = ArtifactIdUtils.toId(winner.node.artifact)
previousParent = None; childIt = None; toRemoveIds = set()

# ---- pass 1 ----
for item in state.items:
    if item is winner: continue
    if item.parent is not previousParent:
        childIt = item.parent.listIterator()           # fresh cursor at position 0
        previousParent = item.parent
    while childIt.hasNext():
        child = childIt.next()
        if child is item.node:                         # identity
            if verbosity == NONE:
                childIt.remove(); break
            if verbosity == STANDARD:
                childArtifactId = ArtifactIdUtils.toId(child.artifact)
                if winnerArtifactId != childArtifactId:
                    toRemoveIds.add(childArtifactId)
            loser = DefaultDependencyNode(child)       # copy ctor, see below
            loser.data[NODE_DATA_WINNER]               = winner.node
            loser.data[NODE_DATA_ORIGINAL_SCOPE]       = loser.dependency.scope
            loser.data[NODE_DATA_ORIGINAL_OPTIONALITY] = loser.dependency.isOptional()
            loser.setScope(item.getScopes().iterator().next())
            loser.setChildren([])
            childIt.set(loser)                         # replace in place
            item.node = loser
            break

# ---- pass 2, STANDARD only, only if toRemoveIds is non-empty ----
previousParent = None
for item in state.items:
    if item is winner: continue
    if item.parent is not previousParent:
        childIt = item.parent.listIterator(); previousParent = item.parent
    while childIt.hasNext():
        child = childIt.next()
        if child is item.node:                         # now the loser COPY
            childArtifactId = ArtifactIdUtils.toId(child.artifact)
            if childArtifactId in toRemoveIds and relatedSiblingsCount(child.artifact, item.parent) > 1:
                childIt.remove()
            break
```

`relatedSiblingsCount(artifact, parentList)` = the number of nodes currently in `parentList` whose
artifact has the same **`groupId:artifactId`** — group and artifact only, ignoring classifier,
extension and version.

`ArtifactIdUtils.toId(artifact)` = `groupId:artifactId:extension[:classifier]:version` — the
classifier segment is omitted entirely when the classifier is empty, and `version` is the resolved
version (`getVersion()`, i.e. a timestamped snapshot version if applicable), **not** the base version.

`DefaultDependencyNode(node)` copy constructor copies `dependency`, `artifact`, `aliases`,
`requestContext`, `managedBits`, `relocations`, `repositories`, `version`, `versionConstraint`, and a
**shallow copy of the data map** (or `null` if empty). It sets `children` to a **new empty list**. It
does *not* register the copy in `CONFLICT_IDS`, which is why §4.4's `NODE_DATA_WINNER` check exists.

### 5.1 Per-verbosity summary

| | `NONE` (default) | `STANDARD` (`verbose=true`) | `FULL` |
|---|---|---|---|
| Loser node in the parent's child list | **removed** | **replaced by a copy**, then conditionally removed in pass 2 | **replaced by a copy**, kept |
| Loser's children | gone with the node | **emptied** (`setChildren([])`) | **emptied** (`setChildren([])`) |
| `NODE_DATA_WINNER` on the copy | – | winner node object | winner node object |
| `NODE_DATA_ORIGINAL_SCOPE` on the copy | – | the copy's pre-overwrite scope | same |
| `NODE_DATA_ORIGINAL_OPTIONALITY` on the copy | – | the copy's pre-overwrite optional flag | same |
| Copy's scope | – | `item.getScopes().iterator().next()` — **an arbitrary element** when the item accumulated several derived scopes | same |
| `NODE_DATA_ORIGINAL_SCOPE` / `_OPTIONALITY` on the **winner** | not written | written (§4.1) | written |
| Cycles | removed (DFS `stack` prune + leftover-loser removal + final cleanup walk) | removed | **left in the graph** |
| Duplicates / conflicts visible in the result | none | yes, as childless marker nodes | yes, all of them |
| Graph is resolvable | yes | **no** | **no** |

Notes that decide `dependency:tree` output:

* **FULL never removes a loser.** `removeLosers` under FULL performs *only* the copy-and-annotate
  step. Cycles are not removed either — a naive recursive consumer will not terminate. (This is what
  the `ConflictResolver` javadoc warns about, and it is why `JavaDependencyContextRefiner` — §9 —
  must not be run on a FULL graph.)
* **`toRemoveIds` is keyed by `ArtifactIdUtils.toId`, which includes the version.** A loser whose id
  equals the winner's id (same GAV+classifier+extension — a *duplicate*, not a version conflict) is
  never added to `toRemoveIds` and is therefore never removed in pass 2. This is deliberate: it
  guarantees the "nearest" node is retained regardless of iteration order.
* **The pass-2 sibling condition.** A loser copy is removed only when its artifact id is in
  `toRemoveIds` **and** its parent's child list currently holds **more than one** node with the same
  `groupId:artifactId`. The common conflict case (parent P has exactly one node for `g:a`) therefore
  *keeps* the loser — that is what prints as `(g:a:jar:1.0:compile - omitted for conflict with 2.0)`.
  The version-range case (the collector expanded a range into several sibling nodes `g:a:1.0`,
  `g:a:1.1`, `g:a:1.2` under P) has count 3, so the non-winning expansions are pruned and the tree
  does not falsely claim P diverges.
* **Pass 2 is order-sensitive**: `relatedSiblingsCount` is evaluated against the *live* list, so a
  removal earlier in the pass lowers the count for later items under the same parent. Iterate
  `state.items` in order and mutate in place.
* **Cursor reuse.** The `ListIterator` is reset only when `item.parent` *changes* between two
  consecutive items, so a run of consecutive items sharing a parent shares one cursor and each scan
  resumes where the previous one stopped. Items for one parent need not be contiguous in
  `state.items` (a sibling subtree walked in between contributes its own items), in which case the
  parent gets a fresh cursor from position 0 on re-entry — still correct, because matching is by node
  identity. The distinction is only observable if the *same node object* appears twice in one child
  list: a shared cursor matches the second occurrence for the second item, a restarted cursor would
  match the first again. Reuse also keeps the pass O(list) per parent instead of O(list²).
* **Edge case, root item.** An item with `parent == null` (the root node itself carries the conflict
  id) would dereference `null` in `item.parent.listIterator()` if it lost. It cannot lose in practice:
  its depth is 0 and `isSibling` against any non-root item is false, so the nearest-wins rule always
  selects it. A Rust port should assert rather than handle this.
* Losers can still exist *below the winner* after this (cycles); §4.8 and later conflict-id walks
  clean them up.

---

## 6. `NearestVersionSelector`

```
selectVersion(context):
    constraints: Set<VersionConstraint> = {}
    candidates: List<ConflictItem> = []
    winner: ConflictItem? = None

    for item in context.getItems():                     # DFS insertion order
        node = item.node
        constraint = node.versionConstraint
        backtrack = false
        hardConstraint = constraint.getRange() is not None

        if hardConstraint and constraints.add(constraint):        # newly seen constraint
            if winner is not None and not constraint.containsVersion(winner.node.version):
                backtrack = true

        if isAcceptable(constraints, node.version):
            candidates.append(item)
            if backtrack:               do_backtrack()
            elif winner is None or isNearer(item, winner): winner = item
        elif backtrack:                 do_backtrack()

    context.setWinner(winner)           # may be None -> the driver then throws

isAcceptable(constraints, v) = every constraint in `constraints` containsVersion(v)

do_backtrack():
    winner = None
    for candidate in list(candidates):                  # in insertion order, removing in place
        if not isAcceptable(constraints, candidate.node.version): candidates.remove(candidate)
        elif winner is None or isNearer(candidate, winner): winner = candidate
    if winner is None: throw UnsolvableVersionConflictException(paths of nodes in this conflict group)
```

### 6.1 The rule and its tie-break

```
isNearer(a, b) = a.isSibling(b) ? a.node.version.compareTo(b.node.version) > 0
                                : a.depth < b.depth
```

**The rule.** Iterating conflict items in DFS discovery order, an item replaces the incumbent winner
only if it is *strictly* nearer: for two items under **the same parent child-list** ("siblings"),
nearer means a **strictly higher version**; for items under different parents it means a **strictly
smaller `ConflictItem.depth`** (§4.7, the minimum-over-paths depth).

**The tie-break.** Because the comparison is strict, ties keep the incumbent — i.e. the **first item
in DFS order** wins any tie, whether the tie is on depth (different parents, equal depth) or on
version (same parent, equal version). Since the DFS visits children in declaration order, "first in
DFS order" is Maven's familiar "first declared wins among equally near dependencies".

Note the asymmetry: siblings are compared by *version* (highest wins), not by depth — sibling items
necessarily have equal depth anyway, so a depth comparison could never break the tie. This is what
makes version ranges expanded into several sibling nodes resolve to the highest acceptable version.

### 6.2 Version ranges

* `hardConstraint` ⟺ `constraint.getRange() != null`. A plain version (`<version>1.0</version>`) is a
  *soft* constraint: `getRange()` is `null` and it imposes nothing on the others. A declared range
  (`[1.0,2.0)`, `[1.0]`, …) is *hard* and every subsequently considered version must satisfy **all**
  hard constraints seen so far.
* Constraints accumulate as iteration proceeds, so the order of `context.getItems()` matters and a
  newly discovered range can invalidate an already-chosen winner. That is what `backtrack` handles:
  it drops the winner, re-filters `candidates` against the (now larger) constraint set, and re-runs
  the nearest-wins scan over the survivors.
* `constraints` is a `HashSet<VersionConstraint>`, so the same range declared twice is added once and
  does not trigger a second backtrack.
* If backtracking leaves no acceptable candidate, resolution **fails** with
  `UnsolvableVersionConflictException`, whose paths are collected by walking the root with a filter of
  `context.isIncluded(node)` (i.e. all nodes carrying the current conflict id).
* An item whose version fails the accumulated constraints is never added to `candidates` and can
  never win, even if it is nearest.
* `NearestVersionSelector` is `@Deprecated` in Resolver 2.x in favour of
  `ConfigurableVersionSelector` — but `ConfigurableVersionSelector`'s default `Nearest` strategy is
  the identical predicate, so behaviour is unchanged. **`[M3≠M4]`** only in the class used.

---

## 7. Scope: `JavaScopeDeriver`, `JavaScopeSelector`, and the Maven 3 table

### 7.1 `JavaScopeDeriver` — the parent × child → derived matrix

```
getDerivedScope(parentScope, childScope):
    1. if childScope in {"system", "test"}:                          return childScope
    2. if parentScope is null or "" or parentScope == "compile":     return childScope
    3. if parentScope in {"test", "runtime"}:                        return parentScope
    4. if parentScope in {"system", "provided"}:                     return "provided"
    5. otherwise:                                                    return "runtime"
```

`parentScope` here is the parent's **already-derived** scope (from `parentScopes`), not its declared
scope. Rows are that; columns are the child's own declared/managed scope.

| parent ↓ \ child → | `compile` | `provided` | `runtime` | `test` | `system` | other/`""` |
|---|---|---|---|---|---|---|
| `""` / null (root) | compile | provided | runtime | **test** | **system** | *child* |
| `compile` | compile | provided | runtime | **test** | **system** | *child* |
| `provided` | provided | provided | provided | **test** | **system** | provided |
| `runtime` | runtime | runtime | runtime | **test** | **system** | runtime |
| `test` | test | test | test | **test** | **system** | test |
| `system` | provided | provided | provided | **test** | **system** | provided |
| *other* | runtime | runtime | runtime | **test** | **system** | runtime |

Bold cells are decided by rule 1 (child wins outright) before the parent is even consulted. The
bottom row (rule 5) is unreachable with the five standard scopes; it only fires for a non-standard
parent scope.

Practical note: the default `DependencySelector` in Maven (`ScopeDependencySelector("test",
"provided")`) drops `test` and `provided` children below depth 1, so the `test`/`provided` **columns**
mostly matter for direct dependencies, while the `test`/`provided`/`system` **rows** matter for the
subtrees of direct test/provided/system dependencies.

The deriver is invoked only when: DFS depth > 0, **and** the node does not have the `MANAGED_SCOPE`
bit, **and** the node's conflict id has not already been resolved (§4.6).

### 7.2 `JavaScopeSelector` — picking the winner's effective scope

```
selectScope(context):
    scope = context.getWinner().getDependency().getScope()      # the winner's own current scope
    if scope != "system":
        scope = chooseEffectiveScope(context.getItems())
    context.setScope(scope)

chooseEffectiveScope(items):
    scopes = HashSet()
    for item in items:                                 # DFS insertion order
        if item.getDepth() <= 1: return item.getDependency().getScope()   # SHORT-CIRCUIT
        scopes.addAll(item.getScopes())
    return chooseEffectiveScope(scopes)

chooseEffectiveScope(scopes: Set<String>):
    if len(scopes) > 1: scopes.remove("system")
    if len(scopes) == 1:      return the single element
    elif "compile"  in scopes: return "compile"
    elif "runtime"  in scopes: return "runtime"
    elif "provided" in scopes: return "provided"
    elif "test"     in scopes: return "test"
    else:                      return ""
```

Winner-scope selection rule, in words:

1. **System is absolute.** If the winner's own scope is `system`, that is the effective scope; no
   further consideration.
2. **The depth ≤ 1 short-circuit.** Scanning items in DFS order, the **first** item at depth 0 or 1
   (the root itself, or a direct dependency of the root) ends the scan immediately and its
   **declared scope** (`item.getDependency().getScope()`, *not* its derived scopes) becomes the
   effective scope. A direct dependency's scope is authoritative and is never widened by transitive
   paths. This is why the `common-misconceptions.md` example prints guava as `test` even though
   guice pulls it in at `compile`.
3. **Otherwise, widest wins** over the union of *all derived scopes of all items* (each item may
   contribute several, §4.5). `system` is dropped from the union whenever more than one scope is
   present. If exactly one scope remains it is used verbatim (even a non-standard one). Otherwise the
   priority order is **compile > runtime > provided > test**, and `""` if none of them is present.

Note that step 3 uses the accumulated `item.getScopes()` sets, so a dependency reached at `compile`
through one path and at `runtime` through another comes out `compile`.

### 7.3 `SimpleOptionalitySelector` — see §8

---

## 8. `SimpleOptionalitySelector`

```
selectOptionality(context):
    optional = true
    for item in context.getItems():                     # DFS insertion order
        if item.getDepth() <= 1: return item.getDependency().isOptional()   # SHORT-CIRCUIT
        if (item.getOptionalities() & OPTIONAL_FALSE) != 0: optional = false
    return optional
context.setOptional(result)
```

The rule for the surviving node's `optional` flag:

* If any item is at depth ≤ 1 — the first such item in DFS order — the winning node's optional flag
  is that item's **own declared** optional flag, full stop. (A direct dependency's optionality is
  authoritative.)
* Otherwise the node is optional **iff every occurrence, on every walked path, was derived as
  optional**. A single non-optional occurrence (`OPTIONAL_FALSE` bit set on any item) makes the
  winner non-optional.
* With no items the loop cannot run (the driver only calls the selector when `items` is non-empty),
  so the vacuous `true` is unreachable in practice.

Recall from §4.6 that "derived as optional" means: declared optional, **or** optional via dependency
management (`MANAGED_OPTIONAL`), **or** inherited from a parent whose derived optionality was true.

---

## 9. `JavaDependencyContextRefiner`

```
transformGraph(node, ctx):
    if node.getRequestContext() == "project":
        s = buildpathScope(node)
        if s is not None: node.setRequestContext("project/" + s)
    for child in node.getChildren(): transformGraph(child, ctx)      # unconditional recursion
    return node

buildpathScope(node):
    if node.dependency is None: return None
    match node.dependency.scope:
        "compile" | "system" | "provided" -> "compile"
        "runtime"                         -> "runtime"
        "test"                            -> "test"
        _                                 -> None
```

* It runs **after** conflict resolution, so it reads the effective scopes.
* It only touches nodes whose request context is exactly `"project"` — that is the context set by
  Maven Core when collecting a project's dependencies. Nodes collected under another context (e.g.
  plugin dependency resolution) are untouched.
* Effect: the request context becomes `project/compile`, `project/runtime` or `project/test`. This
  is used downstream as part of the repository "request key", so it matters for
  workspace/reactor readers and for caching artifact-descriptor and version-range results per
  build path — two different buildpaths do not share cached resolution decisions. It does **not**
  affect the shape of the graph or `dependency:tree` output.
* **No cycle guard.** The recursion is unconditional. That is safe only because verbosity `NONE` and
  `STANDARD` remove cycles. Running this transformer on a `FULL` graph will not terminate.
* **`[M3≠M4]`** Maven 4 uses `ManagedDependencyContextRefiner`, identical in shape but resolving the
  buildpath through the `ScopeManager` (`getDependencyScopeMainProjectBuildScope`) instead of the
  hard-coded four-way match.

---

## 10. `Verbosity` — configuration, retained state, and `dependency:tree -Dverbose`

### 10.1 Configuration

Property: `aether.conflictResolver.verbose` (`ConflictResolver.CONFIG_PROP_VERBOSE`).
`ConflictResolver.getVerbosity(session)` accepts:

| Config value | Verbosity |
|---|---|
| absent / `null` | `NONE` |
| `Boolean.TRUE` | `STANDARD` |
| `Boolean.FALSE` | `NONE` |
| `String` | `Boolean.parseBoolean(s) ? STANDARD : NONE` (so anything other than a case-insensitive `"true"` is `NONE`) |
| a `Verbosity` enum instance | that value |
| anything else | `IllegalArgumentException("Unsupported Verbosity configuration: …")` |

`FULL` is therefore reachable only by putting the enum instance in the session's config properties;
it is not reachable from the CLI.

### 10.2 What each level retains

* **`NONE`** — nothing. No `NODE_DATA_*` is written anywhere, losers and cycles are gone. The graph
  is a resolvable tree.
* **`STANDARD`** — for each conflict id: on the **winner**, `NODE_DATA_ORIGINAL_SCOPE` and
  `NODE_DATA_ORIGINAL_OPTIONALITY` (the values before the effective scope/optionality were applied);
  for each **loser**, a childless copy carrying `NODE_DATA_WINNER` (a reference to the winner node),
  `NODE_DATA_ORIGINAL_SCOPE` and `NODE_DATA_ORIGINAL_OPTIONALITY`, and with its scope set to one
  arbitrary element of the item's derived-scope set. Redundant losers are then pruned by the pass-2
  rule (§5). Cycles are gone.
* **`FULL`** — as `STANDARD`, minus the pass-2 pruning: *every* loser copy survives, and cycles
  survive.

Node-data keys (string constants): `"conflict.winner"`, `"conflict.originalScope"`,
`"conflict.originalOptionality"`.

### 10.3 Mapping onto `mvn dependency:tree -Dverbose`

`-Dverbose` switches `TreeMojo` to the `DependencyCollectorBuilder` path, whose
`DependencyCollectorRequest` sets `ConflictResolver.CONFIG_PROP_VERBOSE = Boolean.TRUE` (→
**`STANDARD`**) and `DependencyManagerUtils.CONFIG_PROP_VERBOSE = true`, and installs the transformer
`ConflictResolver(NearestVersionSelector, VerboseJavaScopeSelector, SimpleOptionalitySelector,
JavaScopeDeriver)`. Without `-Dverbose` the plugin uses the project's own session, i.e. `NONE`.

`VerboseDependencyNode.toNodeString()` renders, in this order:

| Printed text | Source |
|---|---|
| surrounding `( … )` | the node has `NODE_DATA_WINNER` — i.e. it is a **loser copy** kept by `STANDARD` |
| `version managed from <v>` | `DependencyManagerUtils` premanaged version (dependency management), **not** the conflict resolver |
| `scope managed from <s>` | `DependencyManagerUtils` premanaged scope, **not** the conflict resolver |
| `scope updated from <s>` | `ConflictData.originalScope`, conceptually `NODE_DATA_ORIGINAL_SCOPE` on the **winner**. In maven-dependency-tree 3.x `setOriginalScope` is never called, so **this line is currently dead code** and does not appear in real output. |
| `scope not updated to <s>` | `VerboseJavaScopeSelector.REDUCED_SCOPE` on the winner (see below) |
| `omitted for duplicate` | loser copy whose artifact **version equals** `winner.artifact.getBaseVersion()` |
| `omitted for conflict with <v>` | loser copy whose version differs; `<v>` is `winner.artifact.getBaseVersion()` |

Separators: for an included (winning) node the annotations are wrapped as `" (" … "; " … ")"`; for an
omitted node as `" - " … "; "` inside the outer parentheses.

> jv follows this table exactly, including the dead row: `jv-resolver` records
> `Node::original_scope` because maven-resolver records it, and `jv-tree`
> declines to render it because maven-dependency-tree never populates its own
> copy. Rendering it is not a harmless extra — it annotates *every* node of an
> ordinary tree, since a scope derived during resolution differs from the
> declared one on almost every transitive dependency. The oracle harness now
> compares `-Dverbose` against real Maven, which is what would catch it coming
> back.

`VerboseJavaScopeSelector` wraps `JavaScopeSelector` unchanged and then computes the widest scope over
`⋃ item.getScopes()` under the ordering `compile > runtime > provided > test` (any scope not in that
list sorts as most-preferred, because `indexOf` returns −1); if that widest scope differs from the
scope just selected, it is stored on the winner node under key `"REDUCED_SCOPE"` and printed as
`scope not updated to <s>`. This is precisely the "a direct test dependency kept its `test` scope even
though a transitive path wanted `compile`" annotation.

> Implementation caveat: the Java filter is `s != context.getScope()` — **reference** inequality. It
> works in practice because the derived scopes come from interned `JavaScopes` literals, but a Rust
> port should compare by value; the only divergence would be a spurious annotation when two equal
> strings are distinct objects, which value comparison correctly suppresses.

Loser copies are childless, so a verbose tree shows the omitted node as a leaf. `dependency-graph.md`
and `common-misconceptions.md` document the resulting shape; the guice/guava example there is the
canonical `omitted for duplicate` + `scope not updated to compile` case.

---

## 11. Maven 3.9 vs Maven 4 differences

| # | Area | Maven 3.9 (jv's target) | Maven 4 (this clone) |
|---|---|---|---|
| 1 | Chain | `ConflictResolver(...)` then `JavaDependencyContextRefiner` | `TypeCollector`, `ConflictResolver(...)`, `ManagedDependencyContextRefiner`, `TypeDeriver` |
| 2 | Version selector | `NearestVersionSelector` | `ConfigurableVersionSelector` with the `Nearest` strategy — **behaviourally identical** predicate |
| 3 | Scope selector | `JavaScopeSelector` (hard-coded `compile > runtime > provided > test`) | `ManagedScopeSelector(ScopeManager)` — same shape, priority derived from computed scope *widths* |
| 4 | Scope deriver | `JavaScopeDeriver` (the fixed table in §7.1) | `ManagedScopeDeriver(ScopeManager)` — "narrowest of parent and child by width, unless the child is `system`" |
| 5 | Context refiner | `JavaDependencyContextRefiner` | `ManagedDependencyContextRefiner` |
| 6 | Resolver class | algorithm inline in `ConflictResolver` (Resolver 1.9.x) | split into `ClassicConflictResolver`; `ConflictResolver` is a dispatching facade with `aether.conflictResolver.impl` (`auto`→`classic`, or `path`) |
| 7 | Optionality selector | `SimpleOptionalitySelector` | `SimpleOptionalitySelector` — **identical** |
| 8 | `ConflictMarker`, `ConflictIdSorter` | identical | identical |

**Scope-selection equivalence (#3).** For `Maven3ScopeManagerConfiguration`, the computed widths
(`ScopeManagerImpl.calculateDependencyScopeWidth`) order the scopes descending as
`compile > runtime ≈ system > provided > test`. Because `system` is stripped from the set whenever
more than one scope is present, `ManagedScopeSelector` under the Maven 3 configuration produces the
same answers as `JavaScopeSelector`.

**Scope-derivation divergence (#4).** The same widths applied to `ManagedScopeDeriver`'s
"narrowest of parent/child, child wins ties, child `system` short-circuits" rule disagree with the
Maven 3 table in these cells:

| parent | child | Maven 3 (`JavaScopeDeriver`) | Maven 4 (`ManagedScopeDeriver` + Maven3 config) |
|---|---|---|---|
| `runtime` | `provided` | `runtime` | `provided` |
| `system` | `compile` | `provided` | `system` |
| `system` | `runtime` | `provided` | `runtime` |

All other cells agree. These three are largely academic (`provided` children are pruned below depth 1
by the default selector, and `system` artifacts have no transitive dependencies), but jv must
implement the **Maven 3 column**. The widths above were computed by hand from
`calculateDependencyScopeWidth` and `Maven3ScopeManagerConfiguration`; confirm with
`ScopeManagerDump.dump(Maven3ScopeManagerConfiguration.INSTANCE)` before relying on the Maven 4
column for anything.

Also Maven-4-only and not part of jv's target: `PathConflictResolver` (an O(N) alternative, not the
default and explicitly "no guarantees"), the `TypeCollector`/`TypeDeriver` artifact-type transformers,
and `Maven4ScopeManagerConfiguration` (which adds new scopes and project paths).

---

## 12. Implementation checklist (Rust port targeting Maven 3.9)

**Data model**

- [ ] Node identity is a stable handle (arena index / `Rc` pointer). Never compare nodes by value.
- [ ] **Child lists have their own identity**, separate from node identity, and can be shared between
      nodes. `infos`, `stack` and `ConflictItem.parent` are all keyed by the child-list handle.
- [ ] `Dependency` is immutable; `set_scope`/`set_optional` replace it while preserving node identity.
- [ ] Nodes carry: `dependency`, `artifact`, `version`, `version_constraint`, `managed_bits`,
      `relocations`, `aliases`, `request_context`, `children`, and an untyped `data` map.
- [ ] `managed_bits` must at least expose `MANAGED_SCOPE` and `MANAGED_OPTIONAL`.

**ConflictMarker**

- [ ] Key = `(group_id, artifact_id, extension, classifier)`; version excluded; classifier and
      extension included verbatim (`""` and `"jar"` defaults applied by the artifact-type registry
      *before* this point).
- [ ] Union relocations and aliases into the node's key set and merge groups transitively.
- [ ] Assign group indices in DFS-first-encounter order, incrementing on every new `ConflictGroup`
      object including intermediate merged ones; ids are the decimal strings of the final index.
- [ ] Identity-keyed `conflict_ids: HashMap<NodeHandle, ConflictIdString>`.

**ConflictIdSorter**

- [ ] Build the id DAG with a globally identity-visited DFS; add the edge even for visited children.
- [ ] `min_depth` via initial depth + recursive `pullup`.
- [ ] Topological sort with a queue that is **stably insertion-sorted by ascending `min_depth`**.
- [ ] Cycle break-out loop: pick `argmin (min_depth, in_degree)` among ids with `in_degree > 0`,
      force `in_degree = 0`, re-drain.
- [ ] `findCycles` heuristic; cycles are unordered `HashSet<ConflictId>` values.
- [ ] **Always write `CYCLIC_CONFLICT_IDS`**, empty when acyclic.
- [ ] Decide whether bit-exact tie-breaking matters; if so, emulate `java.util.HashSet` iteration
      order for `ConflictId.children` (§3.4).

**ClassicConflictResolver**

- [ ] Build `cyclic_predecessors: Map<Id, Set<Id>>` where each id in a cycle maps to the whole cycle
      *including itself*.
- [ ] `potential_ancestor_ids` and `resolved_ids` persist across the whole run; `items` and `infos`
      are cleared per conflict id.
- [ ] `resolved_ids` must distinguish "absent" from "present with no winner"
      (`HashMap<Id, Option<NodeHandle>>`) — §4.1.
- [ ] DFS: match current id → record item and **do not recurse**; leftover loser → remove from the
      parent's list; otherwise `push` / recurse / `pop`.
- [ ] `push` prunes on: id not in `potential_ancestor_ids`; child list already on the DFS stack;
      `NodeInfo.update` reporting no change. Missing conflict id + non-null dependency is an error
      **unless** the node carries `NODE_DATA_WINNER` (a loser copy).
- [ ] `NodeInfo.update` lowers `min_depth` **unconditionally**, before the change computation.
- [ ] Re-visiting a parent pushes `None` into `parent_infos` (no new items) and retro-updates the
      already-registered child items' scope sets / optionality bits, in reverse order, *after* the new
      parent scope/optional have been pushed.
- [ ] `ConflictItem.depth = 0` if parent is `None`, else `infos[parent].min_depth + 1`, assigned only
      after the whole DFS completes.
- [ ] Derivation shortcuts: `MANAGED_SCOPE` / `MANAGED_OPTIONAL`, and `resolved_ids.contains_key(cid)`
      with `cid` non-`None`.
- [ ] Write `NODE_DATA_ORIGINAL_SCOPE` / `_OPTIONALITY` on the winner *before* overwriting, when
      verbosity ≠ `NONE`.
- [ ] Final cycle-cleanup walk after the last id, rooted at the **last winner**, with `current_id = ""`.

**removeLosers**

- [ ] Two passes; pass 2 only for `STANDARD` and only when `to_remove_ids` is non-empty.
- [ ] Reuse one cursor across *consecutive* items sharing a parent; reset it (to position 0) only
      when `item.parent` changes (identity comparison).
- [ ] `NONE`: remove. `STANDARD`/`FULL`: replace with a childless annotated copy and rewrite
      `item.node` to the copy.
- [ ] Copy semantics: everything except children (empty), plus a shallow clone of the data map.
- [ ] `to_remove_ids` uses `group:artifact:extension[:classifier]:version`; the winner's own id is
      never added.
- [ ] Pass-2 removal condition: id in `to_remove_ids` **and** live `group:artifact` sibling count in
      the parent list `> 1`; evaluate against the mutating list, in item order.

**Selectors**

- [ ] Nearest-wins with the strict `isNearer` predicate; siblings compare versions, non-siblings
      compare depths; ties keep the incumbent (first in DFS order).
- [ ] Hard constraints (declared ranges) accumulate; backtrack when a new range invalidates the
      incumbent; fail with an unsolvable-conflict error when backtracking empties the candidate set.
- [ ] `JavaScopeDeriver`: implement §7.1 exactly, including `null`/`""` parent equivalence and the
      unreachable `runtime` fallback.
- [ ] `JavaScopeSelector`: winner-`system` short-circuit; then the **depth ≤ 1** short-circuit
      returning the item's *declared* scope; then widest-of-union with `system` dropped when the union
      has more than one member.
- [ ] `SimpleOptionalitySelector`: depth ≤ 1 short-circuit returning the item's *declared* optional
      flag; otherwise optional iff no item has the `OPTIONAL_FALSE` bit.
- [ ] `JavaDependencyContextRefiner`: only for request context exactly `"project"`; never run it on a
      `FULL` graph.

**Verbosity plumbing**

- [ ] Three levels; parse the config property per §10.1.
- [ ] Node-data keys `conflict.winner`, `conflict.originalScope`, `conflict.originalOptionality`.
- [ ] To reproduce `dependency:tree -Dverbose`, use `STANDARD`, keep premanaged version/scope data,
      and implement the `REDUCED_SCOPE` computation of §10.3.

**Testing**

- [ ] Golden-file the six annotation strings of §10.3 against real `mvn dependency:tree -Dverbose`
      output.
- [ ] Cover: diamond with equal depth (first-declared wins); diamond with unequal depth (nearest
      wins); two versions as siblings under one parent (highest wins); a version range expanded into
      siblings (pass-2 pruning); a `test` direct dependency also reached at `compile` transitively
      (depth ≤ 1 short-circuit + `scope not updated to compile`); relocation and alias merging;
      a cyclic graph (final cleanup walk); a `system`-scoped winner.
