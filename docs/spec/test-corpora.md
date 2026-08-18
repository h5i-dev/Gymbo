# Maven Resolver Test Corpora — Format Specification

Reference for a Rust implementer consuming Apache Maven Resolver's collection/transformation/visitor
test corpora without reading the Java.

## Provenance

| | |
|---|---|
| Upstream | [Apache Maven Resolver](https://github.com/apache/maven-resolver) |
| License | Apache License 2.0 (corpus files and the Java described here are ASF-licensed) |
| Local clone | `_reference/maven-resolver` |
| Clone commit | `ed4a939a850b73d9a85722c277da9de14b64f1e0` |

Java sources this document was derived from (paths relative to the clone root):

| Role | Path |
|---|---|
| `.ini` lexer/parser | `maven-resolver-test-util/src/main/java/org/eclipse/aether/internal/test/util/IniArtifactDataReader.java` |
| Coordinate line grammar | `maven-resolver-test-util/src/main/java/org/eclipse/aether/internal/test/util/ArtifactDefinition.java` |
| Coordinate → file mapping, relocation loop | `maven-resolver-test-util/src/main/java/org/eclipse/aether/internal/test/util/IniArtifactDescriptorReader.java` |
| Parsed-description value object | `maven-resolver-test-util/src/main/java/org/eclipse/aether/internal/test/util/ArtifactDescription.java` |
| Impl-side subclass (adds nothing) | `maven-resolver-impl/src/test/java/org/eclipse/aether/internal/impl/IniArtifactDescriptorReader.java` |
| Collector test driver | `maven-resolver-impl/src/test/java/org/eclipse/aether/internal/impl/collect/DependencyCollectorDelegateTestSupport.java` |
| DF collector test | `maven-resolver-impl/src/test/java/org/eclipse/aether/internal/impl/collect/df/DfDependencyCollectorTest.java` |
| BF collector tests | `maven-resolver-impl/src/test/java/org/eclipse/aether/internal/impl/collect/bf/{BfWithSkipperDependencyCollectorTest,BfWithoutSkipperDependencyCollectorTest,DependencyResolutionSkipperTest}.java` |
| Version-range stub | `maven-resolver-impl/src/test/java/org/eclipse/aether/internal/impl/StubVersionRangeResolver.java` |
| Transformer tests | `maven-resolver-util/src/test/java/org/eclipse/aether/util/graph/transformer/*.java` |
| Visitor tests | `maven-resolver-util/src/test/java/org/eclipse/aether/util/graph/visitor/*.java` |

**Out of scope by request.** The graph DSL implemented by `DependencyGraphParser.java` and
`NodeDefinition.java` is specified separately. This document describes the corpora *around* it and
references the DSL only where a corpus file's shape depends on it (§3). Where the two disagree, the
separate DSL spec wins.

---

## 1. The `.ini` artifact-description format

An `.ini` file is the stub stand-in for a POM: it declares what a *single* artifact version resolves
to. It is read as UTF-8, line by line.

### 1.1 Line-level lexing

Applied in this order to every physical line, before anything else:

1. **Comment strip.** The first `#` anywhere on the line, and everything after it, is deleted. There
   is no escape. A `#` inside a repository URL will truncate that URL.
2. **Empty check.** If the remainder is the empty string, the line is skipped. Note this test is
   `isEmpty()`, *not* `isBlank()` — a line of only spaces is *not* empty and falls through to step 3
   or 4.
3. **Section header.** If the (uncommented, untrimmed) remainder `startsWith("[")`, it is a header.
   The name is taken as `substring(1, length - 1)` — i.e. the first and last characters are dropped
   unconditionally, with no check that the last one is `]`.
   - The name is then normalized: **all** `-` characters are removed, and the result is uppercased
     with `Locale.ENGLISH`.
   - The normalized name must be one of `RELOCATION`, `DEPENDENCIES`, `MANAGEDDEPENDENCIES`,
     `REPOSITORIES`. Anything else is a hard error (`unknown section: <line>`).
   - A repeated header **resets** that section's accumulated lines to empty; earlier content is
     silently dropped.
   - Because the line is not trimmed first, a leading space makes it a *data* line, and a trailing
     space makes the name `dependencies]` → unknown-section error.
4. **Data line.** Otherwise the line is `trim()`ed and appended to the current section's list. If no
   header has been seen yet, this is a hard error (`missing section: <line>`).

Since normalization removes hyphens and folds case, `[managedDependencies]`,
`[manageddependencies]`, `[managed-dependencies]` and `[MANAGED-DEPENDENCIES]` are all the same
section. All four spellings occur in the corpus.

### 1.2 Coordinate line grammar

The `[relocation]`, `[dependencies]` and `[managed-dependencies]` sections all use one coordinate
grammar (`ArtifactDefinition`):

```
gid ":" aid ":" ext ":" ver [ ":" scope [ ":" ("optional" | "!optional") ] ]
```

Split on `:` with no limit. Fewer than 4 fields is a hard error
(`Need definition like 'gid:aid:ext:ver[:scope]'`).

| Field | Index | Notes |
|---|---|---|
| groupId | 0 | |
| artifactId | 1 | |
| extension | 2 | This is the **extension**, not a classifier. Classifier is always `""`. |
| version | 3 | May be a range, e.g. `[1,3]` or `[1,9]`. Brackets contain no `:`, so splitting is safe. |
| scope | 4 | Optional. Defaults to `""` at this layer. |
| optional | 5 | Optional. Case-insensitive `optional` → `Some(true)`, `!optional` → `Some(false)`, anything else → `None`. |

The constructed artifact is always
`DefaultArtifact(groupId, artifactId, /*classifier*/ "", extension, version)`.

`ArtifactDefinition` additionally accepts a leading `(id)` marker and a bare `^ref` form, but the
`.ini` reader never produces either — those branches exist only because the type is shared with the
graph DSL. A `^ref` line in an `.ini` leaves every field `null` and yields a nonsense artifact; treat
it as unsupported.

### 1.3 Exclusion lines

Inside `[dependencies]` and `[managed-dependencies]` only, a line starting with `-` is an exclusion:

```
"-" gid ":" aid
```

The leading `-` is stripped and the rest split on `:`; index 0 is the groupId, index 1 is the
artifactId. **Classifier and extension are hard-coded to `"*"`.** A line with fewer than two fields
panics in Java (array index out of bounds) — reject it.

**Association rule.** Exclusions accumulate in a buffer. The buffer is attached to a dependency and
cleared *when the next coordinate line is seen* (or at end of section). Consequently exclusion lines
attach to the coordinate line **above** them. Exclusion lines appearing before any coordinate line in
a section are attached to the first coordinate line that follows.

**Storage semantics.** `Dependency` stores exclusions as an insertion-ordered, **deduplicated set**
(a `LinkedHashSet` snapshotted into an array; `Dependency.Exclusions` in
`maven-resolver-api/.../graph/Dependency.java`). Iteration yields insertion order, but `equals` is
`AbstractSet.equals` — order-**insensitive**. So a Rust port should preserve insertion order for
display, dedupe on insert, and compare as a set. An empty exclusion list is the empty set, not
`None`.

### 1.4 Per-section semantics

| Section | Cardinality | Produces |
|---|---|---|
| `[relocation]` | Only the **first** data line is used; extra lines ignored. Absent/empty ⇒ `None`. | An `Artifact`. Scope and optional fields, if present, are ignored. |
| `[dependencies]` | All lines, order preserved. | `Vec<Dependency>` |
| `[managed-dependencies]` | All lines, order preserved. | `Vec<Dependency>` |
| `[repositories]` | All lines, order preserved. | `Vec<RemoteRepository>` |

**Scope defaulting** differs between the two dependency sections:

| | `[dependencies]` (`managed = false`) | `[managed-dependencies]` (`managed = true`) |
|---|---|---|
| scope field absent or `""` | `"compile"` | `""` (kept as-is) |
| scope field present | used verbatim | used verbatim |
| optional field absent | `Some(false)` | `None` |
| optional field `optional` | `Some(true)` | `Some(true)` |
| optional field `!optional` | `Some(false)` | `Some(false)` |

In Java terms: unmanaged optionality is `Boolean.valueOf(Boolean.TRUE.equals(def.optional))`, so it
is never null; managed optionality is the raw tri-state, so "not specified" is distinguishable from
"specified false".

`Dependency` itself always stores a tri-state `Option<bool>` and **compares it with three-valued
equality** (`Objects.equals(optional, that.optional)`), so `None` ≠ `Some(false)` when trees are
compared (§3.3). Model the field as `Option<bool>` in both cases; the unmanaged path simply never
produces `None`. (`Dependency::is_optional()` is the convenience collapse
`matches!(optional, Some(true))`.)

Verified against `IniArtifactDataReaderTest` in
`maven-resolver-test-util/src/test/java/.../IniArtifactDataReaderTest.java`: `testDependencies`
asserts scope `"compile"` for an unmanaged line with no scope field, while `testManagedDependencies`
asserts scope `""` for the identical managed line.

**Repository lines** use `split(":", 3)` — at most 3 fields, so the URL keeps its colons:

```
id ":" type ":" url
```

Index 0 = repository id, index 1 = layout/type, index 2 = everything else (the URL).

### 1.5 Real examples from the corpus

`[relocation]` — `maven-resolver-impl/src/test/resources/artifact-descriptions/transitiveDepsUseRangesAndRelocationDirtyTree_cid_1.ini`:

```ini
[relocation]
transitiveDepsUseRangesAndRelocationDirtyTree:relocatedcid:ext:1

[dependencies]
```

`[dependencies]` with a range — `maven-resolver-impl/src/test/resources/artifact-descriptions/transitiveDepsUseRangesDirtyTree_aid_1.ini`:

```ini
[dependencies]
transitiveDepsUseRangesDirtyTree:bid:ext:1
transitiveDepsUseRangesDirtyTree:cid:ext:[1,3]
```

`[manageddependencies]` (no hyphen, lowercase) — `maven-resolver-impl/src/test/resources/artifact-descriptions/managed/gid_root_ver.ini`:

```ini
[dependencies]
gid:direct:ext:ver
[manageddependencies]
gid:direct:ext:must-be-ignored-for-maven-2-and-3-compat
gid:transitive-1:ext:managed-by-root
```

Note there is no blank line before `[manageddependencies]` — headers do not need separation.

Exclusions, `optional`, and `[repositories]` — `maven-resolver-test-util/src/test/resources/org/eclipse/aether/internal/test/util/ArtifactDataReaderTest.ini`
(the only files in the tree exercising those; both under `maven-resolver-test-util`):

```ini
[relocation]
gid:aid:ext:ver

[dependencies]
gid:aid:ext:ver:scope
-gid3:aid
-gid2:aid2
gid:aid2:ext:ver:scope:optional
gid:aid:ext:ver3:scope:optional
gid1:aid:ext:ver:scope5:optional

[managedDependencies]
gid:aid:ext:ver:scope
-gid3:aid
-gid2:aid2
gid:aid2:ext:ver:scope:optional
gid:aid:ext:ver3:scope:optional
gid1:aid:ext:ver:scope5:optional

[repositories]
id:type:protocol://some/url?for=testing
```

Here both `-gid3:aid` and `-gid2:aid2` attach to `gid:aid:ext:ver:scope` (the line above them), each
becoming `Exclusion(gid, aid, "*", "*")`. The repository line yields
`id = "id"`, `type = "type"`, `url = "protocol://some/url?for=testing"`.

`IniArtifactDataReaderTest#testResource` confirms this exact reading: each of the two dependency
sections yields **4** dependencies (6 non-comment lines minus the 2 exclusion lines); `deps[0]` holds
**2** exclusions, iterated `gid3:aid` then `gid2:aid2`, with classifier and extension both `*`;
`deps[1..=3]` hold **0** exclusions each; `deps[1]` and `deps[2]` are optional. The relocation is
`gid:aid:ext:ver` and the single repository is as above.

**Caution:** the `[dependencies]`-only files in `artifact-descriptions/` are the vast majority — of
615 `[dependencies]` headers across the whole tree there are only 9 managed-dependency headers, 5
relocation headers and 2 repository headers.

---

## 2. Coordinate → file mapping, and collector-test setup

### 2.1 The mapping rule

`IniArtifactDescriptorReader` is constructed with a `prefix` string. For a requested artifact it
computes, verbatim:

```
resource_name = format!("{}_{}_{}.ini", group_id, artifact_id, version)
classpath_key = prefix + resource_name
```

Key properties:

- **Extension and classifier are not part of the name.** `gid:aid:ext:ver` and `gid:aid:jar:ver`
  both map to `gid_aid_ver.ini`.
- **No escaping or sanitization.** Group/artifact/version go in literally, dots and hyphens
  included: `gid:b-alt:ext:1.0` → `gid_b-alt_1.0.ini`;
  `gid:direct:ext:managed-by-dominant-request` → `gid_direct_managed-by-dominant-request.ini`.
- The `prefix` is concatenated with no separator inserted, so it must carry its own trailing `/`.
- The resource is looked up on the JVM classpath; for a Rust port this is a filesystem lookup rooted
  at the corresponding `src/test/resources` directory.
- A missing file is an `IOException("cannot find resource: <name>")`, which the descriptor reader
  wraps into an `ArtifactDescriptorException`. Several tests depend on this (see §5.2).

By the time the reader is called, a version **range** has already been expanded to a concrete
version by the `VersionRangeResolver`, so `version` in the filename is always a point version. The
test stub (`StubVersionRangeResolver`) resolves any range by testing the integers `1..=9` against the
constraint, in ascending order, and returning those that fall inside; a non-range constraint returns
itself. That is why `[1,3]` yields exactly `1, 2, 3` and `[1,9]` yields `1..9`.

### 2.2 The relocation loop

`read_artifact_descriptor` loops:

```
artifact = request.artifact
loop {
    data = parse(prefix + name_of(artifact))          // may error → ArtifactDescriptorException
    if let Some(reloc) = data.relocation {
        result.relocations.push(artifact);            // the *requested* artifact is recorded
        result.artifact = reloc.clone();
        artifact = reloc;                             // and we go around again
    } else {
        result.artifact           = artifact;
        result.dependencies       = data.dependencies;
        result.managed_dependencies = data.managed_dependencies;
        result.repositories       = data.repositories;
        return result;
    }
}
```

A relocating descriptor's `[dependencies]` / `[managed-dependencies]` / `[repositories]` sections are
**never read** — control transfers wholly to the relocation target. There is no cycle guard; a
relocation cycle in the corpus would hang.

### 2.3 How the collection tests set up the root

`DependencyCollectorDelegateTestSupport` (`@BeforeEach setup()`):

```java
session    = TestUtils.newSession();
parser     = new DependencyGraphParser("artifact-descriptions/");
repository = new RemoteRepository.Builder("id", "default", "file:///").build();
collector  = setupCollector(newReader(""));                 // prefix "artifact-descriptions/"
```

- `newReader(p)` returns `new IniArtifactDescriptorReader("artifact-descriptions/" + p)`. So the
  default prefix is `artifact-descriptions/`; individual tests re-point it with
  `newReader("managed/")`, `newReader("cycle-big/")`, `newReader("versionless-cycle/")`,
  `newReader("dependencies-empty/")` or `newReader("pool-cache-transparency/")`.
- The golden-tree parser prefix defaults to `artifact-descriptions/` and is likewise re-pointed to
  `artifact-descriptions/managed/` and `artifact-descriptions/pool-cache-transparency/` by the tests
  that need subdirectory goldens.
- Roots are established in one of three shapes:
  1. `new CollectRequest(dependency, [repository])` — a **dependency root**; the root node carries a
     `Dependency`. Most tests. The dependency is usually `root.getDependency()` of the parsed golden
     tree, so the golden file's first line *is* the request.
  2. `new CollectRequest(vec![dep1, dep2], null, [repository])` — a **rootless / multi-root**
     request; the result root has `getDependency() == null` and the listed dependencies as children.
  3. `CollectRequest().setRootArtifact(artifact).addDependency(...)` — a **root-artifact (POM)**
     request; again `root.getDependency() == null`, but `root.getArtifact()` is set. The support
     class converts such a result back into a dependency-rooted tree (`toDependencyResult`) before
     comparing with a golden.
- Collectors under test: `DfDependencyCollector`; `BfDependencyCollector` with
  `CONFIG_PROP_SKIPPER = false`; `BfDependencyCollector` with `CONFIG_PROP_SKIPPER = true`. All three
  run the same inherited test bodies. `StubRemoteRepositoryManager` and `StubVersionRangeResolver`
  are used throughout, and the artifact-descriptor reader is always the `.ini` reader (except one
  test that injects an inline recording reader).

---

## 3. The golden `.txt` trees under `artifact-descriptions/`

These files use the same graph DSL as the `transformer/` and `visitor/` corpora — see the separate
DSL spec for the authoritative grammar. What follows is the working description needed to read the
nine files in this directory.

### 3.1 Line shape

```
[indent][art] gid:aid[:ext]:ver [scope] [flags] [(id)]
[indent][art] ^ref
```

- **Root line** has *zero* indentation and is the only such line in the file.
- **Indent/prefix art.** One nesting level is exactly **three characters**. The parser derives depth
  by character distance from the start of the line; the specific ASCII art occupying those columns is
  cosmetic. In practice the corpus uses `+- ` / `\- ` for a child and `|  ` / `   ` for a pass-through
  column, but this is **not enforced** — `managed/management-tree.txt` uses `+-` at every level and
  still nests correctly, and `expectedSubtreeOnDescriptorDependenciesEmptyLeft.txt` mixes `\-` at
  depth 1 with `|  +-` at depth 2. Do not validate the art; count columns.
- **Coordinates** are the graph-DSL form, which allows both 3-field `gid:aid:ver` (extension
  defaults) and 4-field `gid:aid:ext:ver`. Files in `artifact-descriptions/` all use the 4-field
  form, e.g. `gid:aid2:ext:ver` and `cycle:a:jar:1`.
- **Scope** is the whitespace-separated token after the coordinates, e.g. `compile`. The root line
  may omit it — `expectedSubtreeComparisonResult.txt` line 1 is `subtree:comparison:ext:ver` with no
  scope, whereas `managed/management-tree.txt` line 1 is `gid:root:ext:ver compile`.
- **`(id)` markers.** A trailing parenthesized token names the node so it can be referenced later.
  Whitespace before it is free-form; `artifact-descriptions/cycle.txt` pads to a column.
- **`^id` back-references.** A line whose body is `^` followed by a previously declared id *reuses
  that same node object* as a child here, producing a shared subgraph (and, in these files, a cycle).
- Comments (`#` to end of line) and blank lines are permitted, as is a missing trailing newline
  (several corpus files lack one).

Verified against `artifact-descriptions/cycle.txt`:

```
cycle:root:jar:1
+- cycle:a:jar:1 compile          (a)
|  \- cycle:b:jar:1 compile
|     \- cycle:c:jar:1 compile
|        \- ^a
\- cycle:b:jar:1 compile          (b)
   \- cycle:c:jar:1 compile
      \- cycle:a:jar:1 compile
         \- ^b
```

Root has no scope; `(a)`/`(b)` are ids; `^a`/`^b` close the two cycles. Measured indent columns
(position of the first character that is neither a space nor `|`) run
`0, 0, 3, 6, 9, 0, 3, 6, 9` down the nine lines, i.e. depths `0, 1, 2, 3, 4, 1, 2, 3, 4` — the root
and a depth-1 child both start at column 0, and every further level adds exactly 3.

### 3.2 `_BF` / `_DF` suffixes

Two test methods are declared in the shared support class but read a per-collector resource name
supplied by an abstract getter:

| Golden | Read by | Collector |
|---|---|---|
| `transitiveDepsUseRangesDirtyTreeResult_DF.txt` | `DfDependencyCollectorTest` | `DfDependencyCollector` (depth-first) |
| `transitiveDepsUseRangesDirtyTreeResult_BF.txt` | `BfWithSkipperDependencyCollectorTest`, `BfWithoutSkipperDependencyCollectorTest` | `BfDependencyCollector` (breadth-first), both skipper settings |
| `transitiveDepsUseRangesAndRelocationDirtyTreeResult_DF.txt` | `DfDependencyCollectorTest` | DF |
| `transitiveDepsUseRangesAndRelocationDirtyTreeResult_BF.txt` | both BF tests | BF |

**Why they differ — version-range iteration order.** When a dependency's version is a range, the
range resolver returns candidate versions and the collector expands one child per candidate into the
*dirty* (pre-conflict-resolution) tree.

- **DF** iterates `filterVersions(...)` in the order the resolver returned them — for
  `StubVersionRangeResolver`, ascending (`1, 2, 3`).
- **BF** calls `Collections.reverse(versions)` immediately after filtering ("resolve newer version
  first to maximize benefits of skipper"), so it iterates descending (`3, 2, 1`) and preserves that
  order in the emitted children.

Hence, for `transitiveDepsUseRangesDirtyTree:aid:ext:1` whose descriptor requests `cid:ext:[1,3]`:

```
# _DF.txt                                      # _BF.txt
...:aid:ext:1 compile                          ...:aid:ext:1 compile
+- ...:bid:ext:1 compile                       +- ...:bid:ext:1 compile
|  +- ...:cid:ext:1 compile                    |  +- ...:cid:ext:3 compile
|  +- ...:cid:ext:2 compile                    |  +- ...:cid:ext:2 compile
|  \- ...:cid:ext:3 compile                    |  \- ...:cid:ext:1 compile
+- ...:cid:ext:1 compile                       +- ...:cid:ext:3 compile
+- ...:cid:ext:2 compile                       +- ...:cid:ext:2 compile
\- ...:cid:ext:3 compile                       \- ...:cid:ext:1 compile
```

Same node multiset, reversed sibling order. The relocation pair is starker — the range candidates all
relocate onto `relocatedcid`, so after the descriptor round-trip only the first candidate survives as
a child, and "first" is version 1 for DF and version 3 for BF:

```
# _DF.txt
transitiveDepsUseRangesAndRelocationDirtyTree:aid:ext:1 compile
\- transitiveDepsUseRangesAndRelocationDirtyTree:relocatedcid:ext:1 compile

# _BF.txt
transitiveDepsUseRangesAndRelocationDirtyTree:aid:ext:1 compile
+- transitiveDepsUseRangesAndRelocationDirtyTree:relocatedcid:ext:3 compile
```

Note this pair also demonstrates that the prefix art carries no meaning: `\-` and `+-` here both
denote a single depth-1 child.

### 3.3 Textual or structural?

**Structural.** `assertEqualSubtree(expected, actual)` in `DependencyCollectorDelegateTestSupport`
never renders either tree. It recursively compares:

1. `expected.getDependency()` vs `actual.getDependency()` with `Dependency.equals`, which compares
   `artifact`, `scope`, `optional` and `exclusions`. `Artifact` equality (from `AbstractArtifact`)
   compares `groupId`, `artifactId`, `version`, `extension`, `classifier`, `path` and `properties` —
   note `properties` is included, which is how the dependency-management `LOCAL_PATH` assertions bite.
2. Then, **cycle short-circuit**: if the *actual* node's artifact equals the artifact of any node
   already on the ancestor stack, the comparison returns successfully without descending. This is
   what lets a finite golden file describe an infinite `^ref` cycle.
3. Otherwise `expected.getChildren().size() == actual.getChildren().size()`, then pairwise recursion
   over children **in order**.

So: node identity, scope, optionality, exclusions, artifact properties, child count, and child order
are all significant; whitespace, prefix art, id naming and the choice of `+-` vs `\-` are not.
`DependencyGraphParser.dump()` exists but is only used to build assertion failure messages.

---

## 4. Corpus inventory

### 4.1 `maven-resolver-util/src/test/resources/transformer/`

| Directory | Files | Behaviour exercised | Driving test(s) |
|---|---:|---|---|
| `conflict-id-sorter/` | 4 | `ConflictIdSorter` topological ordering of conflict groups, and cycle detection among them | `ConflictIdSorterTest` |
| `conflict-marker/` | 4 | `ConflictMarker` grouping of nodes into conflict ids, including via relocations | `ConflictMarkerTest` |
| `optionality-selector/` | 3 | `SimpleOptionalitySelector`: optionality derivation and conflict resolution | `SimpleOptionalitySelectorTest` |
| `scope-calculator/` | 13 (11 used) | `JavaScopeSelector` + `JavaScopeDeriver`: scope inheritance, mediation, direct-node dominance, cycles | `JavaScopeSelectorTest` |
| `version-resolver/` | 18 | `NearestVersionSelector` / `ConfigurableVersionSelector`: nearest-wins, range backtracking, unsolvable hard constraints, cycles, verbose mode | `NearestVersionSelectorTest` (16 files), `ConfigurableVersionSelectorTest` (16 files) |
| `version-resolver-strategies/` | 3 | Graphs where `ConfigurableVersionSelector.Nearest` and `.Highest` disagree | `ConfigurableVersionSelectorStrategiesTest` |

Files present but referenced by **no** Java source: `scope-calculator/system-1.txt`,
`scope-calculator/system-2.txt` (they model `system`-scope mediation; dead corpus at this commit).

`version-resolver/` split by driver:

| File | `NearestVersionSelectorTest` | `ConfigurableVersionSelectorTest` |
|---|:-:|:-:|
| `sibling-versions.txt` | ✓ | ✓ |
| `sibling-major-versions.txt` | | ✓ (expects `UnsolvableVersionConflictException`) |
| `nearest-underneath-loser-a.txt` | ✓ | ✓ |
| `nearest-underneath-loser-b.txt` | ✓ | ✓ |
| `range-backtracking.txt` | ✓ | ✓ |
| `range-major-backtracking.txt` | | ✓ (expects `UnsolvableVersionConflictException`) |
| `conflict-id-cycle.txt` | ✓ | ✓ |
| `unsolvable.txt` | ✓ | ✓ |
| `unsolvable-with-cycle.txt` | ✓ | ✓ |
| `ranges.txt` | ✓ | ✓ |
| `dead-conflict-group.txt` | ✓ | ✓ |
| `soft-vs-range.txt` | ✓ | ✓ |
| `cycle.txt` | ✓ | ✓ |
| `loop.txt` | ✓ | ✓ |
| `overlapping-cycles.txt` | ✓ | ✓ |
| `scope-vs-version.txt` | ✓ | ✓ |
| `verbose.txt` | ✓ | ✓ |
| `expectedSubtreeOnDescriptorDependenciesEmptyLeft.txt` | ✓ | |

That last file is a *near*-duplicate of the same-named file under `artifact-descriptions/`: the
`transformer/version-resolver/` copy has three extra lines (a `gid:d:jar:2 → gid:g:jar:1 →
gid:h:jar:1` chain under `gid:b`). Do not assume the two are interchangeable.

Two parser prefixes are declared with **no corresponding directory** — those tests parse only string
literals: `transformer/context-refiner/` (`JavaDependencyContextRefinerTest`) and
`transformer/conflict-resolver/` (`ConflictResolverTest`).

#### `%s` substitutions

`DependencyGraphParser` replaces each `%s` occurrence, in file order, with the next string from the
substitution list set via `setSubstitutions(String...)`. Exactly **five** corpus files contain `%s`,
all in `scope-calculator/`, all driven by `JavaScopeSelectorTest`. The substituted values are the
lowercased names of the test-local `Scope` enum, whose declaration order is
`TEST, PROVIDED, RUNTIME, COMPILE` (this ordinal order is itself the expected mediation ranking:
higher ordinal wins).

| File | `%s` count | Exact substitution values passed | Assertion |
|---|:-:|---|---|
| `inheritance.txt` | 2 | `("provided", "test")` — one call only | scope at child path `[0,0]` is `"test"` |
| `direct-nodes-winning.txt` | 1 | four calls: `("test")`, `("provided")`, `("runtime")`, `("compile")` | scope at `[0]` equals the substituted value |
| `multiple-inheritance.txt` | 2 | all 16 ordered pairs `(s1, s2)` over `{test, provided, runtime, compile}²` | scope at `[0,0]` is `max(s1, s2)` by enum ordinal (`s1.compareTo(s2) >= 0 ? s1 : s2`) |
| `dueling-scopes.txt` | 2 | all 16 ordered pairs, same set | scope at `[0,0]` is `max(s1, s2)` by enum ordinal |
| `conflicting-direct-nodes.txt` | 2 | all 16 ordered pairs, same set | scope at `[0]` is `s1` (the first/nearest direct node wins outright) |

For reference, `inheritance.txt` is:

```
root:a:ver
\- gid:b:ver %s
   \- gid:c:ver %s
```

so with `("provided", "test")` the depth-1 node gets scope `provided` and the depth-2 node `test`.

No file under `visitor/` or `artifact-descriptions/` contains `%s`.

### 4.2 `maven-resolver-impl/src/test/resources/artifact-descriptions/`

Top level: **26 `.ini` + 9 `.txt` = 35 files**, plus 5 subdirectories.

| Location | Files | Behaviour exercised | Driving test(s) |
|---|---:|---|---|
| (top level) `.ini` | 26 | Simple collection, duplicate transitive deps, missing descriptor, cycles, version ranges, relocation, classic dependency management | all three collector tests via `DependencyCollectorDelegateTestSupport` |
| (top level) `.txt` | 9 | Golden dirty trees for the above (`cycle.txt`, `expectedSubtreeComparisonResult.txt`, `expectedPartialSubtreeOnError.txt`, `expectedSubtreeOnDescriptorDependenciesEmpty{Left,Right}.txt`, and the four `_BF`/`_DF` files) | same |
| `cycle-big/` | 556 `.ini` | A large real-world-shaped cyclic graph; a **performance/termination** guard only | `testCyclicDependenciesBig` (all three collector tests) |
| `dependencies-empty/` | 12 `.ini` | Sibling ordering when a descriptor has an empty `[dependencies]` section, with `ClassicConflictResolver` installed | `testDescriptorDependenciesEmpty` |
| `managed/` | 11 `.ini` + 2 `.txt` | `TransitiveDependencyManager` and `DefaultDependencyManager` propagation of managed versions down a 5-deep chain, incl. request-level managed deps | `testDependencyManagement*`, `BfWithSkipperDependencyCollectorTest#testSkipperWithDifferentExclusion` |
| `pool-cache-transparency/` | 5 `.ini` | Regression for [maven-resolver#2013](https://github.com/apache/maven-resolver/issues/2013): pool-cache key must not vary with `DependencyManager` identity, or the BF skipper drops a shared subtree | `BfWithSkipperDependencyCollectorTest#testPoolCacheTransparencyWithTransitiveDependencyManager` (BF-with-skipper only) |
| `versionless-cycle/` | 3 `.ini` | Cycle where the repeated artifact differs only in version; checks the reported `DependencyCycle` | `testCyclicProjects` |

The two `.txt` files in `managed/` are `management-tree.txt` (for `TransitiveDependencyManager`) and
`default-management-tree.txt` (for `DefaultDependencyManager`); they differ in exactly one line — the
`gid:direct` version is `ver` vs `managed-by-dominant-request`.

### 4.3 `maven-resolver-util/src/test/resources/visitor/`

| Directory | Files | Behaviour exercised | Driving test(s) |
|---|---:|---|---|
| `filtering/` | 1 (`parents.txt`) | `FilteringDependencyVisitor` passes the correct parent stack to the filter | `FilteringDependencyVisitorTest` |
| `ordered-list/` | 2 (`simple.txt`, `cycles.txt`) | Pre-/post-/level-order traversal orders, duplicate suppression, classpath & artifact/dependency/path list generation | `NodeListGeneratorTest`, `PreorderNodeListGeneratorTest`, `PostorderNodeListGeneratorTest`, `DependencyGraphDumperTest` |
| `path-recorder/` | 5 | `PathRecordingDependencyVisitor`: which root→match paths are recorded, nesting behaviour, cycle handling, parent stack | `PathRecordingDependencyVisitorTest` |
| `tree/` | 1 (`cycles.txt`) | `TreeDependencyVisitor` suppresses re-entry into an already visited node | `TreeDependencyVisitorTest` |

`filtering/parents.txt` and `path-recorder/parents.txt` are byte-identical; both are asserted by an
identically-shaped test in their respective visitor.

---

## 5. What a Rust harness must assert, per corpus

Throughout: an *expected tree* means the parsed golden `.txt`; a *result tree* means what your
collector/transformer/visitor produced. None of these tests compare rendered text.

### 5.1 `artifact-descriptions/` — collector, golden-tree cases

**Input.** Descriptor reader rooted at the directory (or subdirectory) named by the test; a single
`RemoteRepository { id: "id", type: "default", url: "file:///" }`; the stub range resolver of §2.1.

**Operation.** `collect_dependencies(session, request)` for each of the three collector
configurations (DF; BF skipper off; BF skipper on).

**Comparison.** `assert_equal_subtree(expected_root, result_root)` exactly as §3.3: recursive
`Dependency` equality (artifact GAV + extension + classifier + path + properties, scope, optional,
exclusions), ancestor-artifact cycle short-circuit, equal child counts, ordered pairwise recursion.

| Test | Root request | Golden / expectation |
|---|---|---|
| `testEqualSubtree` | root of `expectedSubtreeComparisonResult.txt` | that file, structurally |
| `testCyclicDependencies` | root of `cycle.txt` | that file, structurally |
| `testTransitiveDepsUseRangesDirtyTree` | root of the collector's own `_BF`/`_DF` golden | that file |
| `testTransitiveDepsUseRangesAndRelocationDirtyTree` | ditto | ditto |
| `testDescriptorDependenciesEmpty` | roots of `...EmptyLeft.txt` then `...EmptyRight.txt`, prefix `dependencies-empty/`, with `ClassicConflictResolver(NearestVersionSelector, JavaScopeSelector, SimpleOptionalitySelector, JavaScopeDeriver)` installed as the graph transformer | both files |
| `testDependencyManagement_TransitiveDependencyManager` | `gid:root:ext:ver` scope `compile`, prefix `managed/`, `TransitiveDependencyManager`, request-managed `gid:root:ext:must-retain-core-management` | `managed/management-tree.txt`; then repeated as a root-artifact request (adding managed `gid:direct:ext:must-retain-core-management` and `gid:transitive-1:ext:managed-by-root`, direct dep `gid:direct:ext:ver`), whose rootless result is re-wrapped as a `compile` dependency root before the same comparison |
| `testDependencyManagement_DefaultDependencyManager` | `gid:root:ext:ver` scope `compile`, prefix `managed/`, `DefaultDependencyManager`, request-managed `gid:root:ext:must-not-manage-root` and `gid:direct:ext:managed-by-dominant-request` | `managed/default-management-tree.txt`, plus the same root-artifact repeat |
| `testPartialResultOnError` | root of `expectedPartialSubtreeOnError.txt` (`subtree:comparison:ext:error`) | collection **must fail**; the exception's `CollectResult` has exactly 1 exception, of descriptor-read kind, and its partial root matches the golden structurally |

### 5.2 `artifact-descriptions/` — collector, no-golden cases (assert on state)

| Test | Input | Assertion (no textual form) |
|---|---|---|
| `testSimpleCollection` | `gid:aid:ext:ver` scope `compile` | 0 exceptions; root dependency equals the request; 1 child, equal to `gid:aid2:ext:ver` scope `compile` |
| `testMissingDependencyDescription` | `gid = missing, aid = description, ext, ver` with empty scope | fails; result's request is the same object; exactly 1 exception, a descriptor exception; result root's dependency equals the request root |
| `testDuplicates` | `duplicate:transitive:ext:dependency` | 0 exceptions; 2 children; `[0]` = `gid:aid:ext:ver compile`; `[1]` = `gid:aid2:ext:ver compile`; `[0][0]` equals `[1]` |
| `testCyclicDependenciesBig` | `1:2:pom:5.50-SNAPSHOT`, prefix `cycle-big/` | root is non-null; **the real assertion is that it terminates in bounded time and memory** (556 mutually-referencing descriptors). No structural check. |
| `testCyclicProjects` | `test:a:2`, prefix `versionless-cycle/` | node at path `[0,0]` has artifactId `a`, version `1`; none of its children has version `1`; `result.cycles.len() == 1`; that cycle's *preceding* dependencies are empty and its *cyclic* dependencies are exactly `[root.dep, path[0].dep, a1.dep]` |
| `testCyclicProjects_ConsiderLabelOfRootlessGraph` | root-artifact request for `gid:aid:ver`, one dependency `gid:aid:ver compile` | `[0]` is `aid`/`ver`, `[0][0]` is `aid2`/`ver`; 1 cycle, empty preceding, cyclic = `[Dependency(gid:aid:ver, scope = null), a1.dep]` |
| `testCollectMultipleDependencies` | rootless request with `gid:aid:ext:ver compile` and `gid:aid2:ext:ver compile` | 0 exceptions; 2 children; `[0]` = root1 with exactly 1 child = root2; `[1]` = root2 with 0 children |
| `testManagedVersionScope` | `managed:aid:ext:ver` (empty scope) with `ClassicDependencyManager(None)` | 0 exceptions; root dependency equals request; 1 child `gid:aid:ext:ver compile`, which has 1 child `gid:aid2:ext:managedVersion` scope `managedScope` |
| `testDependencyManagement` | prefix `managed/`; root = root of `expectedSubtreeComparisonResult.txt`; a test manager mapping (by versionless id) `[0]→version "managed"`, `[0,1]→version+scope "managed"`, `[1]→localPath "managed"` | `[0,1]` artifact version is `"managed"` and its scope is `"managed"`; `[1]` and `[0,0]` artifacts carry property `localPath = "managed"` |
| `testDependencyManagement_VerboseMode` | root `gid:aid:ver`, manager managing `gid:aid2:ext`'s version/scope/optional/path/exclusions, `DependencyManagerUtils.CONFIG_PROP_VERBOSE = true` | child `[0]`'s **managed bits** are exactly `MANAGED_VERSION \| MANAGED_SCOPE \| MANAGED_OPTIONAL \| MANAGED_PROPERTIES \| MANAGED_EXCLUSIONS`; premanaged version `"ver"`, premanaged scope `"compile"`, premanaged optional `false`. This is bitflag + side-table state with no textual rendering. |
| `testVersionFilter` | root `gid:aid:1` (descriptor requests `gid:aid2:ext:[1,9]`) with `HighestVersionFilter` | root has exactly 1 child (the filter collapses the 9 range candidates to one) |
| `testArtifactDescriptorResolutionNotRestrictedToRepoHostingSelectedVersion` | rootless request for `verrange:parent:jar:1[1,)` against repositories `id` and `test`, with a recording descriptor reader that returns empty results | 0 exceptions; the reader saw exactly 2 repositories, in order `id`, `test` |
| `testInterruption` | `gid:aid:ext:ver compile`, collected on a thread whose interrupt flag is already set | collection fails and the failure's cause is an interruption |
| `testPoolCacheTransparencyWithTransitiveDependencyManager` (BF+skipper only) | `gid:root:ext:1.0 compile`, prefix `pool-cache-transparency/`, `TransitiveDependencyManager` | 0 exceptions; root has 2 children `b`, `b-alt`; both non-empty; `b→c` non-empty with first child `d`; **`b-alt→c` must also have children, one of which is `d`** — the regression |
| `testSkipperWithDifferentExclusion` (BF+skipper only) | prefix `managed/`, rootless request with two `gid:root:ext:ver compile` deps differing only in exclusion (`gid:transitive-1` vs `gid:transitive-2`), `ExclusionDependencySelector`, `TransitiveDependencyManager`, request-managed `gid:direct:ext:managed-by-dominant-request` + `gid:transitive-1:ext:managed-by-root` | 0 exceptions; 2 children equal to root1 and root2 respectively; child `[0]` has 1 child which itself has 0 children; child `[1]` has 0 children (skipped) |

`DependencyResolutionSkipperTest` (BF only) drives no corpus file — it builds graphs in code and
asserts on the skipper's internal `DependencyResolutionResult` map: its size, and which entries carry
`skippedAsVersionConflict` / `skippedAsDuplicate` / force-resolution flags, identified by node
identity. If your port has no such introspection surface, this is the state to expose.

### 5.3 `transformer/conflict-marker/`

**Input.** Parse the file. **Operation.** Run the conflict marker over the root. **Comparison.** The
transformer returns the *same* root object, and the transformation context's `CONFLICT_IDS` entry is
a node→id map with:

| File | Assertion |
|---|---|
| `simple.txt` | root has no id; children `[0]` and `[1]` both have ids, and those ids are **neither identical nor equal** |
| `relocation1.txt` | root has no id; `[0]` and `[1]` have the **identical** id object |
| `relocation2.txt` | same as relocation1 |
| `relocation3.txt` | root has no id; `[0]`, `[1]`, `[2]` all have ids, and `[0] ≡ [1] ≡ [2]` (identical objects) |

Note the assertion is reference identity (`assertSame`) on the id values, not just equality — ids are
interned per conflict group.

### 5.4 `transformer/conflict-id-sorter/`

**Input.** Parse the file. **Operation.** Run `SimpleConflictMarker` then `ConflictIdSorter`.
`SimpleConflictMarker` assigns each node the string `"{groupId}:{artifactId}:{classifier}:{extension}"`
(classifier is empty, extension defaults to `jar` in the DSL — hence ids like `gid:aid::jar`).

**Comparison.** The context's `SORTED_CONFLICT_IDS` (a list of ids, in order) and
`CYCLIC_CONFLICT_IDS` (a collection; non-empty ⇔ a cycle was found). `*` below means "any id — only
the position count matters".

| File | Expected `SORTED_CONFLICT_IDS` (exact, in order) | Cycle? |
|---|---|:-:|
| `simple.txt` | `gid2:aid::jar`, `gid:aid::jar`, `gid:aid2::jar` | no |
| `cycle.txt` | `gid:aid::jar`, `gid2:aid::jar` | yes |
| `cycles.txt` | `*`, `*`, `*`, `gid:aid::jar` (i.e. exactly 4 groups, last one pinned) | yes |
| `no-conflicts.txt` | `gid:aid::jar`, `gid3:aid::jar`, `gid2:aid::jar`, `gid4:aid::jar` | no |

The list must contain *exactly* that many entries — a leftover entry is a failure.

### 5.5 `transformer/scope-calculator/`

**Input.** Parse the file with the substitutions of §4.1. **Operation.** Run a conflict resolver over
the root; every case is run twice, once with `ClassicConflictResolver` and once with
`PathConflictResolver`, both configured as
`(NearestVersionSelector, JavaScopeSelector, SimpleOptionalitySelector, JavaScopeDeriver)`.
The transformer must return the same root object.

**Comparison.** The `scope` string of the dependency at a fixed child-index path.

| File | Substitutions | Path → expected scope |
|---|---|---|
| `inheritance.txt` | `("provided", "test")` | `[0,0]` → `test` |
| `conflict-and-inheritance.txt` | none | `[0,0]` → `compile`; `[0,0,0]` → `compile` |
| `direct-with-conflict-and-inheritance.txt` | none | `[0,0]` → `test` |
| `cycle-a.txt` | none | `[0]` → `compile`; `[1]` → `runtime` |
| `cycle-b.txt` | none | `[0]` → `runtime`; `[1]` → `compile` |
| `cycle-c.txt` | none | `[0]`, `[0,0]`, `[1]`, `[1,0]` → all `runtime` |
| `cycle-d.txt` | none | `[0]` → `compile`; `[0,0]` → `compile` |
| `direct-nodes-winning.txt` | each of `test`/`provided`/`runtime`/`compile` | `[0]` → the substituted scope |
| `multiple-inheritance.txt` | all 16 `(s1, s2)` pairs | `[0,0]` → `max(s1, s2)` in the order `test < provided < runtime < compile` |
| `dueling-scopes.txt` | all 16 `(s1, s2)` pairs | `[0,0]` → same max rule |
| `conflicting-direct-nodes.txt` | all 16 `(s1, s2)` pairs | `[0]` → `s1` |

`system-1.txt` / `system-2.txt` have no driver; skip or port speculatively.

### 5.6 `transformer/optionality-selector/`

Same operation and dual-resolver matrix as §5.5. Assertions are on `Dependency::is_optional`:

| File | Assertion |
|---|---|
| `derive.txt` | 2 children; `[0]` optional and `[0,0]` optional; `[1]` not optional and `[1,0]` not optional |
| `conflict.txt` | 2 children; `[0]` optional; `[0,0]` **not** optional (non-optional wins the conflict) |
| `conflict-direct-dep.txt` | 2 children; `[1]` optional (the direct declaration wins) |

### 5.7 `transformer/version-resolver/` and `version-resolver-strategies/`

Same dual-resolver matrix (`version-resolver-strategies/` instead runs a 4-way matrix:
{`PathConflictResolver`, `ClassicConflictResolver`} × {`ConfigurableVersionSelector::Nearest`,
`::Highest`}). `find(root, artifact_id)` below returns the root→node trail, **root last**, so its
length is `depth + 1` and `trail[0]` is the found node.

| File | Assertion |
|---|---|
| `sibling-versions.txt` | 1 child; its version is `3` |
| `sibling-major-versions.txt` | must raise unsolvable-version-conflict (Configurable only) |
| `nearest-underneath-loser-a.txt` | `find(root, "j")` has length 5 |
| `nearest-underneath-loser-b.txt` | `find(root, "j")` has length 5 |
| `range-backtracking.txt` | `find(root, "x")` has length 3; `trail[0]` version is `2` |
| `range-major-backtracking.txt` | must raise unsolvable-version-conflict (Configurable only) |
| `conflict-id-cycle.txt` | 2 children `a`, `b`, both childless |
| `unsolvable.txt` | must raise unsolvable-version-conflict |
| `unsolvable-with-cycle.txt` | must raise unsolvable-version-conflict |
| `ranges.txt` | must **not** raise; no further assertion |
| `dead-conflict-group.txt` | 2 children `a`, `b`, both childless |
| `soft-vs-range.txt` | 2 children; `a` with 0 children, `b` with 1 child |
| `cycle.txt` | 2 children; `[0]` has 1 child; `[0][0]` has 0 children; `[1]` has 0 children |
| `loop.txt` | root has 0 children |
| `overlapping-cycles.txt` | root has 2 children |
| `scope-vs-version.txt` | `find(root, "y")` has length 3; `trail[1]` and `trail[0]` both have scope `test` |
| `expectedSubtreeOnDescriptorDependenciesEmptyLeft.txt` | `find(root, "h")` has length 5 (`h` survives) — Nearest only |
| `verbose.txt` | with verbose config on: 2 children; `[0]` has 1 child ("winner"), `[1]` has 1 child ("loser"). Winner: scope `test`, node-data `ORIGINAL_SCOPE = "compile"`, `ORIGINAL_OPTIONALITY = false`. Loser: scope `test`, 0 children, node-data `WINNER` is *the same object as* the winner node, `ORIGINAL_SCOPE = "compile"`, `ORIGINAL_OPTIONALITY = false`. **This node-data side table has no textual form; expose it.** |
| `nearest-highest-strategy-difference01.txt` | `find(root, "x")` has length 2 under **both** strategies (direct dependency wins either way) |
| `nearest-highest-strategy-difference02.txt` | Nearest: `find(root, "x")` length 3, version `1`. Highest: length 6, version `3`. |
| `nearest-highest-strategy-difference03.txt` | Nearest: `find(root, "annotations")` length 5, version `13.0`. Highest: length 7, version `13.0`. |

### 5.8 `visitor/`

**Input.** Parse the file. **Operation.** Accept the visitor under test at the root. **Comparison.**

| File | Visitor / operation | Exact expectation |
|---|---|---|
| `ordered-list/simple.txt` | preorder node list | artifactIds `a, b, c, d, e` |
| `ordered-list/simple.txt` | postorder node list | `c, b, e, d, a` |
| `ordered-list/simple.txt` | level-order node list | `a, b, d, c, e` |
| `ordered-list/simple.txt` | level-order, filter `parents.len() <= 1` | `a, b, d` |
| `ordered-list/simple.txt` | level-order, filter `parents.len() <= 2` | `a, b, d, c, e` |
| `ordered-list/simple.txt` | preorder, filter rejecting artifactId `a` | `b, c, d, e` |
| `ordered-list/simple.txt` | postorder, filter rejecting `a` | `c, b, e, d` |
| `ordered-list/simple.txt` | level-order, filter rejecting `a` | `b, d, c, e` |
| `ordered-list/cycles.txt` | preorder / postorder / level-order | `a, b, c, d, e` / `c, b, e, d, a` / `a, b, d, c, e` — i.e. duplicate suppression makes the cyclic graph produce the same sequences as `simple.txt` |
| `ordered-list/simple.txt` | list-generator accounting, no artifact paths set | 5 nodes; `dependencies(false).len() == 0`, `dependencies(true).len() == 5`; `artifacts(false).len() == 0`, `artifacts(true).len() == 5`; `paths().len() == 0`; classpath string is `""` |
| `ordered-list/simple.txt` | all artifacts given a path first | 5 for every one of the six counts; the classpath's basenames equal the set of node path basenames |
| `ordered-list/simple.txt` | alternating nodes given a path | 5 nodes; `dependencies(false) == 3`, `dependencies(true) == 5`; `artifacts(false) == 3`, `artifacts(true) == 5`; `paths() == 3`; classpath basenames equal the set of names that were set |
| `ordered-list/{simple,cycles}.txt` | graph dumper | smoke only — must not panic; output is printed, not asserted |
| `filtering/parents.txt` | filtering visitor wrapping a preorder generator, filter always rejecting and appending the concatenated parent artifactIds plus `,` | buffer is exactly `",a,ba,cba,a,ea,"` |
| `path-recorder/parents.txt` | path-recording visitor with the same recording filter | buffer is exactly `",a,ba,cba,a,ea,"` |
| `path-recorder/simple.txt` | path recorder matching `groupId == "match"` | 2 paths: `[a, b, x]`, `[a, x]` |
| `path-recorder/nested.txt` | path recorder, excluding matches beneath matches (default) | 1 path: `[x]` |
| `path-recorder/nested.txt` | path recorder, including matches beneath matches | 3 paths: `[x]`, `[x, a, y]`, `[x, y]` |
| `path-recorder/cycle.txt` | path recorder, including matches beneath matches | 4 paths: `[a, b, x]`, `[a, x]`, `[a, x, b, x]`, `[a, x, x]` |
| `path-recorder/cycle-3paths.txt` | path recorder, default | 1 path: `[a, b]` |
| `tree/cycles.txt` | tree visitor wrapping an enter/leave recorder (`>id ` / `<id `) | buffer is exactly `">a >b >c <c <b >d <d <a "` |

Paths and sequences are compared by **artifactId only**, element-wise, and the length must match
exactly.
