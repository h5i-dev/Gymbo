# Effective POM construction — compatibility specification

> **Provenance.** This document was derived by reading the Apache Maven sources, which are licensed
> under the **Apache License, Version 2.0**. No source code is reproduced verbatim beyond short
> identifiers, expressions and literal constants required to specify behaviour.
>
> * Clone: `_reference/maven`
> * Commit: `945813a7d4d91f32fe92d2c5a81d0a8223bc10b9`
> * Root `pom.xml` version: **4.1.0-SNAPSHOT** (parent `org.apache:apache:49`)
>
> Primary sources (all paths relative to the clone root):
>
> | Area | Path |
> |---|---|
> | Pipeline | `impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelBuilder.java` |
> | Per-field merge | `impl/maven-impl/src/main/java/org/apache/maven/impl/model/MavenModelMerger.java` |
> | Generated merge base | `src/mdo/merger.vm` → generates `org.apache.maven.model.v4.MavenMerger` |
> | Model schema | `api/maven-api-model/src/main/mdo/maven.mdo` |
> | Generated interpolation walker | `src/mdo/transformer.vm` → generates `org.apache.maven.model.v4.MavenTransformer` |
> | Inheritance / URL rules | `impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultInheritanceAssembler.java` |
> | Interpolation | `impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelInterpolator.java`, `DefaultInterpolator.java` |
> | Profiles | `DefaultProfileSelector.java`, `DefaultProfileInjector.java`, `DefaultProfileActivationContext.java`, `profile/*.java` |
> | Management | `DefaultDependencyManagementImporter.java`, `DefaultDependencyManagementInjector.java`, `DefaultPluginManagementInjector.java` |
> | Normalization / paths / URLs | `DefaultModelNormalizer.java`, `DefaultModelPathTranslator.java`, `DefaultPathTranslator.java`, `impl/maven-impl/src/main/java/org/apache/maven/impl/DefaultModelUrlNormalizer.java`, `DefaultUrlNormalizer.java` |
> | Lifecycle bindings | `DefaultLifecycleBindingsInjector.java` |
> | XML config merge | `api/maven-api-xml/src/main/java/org/apache/maven/api/xml/XmlService.java`, `impl/maven-xml/src/main/java/org/apache/maven/internal/xml/DefaultXmlService.java` |
> | Maven 3 lineage (for divergences) | `compat/maven-model-builder/src/main/java/org/apache/maven/model/**`, `compat/maven-model/src/main/java/org/apache/maven/model/merge/ModelMerger.java` |

## How to read this document

The Rust implementation targets **Maven 3.9 behaviour**. Every place where the Maven 4 code in this
clone differs from the Maven 3 lineage is flagged **`[M3≠M4]`** inline, and all of them are collected
in [§10](#10-maven-39-vs-maven-4-divergences).

Caveat about the "Maven 3" evidence: the Maven 3 model builder still lives in this repository under
`compat/maven-model-builder` (deprecated, `@Deprecated(since = "4.0.0")`). It is Maven 4's *copy* of
the 3.x builder, so it is the best available proxy for 3.9 but it has absorbed a few Maven-4-era
additions (noted where relevant). The clone is shallow (single commit), so history could not be used
to date individual lines.

Two vocabulary notes used throughout:

* **`sourceDominant`** — the merge direction flag of every merge method. Inheritance calls
  `merge(child, parent, /*sourceDominant=*/false, hints)`: the **target is the child**, the **source
  is the parent**, and the child wins. Profile injection calls the same merger with
  `sourceDominant = true`: target is the model, source is the profile, and the **profile wins**.
* **file model / raw model / effective model** — three snapshots that Maven 4 keeps in its result
  object. See [§1.1](#11-model-snapshots).

---

## 1. Pipeline order

### 1.1 Model snapshots

| Snapshot | Produced by | Contents |
|---|---|---|
| **file model** | `ModelBuilderSessionState.readFileModel()` → `doReadFileModel()` | POM as parsed, plus: model-version inferred from the XML namespace, `pomFile` set, parent GAV completed from `relativePath` if incomplete, subproject auto-discovery (model ≥ 4.1.0 only), CI-friendly `${revision}`-style substitution in `version`/`parent.version`, `id`+`url` interpolation of repositories/pluginRepositories/profile repositories/distributionManagement, model+profile properties overridden by user properties. Validated by `validateFileModel`. |
| **raw model** | `readRawModel()` → `doReadRawModel()` | file model, plus (model > 4.0.0 and build request only) `transformFileToRaw()`, which infers missing `version`/`groupId` of dependencies and managed dependencies from reactor members. Validated by `validateRawModel`. **For a 4.0.0 POM raw == file.** |
| **effective model** | `buildEffectiveModel()` | fully assembled model. |

### 1.2 Exact ordered pipeline (Maven 4, authoritative reading of `DefaultModelBuilder`)

`ModelBuilderSessionImpl.build(request)` dispatches on request type:
`BUILD_PROJECT` → `buildBuildPom()`; everything else → `buildEffectiveModel(new LinkedHashSet<>())`.

**Phase A — reactor discovery (`BUILD_PROJECT` only), `buildBuildPom()`**

| # | Step | Class / method |
|---|---|---|
| A1 | Locate root directory (`session.getRootDirectory()` or `RootLocator.findMandatoryRoot(top)`) and the root POM (`ModelProcessor.locateExistingPom`). | `DefaultModelBuilder.buildBuildPom` |
| A2 | Recursively read the **file model** of the root POM and of every subproject; register each `groupId:artifactId → source`. Aggregation cycles and missing subprojects are reported here. | `loadFromRoot` → `loadFilePom` |
| A3 | For subproject discovery only, each file model is run through **`activateFileModel()`** (see B1–B6) and `getSubprojects()` is read from the result (`subprojects`, falling back to the deprecated `modules`). | `loadFilePom` |
| A4 | For the top model and every child result, run Phase B/C in parallel. | `buildBuildPom` |

**Phase B — `buildEffectiveModel()` → `readEffectiveModel()`**

| # | Step | Class / method |
|---|---|---|
| B0 | `inputModel = readRawModel()` (which forces `readFileModel()` first). | `readRawModel` |
| B1 | Build a `DefaultProfileActivationContext` over the **raw/file model** (active/inactive profile ids, system properties, user properties, model). | `getProfileActivationContext` |
| B2 | **Activate external (settings) profiles** against that context. | `activateFileModel` → `DefaultProfileSelector.getActiveProfiles` |
| B3 | Merge the properties of the active external profiles into the context's *user* properties (user properties still win). | `activateFileModel` |
| B4 | **Activate this POM's own profiles** against the same context. | `activateFileModel` |
| B5 | **Normalize (merge duplicates)**. | `DefaultModelNormalizer.mergeDuplicates` |
| B6 | `getProfileActivations` / `injectProfileActivations` — captures each profile's `<activation>` and re-injects it. **This is a no-op in the current code** (the activations are neither interpolated nor otherwise transformed); it survives from the Maven 3 save/restore-around-interpolation idiom. Do not implement anything here. | `DefaultModelBuilder.injectProfileActivations` |
| B7 | **Inject the active POM profiles, then the active external profiles**, into the file model. Result = *activated file model*, used only for parent resolution and subproject discovery. | `DefaultProfileInjector.injectProfiles` |
| B8 | Re-create the activation context over the *activated file model*; re-apply B3. | `readEffectiveModel` |
| B9 | **Resolve the parent** — recursion, see [§1.3](#13-parent-recursion). Returns a fully inherited, profile-injected, **non-interpolated** parent model. If there is no `<parent>`, the parent is the **super POM** for the model version (`SuperPomProvider.getSuperPom`). | `readParent` → `resolveParent` → `readParentLocally` / `resolveAndReadParentExternally` → `readAsParentModel` |
| B10 | Fill in `parent.relativePath` when it was absent (relativized real path for build requests, else `".."`). | `readEffectiveModel` |
| B11 | **Inheritance assembly**: `assembleModelInheritance(inputModel, parentModel)` — note the child side is the **raw** model, *not* the profile-injected one. | `DefaultInheritanceAssembler` |
| B12 | For each `<mixin>` (model ≥ 4.1.0 only): resolve it like a parent, assemble inheritance again, then force the mixin's properties to win. | `readEffectiveModel` |
| B13 | **Normalize (merge duplicates)** again. | `DefaultModelNormalizer.mergeDuplicates` |
| B14 | Re-point the activation context at the **post-inheritance, pre-interpolation** model and **re-activate** the POM's profiles. `[M3≠M4]` — Maven 3 never re-activates after inheritance. | `readEffectiveModel` |
| B15 | **Inject the active POM profiles, then the active external profiles** into the inherited model. | `DefaultProfileInjector.injectProfiles` |
| B16 | **Interpolation** of the whole model. | `DefaultModelBuilder.interpolateModel` → `DefaultModelInterpolator` |
| B17 | Interpolate `parent.version` separately against user properties → model properties → system properties. | `interpolateModel` |
| B18 | **Normalize (merge duplicates)** a third time. | `DefaultModelNormalizer.mergeDuplicates` |
| B19 | **URL normalization** — collapse `/../` segments in `url`, `scm.{url,connection,developerConnection}`, `distributionManagement.site.url`. | `DefaultModelUrlNormalizer` / `DefaultUrlNormalizer` |
| B20 | Re-configure the repository list from the now-interpolated `<repositories>` (replace mode). | `mergeRepositories(model, true)` |

**Phase C — `buildEffectiveModel()` continues after `readEffectiveModel()` returns**

| # | Step | Class / method |
|---|---|---|
| C1 | **Path translation** — make `build.*` directories, `build.sources`, resources, testResources, filters and `reporting.outputDirectory` absolute against the project directory. | `DefaultModelPathTranslator.alignToBaseDirectory` |
| C2 | **pluginManagement injection**. | `DefaultPluginManagementInjector.injectManagement` |
| C3 | **Lifecycle-bindings injection** (skipped for `CONSUMER_DEPENDENCY` requests, and only if the request carries a `lifecycleBindingsInjector`). | `DefaultLifecycleBindingsInjector` |
| C4 | **dependencyManagement import (BOM)** — strip `type=pom,scope=import` entries, build each BOM's *effective* model recursively, then merge. | `importDependencyManagement` → `DefaultDependencyManagementImporter.importManagement` |
| C5 | **dependencyManagement injection** into `<dependencies>`. | `DefaultDependencyManagementInjector.injectManagement` |
| C6 | **Inject default values** — `dependency.scope = "compile"` when unset (top-level and plugin dependencies). | `DefaultModelNormalizer.injectDefaultValues` |
| C7 | **Plugin configuration expansion** — push each plugin's `<configuration>` down into each of its `<execution>`s (skipped for `CONSUMER_DEPENDENCY`). | `DefaultPluginConfigurationExpander` |
| C8 | SPI `ModelTransformer.transformEffectiveModel` hooks. | `buildEffectiveModel` |
| C9 | **Effective-model validation** (`STRICT` for build requests, `MINIMAL` otherwise). | `DefaultModelValidator.validateEffectiveModel` |

### 1.3 Parent recursion

`readParent(childModel, parent, ctx, parentChain)`:

1. If `parent == null`: the parent is the **super POM** of `childModel.modelVersion` (falling back to
   `4.0.0` when the version is absent or unknown), and recursion stops.
2. Otherwise push `groupId:artifactId:version` onto `parentChain`; a duplicate is a **FATAL**
   "The parents form a cycle" problem. `readParentLocally` additionally pushes the candidate's
   *file location* onto the chain before descending (guards against `StackOverflowError`).
3. `resolveParent`: for build requests try `readParentLocally` first — `relativePath` if set,
   otherwise the reactor by GAV, otherwise `".."`; a GA mismatch is reported and falls back to the
   repository. Non-build requests / failures go to `resolveAndReadParentExternally`, which merges the
   child's `<repositories>` into the resolver's repository list before resolving.
4. `parentModel.packaging` must be `pom` (ERROR otherwise).
5. **`readAsParentModel()` is what actually builds a parent**, and it is *not* `buildEffectiveModel`:
   * `raw = readRawModel()` for the parent source;
   * `parentData = readParent(raw, raw.getParent(), childActivationContext, parentChain)` — **recursion**;
   * inheritance assembly of `raw` over `parentData`, using an `InheritanceModelMerger` subclass that
     additionally no-ops `mergeModel_Modules`/`mergeModel_Subprojects` (belt-and-braces: those
     generated overloads are dead code — `mergeModelBase_Modules`/`_Subprojects` are the ones that
     actually run, and they already refuse to inherit);
   * mixins, if any;
   * **profile activation and injection for the parent's profiles, evaluated with the *child's*
     activation context**, then `withProfiles(List.of())` to strip them, then `withParent(null)`.
   * Results are cached per activation-context *record* (the set of properties/files actually
     consulted), so a parent is re-assembled only when a relevant input changes.
6. Consequently a parent model handed to inheritance is: raw + inherited + profile-injected, and
   **never interpolated, never path-translated, never dependency-managed**.

### 1.4 Answers to the two ordering questions

* **Interpolation happens strictly after inheritance** (B11 → B16), once, on the fully assembled
  child model. Parents are not interpolated; the only pre-inheritance textual substitution is the
  CI-friendly-version and repository-id/url pass inside `doReadFileModel`.
* **Profiles are activated against the raw (non-interpolated) model.** Maven 4 activates twice: once
  against the file/raw model (B4, drives subproject discovery and the reported active profiles) and
  once against the post-inheritance pre-interpolation model (B14, drives what is injected into the
  effective model). Activation *values* (`os`, `property`, `jdk`) are never interpolated in Maven 4;
  only `file/@exists|@missing` paths are. `[M3≠M4]` — see §6.7 and §10.

### 1.5 Maven 3 pipeline, for comparison

`compat/.../model/building/DefaultModelBuilder.build(request, importIds)`:

```
phase 1:
  read + validate raw model of the POM being built
  activate external profiles; fold their properties into user properties
  loop over the lineage, starting at the POM itself:
      normalize (mergeDuplicates)
      set context project properties := THIS lineage model's properties
      interpolate the <activation> elements of this model's profiles (limited value sources)
      activate this model's profiles
      inject the active profiles into THIS model      <-- before inheritance, for every model
      (top model only) inject the active external profiles
      read the parent -> next lineage element; stop at the super POM
  inheritance assembly over the whole lineage, from the super POM downwards
  interpolate the resulting model (once)
  interpolate parent.version
  url normalization
phase 2 (same order as Maven 4 phase C):
  path translation
  pluginManagement injection
  lifecycle bindings injection (if request.isProcessPlugins())
  dependencyManagement import (BOM)
  dependencyManagement injection
  injectDefaultValues
  reportConfigurationExpander        <-- Maven 3 only
  reportingConverter                 <-- Maven 3 only
  pluginConfigurationExpander
  effective model validation
```

The phase-2 order is identical between the two, and interpolation is after inheritance in both. The
structural differences are: **when the child's profiles are injected** (Maven 3: before inheritance;
Maven 4: after), **how many times profiles are activated**, and the two extra Maven 3 reporting
steps. See §10.

---

## 2. Merge precedence table

### 2.1 The merge machinery

`MavenModelMerger extends MavenMerger` (generated from `src/mdo/merger.vm`) and is constructed with
`super(false)` — **`deepMerge = false`**. Five subclasses exist, all sharing the same field rules
except where noted:

| Subclass | Used for | `sourceDominant` |
|---|---|---|
| `DefaultInheritanceAssembler.InheritanceModelMerger` | parent → child | `false` |
| `DefaultProfileInjector.ProfileModelMerger` | profile → model | `true` |
| `DefaultPluginManagementInjector.ManagementModelMerger` | pluginManagement → build plugins | `false` |
| `DefaultDependencyManagementInjector.ManagementModelMerger` | dependencyManagement → dependencies | `false` |
| `DefaultModelNormalizer.DuplicateMerger` | duplicate plugin → first occurrence | `false` |
| `DefaultLifecycleBindingsInjector.LifecycleBindingsMerger` | lifecycle plugins → build plugins | `false` |

Generic rules from the generated base, by field kind:

| Field kind | Rule | Inheritance (`sourceDominant=false`) | Profile injection (`sourceDominant=true`) |
|---|---|---|---|
| `String` | `if (src != null && (sourceDominant \|\| tgt == null)) tgt = src` | inherited only when the child's value is `null` (child wins) | profile wins when non-`null` |
| `boolean`, `int`, `Path` | `if (sourceDominant) tgt = src` | **never inherited** | always overwritten by the profile's value, including defaults |
| `List<String>` | union: target order first, then source entries not already present | child list then parent-only entries appended | model list then profile-only entries appended |
| `Properties` (`Map<String,String>`) | empty source → no change; empty target → source; else dominant overlays recessive | see §2.3 (overridden for inheritance) | profile entries win |
| single association | recursive merge into the target instance (an empty instance is created if the target is `null`) | recursive, child wins per leaf field | recursive, profile wins per leaf field |
| list association | **merge by key**: target elements in order, then source elements whose key is new; on key collision the *dominant* element **replaces the recessive one wholesale — no per-field merge** (because `deepMerge = false`) | child element wins entirely | profile element wins entirely |
| `DOM` (`<configuration>`) | `tgt == null → src`; else `XmlService.merge(dominant, recessive)` where dominant = source if `sourceDominant` else target | child config dominant, parent config recessive | profile config dominant |

**Key computers** (`MavenModelMerger` overrides):

| Element | Merge key |
|---|---|
| `Dependency` | `Dependency.getManagementKey()` = **`groupId:artifactId:type[:classifier]`** — the classifier segment is appended only when the classifier is non-`null` and non-empty. `type` defaults to `jar` via the model defaults, so it is effectively always present. **The version is *not* part of the key.** |
| `Plugin` | `groupId:artifactId` |
| `PluginExecution` | `id` (model default `default`) |
| `ReportPlugin` | `groupId:artifactId` |
| `ReportSet` | `id` (model default `default`) |
| `Extension` | `groupId + ':' + artifactId` |
| `Exclusion` | `groupId + ':' + artifactId` |
| `RepositoryBase` | `id` |
| `Repository` / `DeploymentRepository` in `repositories`/`pluginRepositories` | identity key `v -> v`, but `RepositoryBase.id` is the **only** field in the whole model marked `<identifier>true</identifier>`, so generated `equals`/`hashCode` compare **`id` alone** → effectively keyed by `id`. `ProfileModelMerger` additionally overrides `getRepositoryKey()` to `RepositoryBase::getId` explicitly. |
| everything else (`Resource`, `License`, `Developer`, `Contributor`, `MailingList`, `Notifier`, `Source`, `Profile`, …) | identity key `v -> v` **with no generated `equals`** → object identity → distinct instances never collide, i.e. plain concatenation |

### 2.2 `Model` (root)

| Field | Inheritance (parent → child) | Profile injection | Notes |
|---|---|---|---|
| `modelVersion` | **never inherited** (`mergeModel_ModelVersion` = no-op) | n/a (not a `Profile` field) | |
| `parent` | recursive single-association merge — child's own `<parent>` fields win; the parent model handed in has `parent == null`, so in practice unchanged | n/a | `readAsParentModel` strips the parent's parent |
| `mixins` | **never inherited** (`InheritanceModelMerger.mergeModel_Mixins` = no-op) | n/a | model ≥ 4.1.0 only |
| `groupId` | inherited if child's is `null` | n/a | |
| `artifactId` | **never inherited** (no-op) | n/a | |
| `version` | inherited if child's is `null` | n/a | |
| `packaging` | plain `String` rule — but the reader applies the model default `jar`, so the child's value is never `null` and the parent's is **never** visible. Treat as *not inherited*. | n/a | |
| `name` | **never inherited** (`mergeModel_Name` only assigns when `sourceDominant`) | n/a | |
| `description` | inherited if child's is `null` | n/a | |
| `url` | inherited if child's is `null`, **with child-path appending** — §4 | n/a | |
| `childProjectUrlInheritAppendPath` | plain `String` rule → inherited when the child does not set the attribute | n/a | |
| `root`, `preserveModelVersion` | `boolean` → **never inherited** | n/a | model ≥ 4.1.0 |
| `inceptionYear` | inherited if child's is `null` | n/a | |
| `organization` | copied from the parent **only if the child has none**; never partially merged | n/a | |
| `licenses` | all-or-nothing: `child.isEmpty() ? parent : child` | n/a | |
| `developers` | all-or-nothing | n/a | |
| `contributors` | all-or-nothing | n/a | |
| `mailingLists` | all-or-nothing (parent's list used only if child's is empty) | n/a | |
| `prerequisites` | **never inherited** (no-op) | n/a | |
| `scm` | recursive; `connection`, `developerConnection`, `url` get child-path appending (§4); `tag` and the three `child*InheritAppendPath` attributes follow the plain `String` rule | n/a | |
| `issueManagement` | copied only if the child has none | n/a | |
| `ciManagement` | copied only if the child has none | n/a | |
| `build` | recursive (`Build`), see §2.4 | `Profile.build` (a `BuildBase`) is merged into `Model.build` by `DefaultProfileInjector` **separately** from `mergeModelBase` | |
| `profiles` | **never inherited** (no-op, and parents have their profiles stripped after injection) | n/a | |
| `pomFile` | not an XML field; restored explicitly after interpolation | n/a | |

### 2.3 `ModelBase` (shared by `Model` and `Profile`)

| Field | Inheritance | Profile injection |
|---|---|---|
| `modules` (deprecated ≤ 4.2.0) | **never inherited** — `mergeModelBase_Modules` acts only when `sourceDominant` | union, model order first, profile entries appended if not already present (with `InputLocation` index tracking) |
| `subprojects` (≥ 4.1.0) | **never inherited** (same guard) | union as above |
| `distributionManagement` | recursive, see §2.7 | recursive, profile wins |
| `properties` | overridden by `InheritanceModelMerger.mergeModelBase_Properties`: start from the **parent's** properties **excluding the key `project.directory`**, then overlay the child's → child wins, and `project.directory` is never inherited | generic `Properties` rule → profile properties overlay the model's |
| `dependencyManagement` | recursive → its `dependencies` list merged by management key: **child entries first, in child order, then parent-only entries appended**; on collision the child's entry wins wholesale | profile entries override same-key entries in place and new ones are appended |
| `dependencies` | merge by management key: child entries first, then parent-only entries; collision → **child's entry replaces the parent's entirely** (the parent's `version`/`scope`/`exclusions` are *not* folded in) | profile deps override same-key entries in place, new ones appended |
| `repositories` | overridden: build a `LinkedHashMap` keyed by repository **id**, dominant list first (child), then recessive entries whose id is absent → **child order first, then parent-only repositories** | dominant = profile: profile repositories first, then model-only ones. ⚠ Note the *order flips* relative to the model's own declaration order when a profile is injected. |
| `pluginRepositories` | identical to `repositories` | identical |
| `reporting` | recursive: `excludeDefaults`, `outputDirectory` = plain `String`; `plugins` overridden — §2.6 | recursive |
| `reports` (`DOM`, 4.0.0 only) | `DOM` rule | `DOM` rule |

### 2.4 `Build` / `BuildBase` / `PluginConfiguration` / `PluginContainer`

| Field | Inheritance | Profile injection |
|---|---|---|
| `defaultGoal`, `directory`, `finalName` | plain `String` | plain `String` |
| `sourceDirectory`, `scriptSourceDirectory`, `testSourceDirectory`, `outputDirectory`, `testOutputDirectory` (`Build` only) | plain `String` | not reachable — `Profile.build` is a `BuildBase` |
| `sources` (≥ 4.1.0) | list association with **identity key** → concatenation (child's first, then the parent's) | n/a (`Build` only) |
| `resources` | **all-or-nothing**: if the child declares any resource, the parent's are dropped; otherwise the parent's are inherited wholesale | `sourceDominant` → generic identity-keyed merge → **concatenation**: model resources first, then the profile's |
| `testResources` | same as `resources` | same as `resources` |
| `filters` | overridden: union of child's then parent's, duplicates removed | union, model's first |
| `extensions` (`Build` only) | list keyed by `groupId:artifactId`; child's first, then parent-only; collision → child wins wholesale | n/a |
| `plugins` (`PluginContainer`) | **overridden by `InheritanceModelMerger.mergePluginContainer_Plugins`** — see §2.5 | **overridden by `ProfileModelMerger.mergePluginContainer_Plugins`** — see §2.5 |
| `pluginManagement` (`PluginConfiguration`) | recursive `PluginManagement` → its `plugins` go through the same overridden `mergePluginContainer_Plugins` | same |

### 2.5 Plugins

**Inheritance (`InheritanceModelMerger.mergePluginContainer_Plugins`)**

1. Walk the **parent's** plugins. A parent plugin is a candidate only when
   `plugin.isInherited() || !plugin.getExecutions().isEmpty()` (`inherited` defaults to `true`; the
   `|| executions` clause lets a plugin with `<inherited>false</inherited>` still contribute
   executions that individually opt back in). Each candidate is merged into a fresh empty `Plugin`
   (so that execution-level inheritance logic runs), keyed by `groupId:artifactId`.
2. Walk the **child's** plugins. On a key collision, `mergePlugin(childPlugin, parentPlugin, false)`;
   otherwise the child plugin is buffered as a "predecessor" of the next colliding key.
3. Output order: for each parent-derived key in parent order, first the buffered child-only plugins
   that preceded it, then the merged plugin; finally any remaining child-only plugins.
   **Net effect: parent plugin order dominates, child-only plugins keep their relative position.**

**Profile injection (`ProfileModelMerger.mergePluginContainer_Plugins`)** is the mirror image: the
master map is seeded from the **model's** plugins (so model order dominates), then the profile's
plugins are merged over them with `mergePlugin(existing, profilePlugin, true)`; profile-only plugins
are buffered as predecessors of the next colliding key and appended at the end otherwise. Note that
`ProfileModelMerger` does **not** override `mergePlugin`, so the `isInherited()` gate of §2.5 does not
apply and `<configuration>`/`<inherited>` are always merged with the profile dominant.

**`InheritanceModelMerger.mergePlugin(target=child, source=parent, false)`** merges, in order:
`ConfigurationContainer` fields (`inherited`, `configuration`) **only if `source.isInherited()`**,
then `groupId`, `artifactId`, `version`, `extensions`, `executions`, `dependencies`.

| Plugin field | Inheritance | pluginManagement injection | Profile injection |
|---|---|---|---|
| `groupId`, `artifactId` | plain `String` | plain `String` (management fills gaps) | profile wins |
| `version` | inherited when the child's is `null` | **management fills it only when the plugin has none** (`sourceDominant=false`) | profile wins |
| `extensions` (String-typed boolean) | plain `String` | management fills when unset | profile wins |
| `inherited` | plain `String`, and only when the parent plugin is itself inherited | management fills when unset | profile wins |
| `configuration` | `XmlService.merge(childConfig, parentConfig)` — child dominant; skipped entirely when the parent plugin has `inherited=false` | `XmlService.merge(pluginConfig, managedConfig)` — the plugin's own config dominant | `XmlService.merge(profileConfig, modelConfig)` — profile dominant |
| `dependencies` | keyed by management key; child's first, then parent-only; collision → child wins wholesale | same shape, plugin's own win | profile's win |
| `executions` | see below | see below | see below |

**`executions` merge by `id`:**

* *Inheritance* (`MavenModelMerger.mergePlugin_Executions`): parent executions are admitted only when
  `execution.getInherited() != null ? execution.isInherited() : plugin.isInherited()`. Admitted
  parent executions are inserted first (parent order); then the child's executions are walked and,
  on an id collision, `mergePluginExecution(childExec, parentExec, false)` (child wins per field,
  `goals` unioned child-first, `configuration` child-dominant). Child-only executions are appended.
* *pluginManagement injection* (`DefaultPluginManagementInjector`): managed executions first, then the
  plugin's own; collision → `mergePluginExecution(ownExec, managedExec, false)` → the plugin's own
  values win, management only fills gaps.
* *Profile injection* (`ProfileModelMerger`): model executions first, then the profile's; collision →
  `mergePluginExecution(modelExec, profileExec, true)` → **profile wins**.
* *Lifecycle bindings* (`LifecycleBindingsMerger`): the POM's plugins are the target and dominate
  (`mergePlugin(pomPlugin, lifecyclePlugin, false)`); lifecycle plugins not present in the POM are
  appended, and for each such added plugin that *is* declared in `<pluginManagement>` the merge is
  redone as `mergePlugin(managedPlugin, addedPlugin, false)` — the **managed declaration becomes the
  base and wins**, with the lifecycle plugin only filling gaps. `PluginExecution.priority` uses a
  special rule here: the **lower** priority number wins.

`PluginExecution` fields: `id` and `phase` plain `String`; `priority` `int` → not inherited (see the
lifecycle exception above); `goals` union (target first, duplicates removed);
`configuration`/`inherited` per `ConfigurationContainer`.

#### Where the bindings themselves come from, and in what order

`DefaultLifecyclePluginAnalyzer.getPluginsBoundByDefaultToAllLifecycles(packaging)` builds the source
model's plugin list. Three things about it are not guessable from the code alone:

* The data is in `maven-core-3.9.9.jar`, not in Maven's source-controlled Java: the `default`
  lifecycle's per-packaging phase→goal mapping is `META-INF/plexus/default-bindings.xml`
  (packagings `pom`, `jar`, `ejb`, `maven-plugin`, `war`, `ear`, `rar`), and the `clean` and `site`
  lifecycles' packaging-independent `<default-phases>` are in `META-INF/plexus/components.xml`.
  Each entry is `groupId:artifactId:version:goal`, comma-separated where a phase binds several.
* Lifecycles are visited in **id order** (`getOrderedLifecycles` sorts them), so every effective POM
  lists the `clean` binding first, then the `default` ones, then `site`.
* Within one lifecycle the phases are visited in **`java.util.HashMap` bucket order**, because plexus
  deserializes `<phases>` into a plain `HashMap` and the analyzer walks its `entrySet`. No mapping
  exceeds nine phases, so the table stays at its initial 16 buckets and the order is
  `(h ^ h >>> 16) & 15` over `String.hashCode`, ties broken by document order. This is why a `jar`
  project's effective POM lists maven-jar-plugin *before* maven-compiler-plugin, and why
  maven-resources-plugin's `default-testResources` execution precedes `default-resources`.

A plugin is emitted once, at the first phase that binds it, accumulating one execution per goal with
id `default-<goal>` (colliding ids get a `-1`, `-2` suffix; no 3.9.9 mapping collides) and `phase` set
to the binding phase.

### 2.6 Reporting

* `Reporting.plugins` — `InheritanceModelMerger.mergeReporting_Plugins`: parent report plugins are
  admitted only when `element.isInherited()` (no `executions` escape hatch here), merged into a fresh
  instance, keyed by `groupId:artifactId`; then the child's plugins are merged over them.
  Output order: parent-derived keys first (insertion order of the `LinkedHashMap`), child-only
  appended. `ProfileModelMerger` mirrors it with the profile dominant.
* `ReportPlugin.reportSets` — merge by `id`; the same `inherited` gate as plugin executions
  (`rset.getInherited() != null ? rset.isInherited() : reportPlugin.isInherited()`).
* `ReportSet.reports` — `List<String>` union.

### 2.7 `DistributionManagement`

| Field | Inheritance | Profile injection |
|---|---|---|
| `repository` | if the parent has one and the child has none → recursive merge into a fresh `DeploymentRepository` (i.e. inherited); if the child has one → untouched | profile wins (fresh instance built from the profile's) |
| `snapshotRepository` | as `repository` | as `repository` |
| `site` | if the child's site is `null` **or "empty"** (`id`, `name`, `url` all null/empty) → merge the parent's in (`url` gets child-path appending, §4); otherwise only `childSiteUrlInheritAppendPath` is carried over from the parent | profile wins |
| `downloadUrl` | plain `String` → inherited when unset | profile wins |
| `status` | plain `String` → inherited when unset | profile wins |
| `relocation` | **never inherited** — `mergeDistributionManagement_Relocation` is an explicit no-op. Maven 3's generated `mergeDistributionManagement` simply never listed `relocation`, so it is not inherited there either: **same observable behaviour, no divergence.** | never injected |

### 2.8 Repositories, policies, dependencies (leaf classes)

* `RepositoryBase`: `id`, `name`, `url`, `layout` — plain `String`. `Repository` adds `releases` /
  `snapshots` (recursive `RepositoryPolicy`: `enabled`, `updatePolicy`, `checksumPolicy` as `String`).
  `DeploymentRepository.uniqueVersion` is `boolean` → not inherited.
  Because list merging is keyed by `id` and `deepMerge=false`, **a same-id repository is never
  partially merged**: the dominant declaration replaces the recessive one entirely.
* `Dependency`: `groupId`, `artifactId`, `version`, `type`, `classifier`, `scope`, `systemPath`,
  `optional` are all `String`; `exclusions` is a list keyed by `groupId:artifactId`. These per-field
  rules only ever come into play for **dependencyManagement injection** (§8) and for
  `pluginManagement`-driven plugin-dependency merges — never for plain inheritance, where whole
  `Dependency` objects are swapped by key.

### 2.9 `<configuration>` XML merge (`XmlService.merge(dominant, recessive)`)

Reference implementation: `DefaultXmlService.doMerge`.

* `combine.self="override"` on the dominant node → the recessive node is discarded outright.
* Otherwise: the **dominant's text value always wins** (the recessive value is never adopted, even
  when the dominant's is empty); recessive **attributes** are copied only where the dominant's value
  for that attribute is missing/empty.
* Children: by default matched **by element name** and merged recursively; `combine.children="append"`
  on the dominant appends the recessive children instead; `combine.id="X"` or
  `combine.keys="a,b"` (read from the recessive parent node) match children by attribute value and
  force merge mode for that child; `combine.self="remove"` on a *dominant child* removes it.
* When several dominant children share a name, they are consumed in document order, one per matching
  recessive child.

Maven 3.9 performs this with `plexus-utils`' `Xpp3DomUtils.mergeIntoXpp3Dom`. The algorithm is the
same in spirit, but `combine.self="remove"` is a Maven 4 addition and empty-value edge cases may
differ. `[M3≠M4]`

---

## 3. Non-inherited fields

A child **never** takes these from its parent:

| Element | Mechanism |
|---|---|
| `modelVersion` | `mergeModel_ModelVersion` no-op |
| `artifactId` | `mergeModel_ArtifactId` no-op |
| `name` | `mergeModel_Name` assigns only when `sourceDominant` |
| `prerequisites` | `mergeModel_Prerequisites` no-op |
| `profiles` | `mergeModel_Profiles` no-op **and** the parent's profiles are stripped (`withProfiles(List.of())`) after being injected into the parent |
| `mixins` (≥ 4.1.0) | `InheritanceModelMerger.mergeModel_Mixins` no-op |
| `modules` / `subprojects` | `mergeModelBase_Modules` / `_Subprojects` act only when `sourceDominant` |
| `distributionManagement.relocation` | explicit no-op (M4) / absent from the merge (M3) |
| `packaging` | *de facto*: the model default `jar` means the child's value is never `null` |
| the `project.directory` property | explicitly excluded when merging the parent's `<properties>` |
| every `boolean`/`int`/`Path` field | generic rule `if (sourceDominant)` — includes `root`, `preserveModelVersion`, `DeploymentRepository.uniqueVersion`, `Notifier.sendOn*`, `Source.stringFiltering`, `Source.enabled`, `PluginExecution.priority`, `Activation.activeByDefault` |
| `pomFile` | not an XML/model field |

Partially inherited, i.e. all-or-nothing rather than per-field: `organization`, `issueManagement`,
`ciManagement` (only when the child has none at all), `licenses`, `developers`, `contributors`,
`mailingLists` (only when the child's list is empty), `build.resources`, `build.testResources` (only
when the child's list is empty), `distributionManagement.site` (only when absent or empty).

Maven 3's own documentation (`compat/maven-model-builder/src/site/markdown/index.md`) lists the
intentionally-not-inherited set as `modelVersion`, `artifactId`, `packaging`, `profiles`,
`prerequisites` — consistent with the above (`name` is missing from that doc but is genuinely not
inherited in both implementations).

---

## 4. URL adjustment (child path appending)

### 4.1 Which fields

Exactly five, all handled by `extrapolateChildUrl(parentValue, appendPathFlag, context)`:

| Field | Append-path flag (XML attribute) | Default |
|---|---|---|
| `project.url` | `project/@child.project.url.inherit.append.path` | `true` |
| `project.scm.connection` | `project/scm/@child.scm.connection.inherit.append.path` | `true` |
| `project.scm.developerConnection` | `project/scm/@child.scm.developerConnection.inherit.append.path` | `true` |
| `project.scm.url` | `project/scm/@child.scm.url.inherit.append.path` | `true` |
| `project.distributionManagement.site.url` | `project/distributionManagement/site/@child.site.url.inherit.append.path` | `true` |

The flag is read **from the source (parent) element** at merge time; the attributes are `String`-typed
in the model and parsed as `Boolean.parseBoolean`, so anything other than the literal `"false"`
(case-insensitively per `parseBoolean`, i.e. `"false"`/`"FALSE"`) means *append*. The attributes
themselves are inherited under the plain `String` rule, so a parent setting `false` also disables
appending for grandchildren unless the child re-declares it.

`distributionManagement.relocation` is **not** part of this mechanism and is not inherited at all.
Neither is `distributionManagement.downloadUrl` adjusted (it is inherited verbatim).

### 4.2 When appending happens

Only during **inheritance** (`sourceDominant == false`) **and** only when the child's own value is
`null`. If `sourceDominant` (profile injection) the value is copied verbatim. If the parent value is
`null` or blank, or the flag is `false`, or the hints are missing, the parent value is used verbatim.

### 4.3 How the appended path is derived

`DefaultInheritanceAssembler.assembleModelInheritance` computes two hints before merging:

1. **`child-directory`** (`childPath`) =
   `child.getProperties().getOrDefault("project.directory", child.getArtifactId())`.
   So the child's `<properties><project.directory>…</project.directory></properties>` overrides the
   artifactId. That property is deliberately **not inherited** (§2.3), so it only ever affects the
   POM that declares it. (Documented in Maven as "since Maven 3.5.0".)
2. **`child-path-adjustment`** = `getChildPathAdjustment(child, parent, childPath)`:
   * start with `""`;
   * `childName` = `child.artifactId`, **overridden by the child's project directory name** when the
     child model has a project directory (`child.getProjectDirectory().getFileName()`) — this is the
     MNG-5000 back-compat rule, and it means a repository-loaded model and a filesystem-loaded model
     can produce different URLs;
   * for each `module` in **`parent.getModules()`** (in order): normalise `\` → `/`; if the entry ends
     with `.xml` (case-insensitive) truncate to the last `/` inclusive; strip a trailing `/`; take the
     segment after the last `/` as `moduleName`; if (`moduleName == childName` **or**
     `moduleName == childPath`) **and** the entry contained a `/`, then the adjustment is the module
     path up to (excluding) that last `/`, and the loop stops.
   * The filesystem is never consulted.

   ⚠ `[M3≠M4]` This loop reads **`modules` only**. A Maven 4 parent that uses `<subprojects>`
   (model ≥ 4.1.0) yields an empty adjustment, so nested-directory URL adjustment silently stops
   working. Maven 3.9 has no `subprojects`, so for a 3.9-compatible implementation: read `modules`.

3. `appendPath(parentUrl, childPath, pathAdjustment)` = `parentUrl` then `concatPath(pathAdjustment)`
   then `concatPath(childPath)`, where `concatPath(url, path)` for a non-empty `path`:
   * remember whether `url` currently ends with `/`;
   * if `path` starts with `/` and `url` ends with `/`, drop one `/` from `url`;
   * else if neither, insert a `/`;
   * append `path`;
   * if `url` originally ended with `/` and `path` does not end with `/`, append a trailing `/`.

   Empty path components are skipped entirely.

4. After interpolation, `DefaultModelUrlNormalizer` collapses `/../` in the five URL fields
   (`DefaultUrlNormalizer`: repeatedly find `/../`; if at index 0 strip the leading `/..`; else remove
   the preceding path segment). This is how `../` in a `child-path-adjustment` disappears.

### 4.4 Worked example

Parent `com.acme:parent:1.0` with `<url>http://acme.com/proj/</url>` and
`<modules><module>sub/child</module></modules>`; child artifactId `child`, no `url`.
`childPath = "child"`, adjustment `= "sub"` → `http://acme.com/proj/sub/child/` (trailing slash
preserved because the parent URL had one).

If the child instead declares `<properties><project.directory>modules/child</project.directory></properties>`,
`childPath = "modules/child"`; the module entry `sub/child` still matches on `moduleName == childName`
(`child`), so adjustment stays `sub` → `http://acme.com/proj/sub/modules/child/`.

---

## 5. Interpolation

Entry point: `DefaultModelInterpolator.interpolateModel(model, projectDir, request, problems)`.

### 5.1 What gets interpolated

The generated `MavenTransformer` (`src/mdo/transformer.vm`) walks the **entire** model tree and applies
the substitution to:

* every `String` field of every model class (including profiles and their `<activation>` values — the
  activations have already been evaluated by then, so this only affects the reported effective model);
* every entry **value** of every `Properties`/`Map<String,String>` field (keys are untouched);
* every element of every `List<String>`;
* every `<configuration>`/`DOM` node's **value and attribute values**, recursively.

Not interpolated: `boolean`, `int` and `Path` fields; `Model.pomFile` (explicitly restored afterwards).
A fast path skips any string without a `$`.

### 5.2 Syntax

* Placeholder `${expression}`; `\${` / `\}` escapes (the backslash is removed by `unescape`).
* Nested placeholders are resolved **innermost-first** (`doSubstVars` scans for the first `}` whose
  matching `${` is the closest preceding one, substitutes, then re-scans the whole string).
* Shell-like operators, processed left to right and chainable:
  `${var:-fallback}` (use `fallback` when `var` is unset **or empty**) and
  `${var:+alternative}` (use `alternative` when `var` is set and non-empty). The operator's operand is
  itself interpolated. Any other `:x` pair is not treated as an operator (only `:-` and `:+` are
  recognised; the `InterpolatorException("Bad substitution operator")` branch is unreachable in
  practice).
* Unterminated or absent delimiters → the string is returned unchanged.

### 5.3 Value sources, in priority order (`DefaultModelInterpolator.doCallback`)

1. the literal expression `basedir` → absolute project directory;
2. `build.timestamp` or `maven.build.timestamp` → `MavenBuildTimestamp(session.getStartTime(),
   model.getProperties())`, formatted with `maven.build.timestamp.format` from the model properties
   (default `yyyy-MM-dd'T'HH:mm:ss'Z'`, UTC);
3. **user properties** (`request.getUserProperties()`, i.e. `-D`);
4. **model properties** (`model.getProperties()`);
5. **prefixed model reflection** — for each prefix in `getProjectPrefixes(request)`: strip the prefix
   and evaluate against the model, with these specials available *only* in prefixed form:
   `basedir`, `basedir.*`, `baseUri`, `baseUri.*`, `rootDirectory`, `rootDirectory.*`;
   otherwise `ReflectionValueExtractor.evaluate(subExpr, model, false)`.
   Prefixes: **`project.` only** for `RequestType.BUILD_PROJECT`; `pom.` **and** `project.` for every
   other request type. `[M3≠M4]`
6. **system properties** (`request.getSystemProperties()`);
7. **environment variables**: `request.getSystemProperties().get("env." + expression)` — i.e.
   `${env.PATH}` resolves at step 6 (the key literally contains the prefix) and `${PATH}` resolves
   here as a fallback. Case handling is whatever the session put into the system properties (Maven
   upper-cases env keys on Windows);
8. **un-prefixed model reflection** (`${version}`, `${build.finalName}`, …), including bare `basedir`
   and `basedir.*` but **not** `baseUri`/`rootDirectory`.

Post-processing (`postProcess`, applied to every resolved value):

* if the prefix-stripped expression is one of `build.directory`, `build.outputDirectory`,
  `build.testOutputDirectory`, `build.sourceDirectory`, `build.testSourceDirectory`,
  `build.scriptSourceDirectory`, `reporting.outputDirectory` → `PathTranslator.alignToBaseDirectory`;
* if the **raw** expression is one of `project.url`, `project.scm.url`, `project.scm.connection`,
  `project.scm.developerConnection`, `project.distributionManagement.site.url` →
  `UrlNormalizer.normalize` (collapse `/../`).

Results are memoised per expression for the duration of one model.

### 5.4 Cycles and unresolvable expressions

* **Cycles**: `resolveVariable` maintains a set of in-flight variable names; re-entering one throws
  `InterpolatorException("recursive variable reference: <name>")`. `DefaultModelInterpolator` catches
  it, records a **`Severity.ERROR`** model problem, and **returns `null` for that whole string** — the
  field becomes `null`, not "left as written". The cycle set is shared and cleared per string within
  one model.
* **Unresolvable**: `defaultsToEmpty` is `false` for model interpolation, so an unresolved `${x}` is
  first rewritten to the internal marker `$__{x}` and then restored to the literal `${x}` by
  `unescape`. Net effect: **unresolvable expressions survive verbatim** and no problem is reported.
* A resolved value that itself contains placeholders is interpolated recursively.

### 5.5 Other, narrower interpolation passes

| Pass | Value sources | Applies to |
|---|---|---|
| CI-friendly version (`doReadFileModel` → `replaceCiFriendlyVersion`) | `getEnhancedProperties`: `basedir`, `project.basedir`, `project.basedir.uri`, `project.rootDirectory`, `project.rootDirectory.uri`, then the root model's properties **including those of its active profiles**, then system properties, then user properties (later wins) | `model.version`, `model.parent.version` |
| repository interpolation (`doReadFileModel`) | the same enhanced properties | `repository.id`, `repository.url` for `repositories`, `pluginRepositories`, every profile's repositories, and `distributionManagement.{repository,snapshotRepository}` |
| `parent.version` (post-interpolation) | user properties → model properties → system properties | `parent.version` |
| repository re-resolution (`mergeRepositories`) | full model interpolation over a synthetic model carrying only `pomFile`, `properties`, `repositories` | repository entries used by the resolver |
| profile file-activation paths | see §6.5 | `activation/file/@exists`, `@missing` |

---

## 6. Profile activation

### 6.1 Selection algorithm (`DefaultProfileSelector.getActiveProfiles`)

```
activeProfiles      := []
byDefaultPomProfiles := []
sawExplicitPomProfile := false
for each profile, in declaration order:
    if context.isProfileInactive(id):            # -P !id  / -P -id
        skip
    if context.isProfileActive(id)               # -P id
       or isActive(profile, context):            # activator evaluation
        activeProfiles += profile
        if profile.source == "pom": sawExplicitPomProfile = true
    else if profile.activation?.activeByDefault:
        if profile.source == "pom": byDefaultPomProfiles += profile
        else:                      activeProfiles += profile
if not sawExplicitPomProfile:
    activeProfiles += byDefaultPomProfiles
```

Consequences:

* **Deactivation wins over everything**, including explicit `-P id`.
* `<activeByDefault>true</activeByDefault>` on a **POM** profile is suppressed as soon as *any other*
  POM profile in the same list becomes active by id or by activator — including a profile activated
  purely by `-P`. It is **not** suppressed by an active *settings* profile, and settings profiles with
  `activeByDefault` are never suppressed at all.
* The suppression is evaluated **per call**, i.e. per profile list. `activeByDefault` in a parent POM
  is evaluated when the parent's own profile list is processed (§6.8), independently of the child's.
* Profiles keep declaration order, and the by-default ones are appended at the end.
* `profile.getSource()` is a **transient, non-XML** field on `Profile` that defaults to `"pom"`;
  `SettingsUtilsV4` sets it to `"settings.xml"` when converting `settings.xml` profiles. Because POM
  profiles and settings profiles are always selected in *separate* `getActiveProfiles` calls, the
  suppression above can only ever fire within a single POM's profile list.

`isActive(profile, context)`: `false` unless at least one activator reports `presentInConfig`, and
**all** activators that report `presentInConfig` return `true` → **the activators inside one
`<activation>` are ANDed.** Maven 4 short-circuits on the first `false`; Maven 3 evaluates all of them
and `&=`s the results — same verdict, only the set of reported problems can differ. A `RuntimeException`
from any activator ⇒ ERROR problem and the profile is inactive.

`activeByDefault` is examined **only** when `isActive` returned `false`; note that
`<activeByDefault>true</activeByDefault>` also makes `presentInConfig` false for every activator (it is
not an activator of its own), so a profile whose activation contains *only* `activeByDefault` is
handled entirely by the branch above.

⚠ Combining `activeByDefault` with another activator in the same `<activation>` means:
if the other activator matches → active via `isActive`; if it does not → the profile is still a
by-default candidate, and is activated unless some other POM profile activated explicitly.

### 6.2 `jdk` (`JdkVersionProfileActivator`)

`presentInConfig` ⇔ `activation.jdk != null`. The runtime version is the **system property
`java.version`**; missing/empty ⇒ ERROR problem, inactive.

* leading `!` → `!currentVersion.startsWith(rest)`;
* value starting with `[` or `(` → range test (below);
* otherwise → `currentVersion.startsWith(value)` (prefix match, so `1.8` matches `1.8.0_292`).

Range syntax: split on `,`. A token starting with `[` → inclusive lower bound; `(` → exclusive lower
bound; ending with `]` → inclusive upper; ending with `)` → exclusive upper; an empty token → an
unbounded end (`RangeValue("", closed=false)`). If fewer than two range values were produced, a second
value `RangeValue("99999999", closed=false)` is appended. Bracket characters are stripped with
`String.replace` and the remainder is `trim()`med.

Comparison (`getRelationOrder`): an empty bound value returns `+1` for the left bound and `-1` for the
right bound (i.e. unbounded). Otherwise the current version is filtered through
`[^\d._-] → ""` (all non-digit/dot/underscore/dash characters deleted), split on `[._-]`; the bound is
split on `.`; both lists are zero-padded to 3 elements; the first three elements are compared as
integers. On full equality, a non-closed bound returns `-1` for the left / `+1` for the right (i.e.
exclusive). `isInRange`: left relation `0` ⇒ in range; `< 0` ⇒ out; else the right relation must be
`<= 0`. `NumberFormatException` ⇒ WARNING problem, inactive.

Worked example: `java.version = 1.8.0_292` against the range `[1.8,)`. `FILTER_1` keeps digits, `.`,
`_` and `-`, so the string is unchanged; `FILTER_2` splits on `[._-]` → `["1","8","0","292"]`; only the
first three tokens are compared, i.e. `1.8.0`. The lower bound `1.8` is padded to `1.8.0` → equal →
`leftRelation == 0` (the bound is closed) → in range.

### 6.3 `os` (`OperatingSystemProfileActivator`)

`presentInConfig` ⇔ `activation.os != null`. Active iff **at least one** of `family`, `name`, `arch`,
`version` is non-null **and every non-null one matches** (AND).

Actual values: system properties `os.name`, `os.arch`, `os.version`, each defaulting to the JVM's
`Os.OS_NAME/OS_ARCH/OS_VERSION` and **lower-cased**.

| Sub-field | Rule |
|---|---|
| `family` | optional leading `!` negates; `Os.isFamily(test, actualOsName)`. Maven 4 passes the family through **as written**; Maven 3 lower-cases it first. `[M3≠M4]` |
| `name` | lower-cased; optional leading `!`; exact `equals` against `os.name` |
| `arch` | lower-cased; optional leading `!`; exact `equals` against `os.arch` |
| `version` | lower-cased **in Maven 4**; if it starts with `regex:` the remainder is used as a `String.matches` regex over the actual version (negation is *not* supported in the regex form); otherwise optional leading `!` and exact `equals`. Maven 3 does **not** lower-case the expected value and uses `equalsIgnoreCase`. `[M3≠M4]` |

Recognised families come from `org.apache.maven.impl.util.Os` (`windows`, `dos`, `mac`, `unix`,
`netware`, `os/2`, `tandem`, `win9x`, `z/os`, `os/400`, `openvms`, plus `Os.OS_FAMILY` itself).

### 6.4 `property` (`PropertyProfileActivator`)

`presentInConfig` ⇔ `activation.property != null`.

1. `name`: a leading `!` sets `reverseName` and is stripped. An empty/absent name ⇒ ERROR problem,
   inactive.
2. Value lookup, in order: **user property** `name` → if `name == "packaging"` and still unresolved,
   `context.getModelPackaging()` → **system property** `name`. `[M3≠M4]`: Maven 3 looks up user
   properties then system properties only, but the Maven 3 builder pre-seeds the *user* properties with
   `packaging = rawModel.getPackaging()` (`ProfileActivationContext.PROPERTY_NAME_PACKAGING`), so
   `<name>packaging</name>` works in both — in Maven 3 the value is the **top-level POM's** packaging
   for the whole lineage, in Maven 4 it is the packaging of the model in the activation context.
   Note user/system properties named `packaging` shadow the model packaging in both.
3. If `<value>` is present and non-empty: a leading `!` sets `reverseValue` and is stripped; the result
   is `reverseValue != value.equals(lookedUpValue)` — i.e. plain string equality against the raw
   looked-up value (`null` never equals anything, so `!x` on an unset property is *true*).
4. If `<value>` is absent or empty: presence check —
   `reverseName != (lookedUp != null && !lookedUp.isEmpty())`.

There is no interpolation of `name` or `value` in Maven 4. `[M3≠M4]` Maven 3 interpolates both against
(model properties → user properties → system properties) before matching.

### 6.5 `file` (`FileProfileActivator`)

`presentInConfig` ⇔ `activation.file != null`.

* If `<exists>` is non-empty it is used and `missing = false`; if `<missing>` is *also* present, Maven 4
  emits a WARNING that `missing` is ignored (Maven 3 silently ignores it). If only `<missing>` is
  non-empty it is used with `missing = true`. If neither is non-empty ⇒ inactive.
* The path is interpolated by `ProfileActivationContext.interpolatePath` with these sources, in order:
  `basedir` / `project.basedir` → the model's project directory (absolute);
  `project.rootDirectory` → `RootLocator.findRoot(basedir)`;
  then model properties → user properties → system properties.
  The interpolated path is then made absolute with `PathTranslator.alignToBaseDirectory` against the
  model's project directory (`\`/`/` normalised to the platform separator; already-absolute and
  drive-relative paths are kept).
* Existence: `Files.exists(path)`. `FileProfileActivator` calls `context.exists(path, /*glob=*/false)`,
  so **wildcards are not supported** for profile activation (the glob branch of
  `DefaultProfileActivationContext.doExists` exists for the Maven 4 `condition` activator only).
* Result: `missing != fileExists` — i.e. `<exists>` ⇒ active when the file exists, `<missing>` ⇒ active
  when it does not. A `MavenException` while checking ⇒ ERROR problem, inactive.
* Maven 3 is equivalent (`ProfileActivationFilePathInterpolator` uses the same source order), except
  that it returns "no match" if the path contains `${basedir}` and there is no project directory.

### 6.6 `packaging` and `condition` — Maven 4 only `[M3≠M4]`

* `<activation><packaging>` (model **4.1.0+**, `PackagingProfileActivator`): active iff
  `Objects.equals(activation.packaging, context.getModelPackaging())`. Not available in Maven 3.9
  (the shim under `compat/` is a Maven 4 back-port and reads the `packaging` *user property*).
* `<activation><condition>` (model **4.1.0+**, `ConditionProfileActivator`): a small expression
  language (`ConditionParser` + `ConditionFunctions`, with `${}` property access). Out of scope for a
  3.9-compatible implementation; a Maven 3 target should reject or ignore it.

### 6.7 What the activation context sees

`DefaultProfileActivationContext` (created by `DefaultModelBuilder.getProfileActivationContext`)
carries: active/inactive profile ids from the request, the request's system properties, the request's
user properties, and **one model**. From that model it exposes `artifactId`, `packaging`,
`properties`, base directory (`model.getProjectDirectory()`) and root directory
(`RootLocator.findRoot(basedir)`). While a `Record` is active every lookup is memoised so that the
parent-model cache can decide whether a cached parent is still valid.

Before either activation pass, the properties of the already-active **external (settings) profiles**
are folded into the *user* properties of the context (request user properties still win).

### 6.8 Parent POMs vs the current POM

* A **parent's** profiles are selected and injected inside `doReadAsParentModel`, using the
  **child's activation context** (`childProfileActivationContext`) — so `packaging`, `artifactId`,
  `basedir`, `rootDirectory` and the model properties used for `file`-path interpolation are the
  **child's**, not the parent's. After injection the parent's `<profiles>` are dropped.
  `[M3≠M4]` Maven 3 re-points `context.projectProperties` at **each lineage model's own properties**
  before selecting that model's profiles (the project directory stays the top POM's throughout, and
  the `packaging` user property stays the top POM's).
* The **current** POM's profiles are selected twice: against the file/raw model (B4) and again against
  the post-inheritance, pre-interpolation model (B14). The second pass is the one whose result is
  injected into the effective model, so in Maven 4 a profile can be activated by a property
  **inherited from the parent**. `[M3≠M4]` In Maven 3 only the raw pre-inheritance model is ever used,
  so inherited properties cannot activate a child profile.
* Reported active profiles (`result.getActivePomProfiles(modelId)`) are tracked per model id
  (`groupId:artifactId:version`): the child records only its *local* profiles (activated from
  `inputModel.getProfiles()`), and each parent records its own.

### 6.9 Injection order (`DefaultProfileInjector.injectProfiles`)

Profiles are merged one after another in list order with `sourceDominant = true`, so **later profiles
override earlier ones**. Only `ModelBase` fields plus `Profile.build` are injected — a profile can
never set `name`, `url`, `scm`, `packaging`, `licenses`, `developers`, `build.sourceDirectory`, … because
those fields do not exist on `Profile`/`BuildBase`. The call sequence is always **POM profiles first,
then external (settings) profiles**, so settings profiles win. `Profile.build` (a `BuildBase`) is
merged into `Model.build` (a `Build`) separately from `mergeModelBase`, creating an empty `Build` if the
model has none.

---

## 7. BOM import (`<type>pom</type><scope>import</scope>`)

`DefaultModelBuilder.importDependencyManagement` (step C4), then
`DefaultDependencyManagementImporter.importManagement`.

1. Runs on the **effective, post-inheritance, post-interpolation, post-pluginManagement,
   post-lifecycle-bindings** model — so `${…}` in a BOM coordinate has already been resolved, and
   inherited `<dependencyManagement>` entries are already part of the list being scanned.
2. The importing model pushes `groupId:artifactId:version` onto `importIds` for the duration.
3. Scan `dependencyManagement.dependencies` in order. An entry is an import iff
   `"pom".equals(type) && "import".equals(scope)`. Matching entries are **removed** from the
   dependencyManagement list (they never appear in the effective model).
   Maven 4 additionally skips (leaves in place) entries whose `type` is `bom`. `[M3≠M4]`
4. Each import must have non-empty `groupId`, `artifactId` **and `version`**, else ERROR and skip
   (no version inference, no dependencyManagement lookup for the BOM's own version).
5. Cycle handling: if `groupId:artifactId:version` is already in `importIds`, report
   ERROR "The dependencies of type=pom and with scope=import form a cycle: …" and skip that import.
   **There is no depth limit** — recursion is bounded only by cycle detection.
6. Resolution: reactor first (`resolveReactorModel`), then the repositories. A reactor-internal BOM in
   a `BUILD_PROJECT` build produces a WARNING ("BOM imports from within reactor should be avoided").
   Failure ⇒ ERROR "Non-resolvable import POM …", skip.
7. The BOM is built as a **full effective model** via a derived session with
   `RequestType.CONSUMER_DEPENDENCY` (which skips lifecycle-bindings injection and plugin-configuration
   expansion, and uses `MINIMAL` validation), inheriting the outer request's system and user
   properties and the current repository list, and passing the current `importIds` down — so a BOM's
   own imports are resolved recursively, and its own parent chain and interpolation are applied.
   Results are cached per (repositories, G, A, V, `IMPORT`).
8. Only the BOM's `getDependencyManagement()` is retained (an absent one becomes an empty
   `DependencyManagement`).
9. **Exclusions on the import (MNG-5600, Maven 4 only `[M3≠M4]`)**: if the import entry declares
   `<exclusions>`, managed entries matching an exclusion (`groupId`/`artifactId`, `*` wildcard allowed
   for either) are dropped from the imported set, and every surviving entry gets the exclusion list
   appended to its own exclusions.
10. Merge (`DefaultDependencyManagementImporter`): a `LinkedHashMap` keyed by the **management key**
    is pre-loaded with the model's **locally declared (and inherited)** managed dependencies, in
    order. Then each BOM, in declaration order, contributes its entries with `putIfAbsent`.
    Therefore:
    * **local/inherited management always wins over any BOM**;
    * between two BOMs managing the same key, **the first-declared BOM wins**;
    * a losing BOM entry that differs from the winner and whose key was not locally declared produces
      a **WARNING** "Ignored POM import for: … as already imported …" (Maven 4 only, MNG-8004);
    * the resulting order is: local entries in their original order, then newly imported entries in
      (BOM order, entry order).
11. **An import brings in nothing but `dependencyManagement` entries** — no properties, no
    `<dependencies>`, no plugins, no repositories. Note that the BOM's *own* properties were used while
    building the BOM's effective model, so its managed versions are already resolved; a property
    defined in the BOM is **not** visible to the importing POM.
12. With location tracking enabled, each newly imported dependency records `importedFrom` so tooling
    can attribute it to the BOM.

---

## 8. dependencyManagement injection

`DefaultDependencyManagementInjector.ManagementModelMerger.mergeManagedDependencies(model)`:

1. Index `model.getDependencies()` by **management key** (`groupId:artifactId:type[:classifier]`).
2. For each managed dependency whose key matches a declared dependency, run
   `mergeDependency(declared, managed, /*sourceDominant=*/false, {})` — i.e. **management only fills
   fields the declaration left `null`**.
3. Rebuild the dependency list in the original declaration order.
4. Managed entries with no matching declared dependency have **no effect on `<dependencies>`** (they
   remain in `dependencyManagement` for transitive resolution, which is outside the model builder).

Per-field behaviour:

| Dependency field | Can management set it? | Does management override an explicit value? |
|---|---|---|
| `version` | yes (when the declaration has none) | **no** |
| `scope` | yes | **no** |
| `systemPath` | yes | **no** |
| `type` | part of the key — a managed entry with a different `type` simply does not match | n/a |
| `classifier` | part of the key | n/a |
| `groupId`, `artifactId` | part of the key | n/a |
| `optional` | **never** — `mergeDependency_Optional` is an explicit no-op | no |
| `exclusions` | yes, **all-or-nothing**: the managed exclusion list is copied **only if the declaration has no exclusions at all**; there is no per-exclusion union | no |

Note that `scope` is injected here **before** `DefaultModelNormalizer.injectDefaultValues` defaults it
to `compile` (C5 precedes C6), which is exactly why the default cannot live in the model schema.

`DefaultPluginManagementInjector` is the analogous pass for plugins (C2, i.e. **before** BOM import and
dependencyManagement injection): plugins are matched by `groupId:artifactId`, and each match is
`mergePlugin(ownPlugin, managedPlugin, false)` → management fills gaps only, executions are merged by
id with the plugin's own values winning, and configurations are merged with the plugin's own config
dominant. `pluginManagement` entries with no matching `<plugin>` do not add plugins to the build (but
they are consulted by lifecycle-bindings injection for plugins the lifecycle contributes).

---

## 9. Normalization

`DefaultModelNormalizer` has two independent entry points.

### 9.1 `mergeDuplicates(model, …)` — steps B5, B13, B18

* **Duplicate build plugins**: walk `build.plugins`, key `groupId:artifactId`. On a repeat, the later
  occurrence is merged as `DuplicateMerger.mergePlugin(later, first)` = `mergePlugin(later, first,
  /*sourceDominant=*/false, {})` — the **later** declaration is the target and wins per field, while
  the first fills gaps (its executions are merged in by id, its configuration is recessive). The map
  keeps the **first** occurrence's position but the merged value. The build is rewritten only if the
  count actually changed.
* **Duplicate dependencies**: `model.dependencies` is put into a `LinkedHashMap` keyed by the
  management key — **last declaration wins, first position is kept** (deliberate Maven 2.x
  bug-compatibility). Only rewritten if the count changed.
* Nothing else is touched: profiles, dependencyManagement, pluginManagement and reporting plugins are
  **not** de-duplicated.

### 9.2 `injectDefaultValues(model, …)` — step C6

* For every entry of `model.dependencies` **and** of every `build.plugins[*].dependencies`: if `scope`
  is `null` or empty, set it to **`compile`**.
* Nothing else. (Other defaults — `type=jar`, `plugin.groupId=org.apache.maven.plugins`,
  `execution.id=default`, `inherited=true`, the `build.*` directories — come from the model schema
  defaults applied by the reader and from the super POM, not from this class.)

### 9.3 Related default-injection you must not forget

* Model schema defaults (`api/maven-api-model/src/main/mdo/maven.mdo`), applied when a model instance
  is built "with defaults": `packaging=jar`, `Dependency.type=jar`, `Plugin.groupId=
  org.apache.maven.plugins`, `PluginExecution.id=default`, `ReportSet.id=default`, `inherited` ⇒ `true`
  when absent, `child*InheritAppendPath` ⇒ `true` when absent.
* The **super POM** (`impl/maven-impl/src/main/resources/org/apache/maven/model/pom-4.0.0.xml`) is the
  top of every parent chain and supplies `project.build.sourceEncoding`,
  `project.reporting.outputEncoding`, `project.build.outputTimestamp`, the `build.*` directories,
  `finalName`, the default `src/main/resources` + `src/test/resources` resource entries and
  `reporting.outputDirectory`. `[M3≠M4]` In this clone the compat (Maven 3) copy is byte-identical to
  the Maven 4 one, i.e. it has **no** `central` `<repositories>`/`<pluginRepositories>` block and no
  `<pluginManagement>`; the super POM shipped with a real Maven 3.9 distribution does contain those.
  Verify against a 3.9 distribution before relying on this.
* `DefaultModelPathTranslator` (C1) is where relative `build`/`reporting` paths become absolute:
  `build.sources[*].directory` (targetPath deliberately left relative), `build.directory`,
  `sourceDirectory`, `testSourceDirectory`, `scriptSourceDirectory`, `resources[*].directory`,
  `testResources[*].directory`, `filters`, `outputDirectory`, `testOutputDirectory`,
  `reporting.outputDirectory`. `DefaultPathTranslator` normalises separators, leaves absolute paths
  alone, resolves drive-relative Windows paths against the drive root, and otherwise
  `basedir.resolve(path).normalize()`.

---

## 10. Maven 3.9 vs Maven 4 divergences

Ordered by how likely they are to bite a 3.9-compatible implementation.

| # | Area | Maven 3.9 | Maven 4 (this clone) |
|---|---|---|---|
| 1 | **Child profile injection vs inheritance** | The child's active profiles are injected **before** inheritance assembly, into the same model that then becomes the inheritance target. | The child's profiles are injected **after** inheritance (B15). Observable: with a parent declaring `<resources>` and the child declaring none but a profile declaring some, Maven 3 yields only the profile's resources (child list non-empty ⇒ parent's dropped) while Maven 4 yields parent resources **followed by** the profile's. List positions for dependencies/plugins/repositories can also differ. |
| 2 | **Number of activation passes for the current POM** | Once, against the raw pre-inheritance model. | Twice: raw model (B4) and post-inheritance pre-interpolation model (B14). A profile activated by a property **inherited from the parent** activates in Maven 4 but not in Maven 3. |
| 3 | **Activation context for parent profiles** | Each lineage model's profiles are selected with `projectProperties` = that model's own properties (project directory and the `packaging` user property stay the top POM's). | Parent profiles are selected with the **child's** context: child properties, child packaging, child artifactId, child basedir. |
| 4 | **Interpolation of `<activation>` values** | `os.{name,family,arch,version}`, `property.{name,value}` and `jdk` are interpolated before matching, with sources model-properties → user → system. | Not interpolated at all; only `file/@exists|@missing` paths are (via `interpolatePath`). |
| 5 | **Interpolation priority: model reflection vs properties** | Prefixed model reflection (`${project.*}`, `${pom.*}`) is consulted **before** user properties and model properties. | User properties, then model properties, then prefixed reflection. A `-Dproject.version=…` or a POM property literally named `project.version` wins in Maven 4 but not in Maven 3. |
| 6 | **`pom.` prefix** | Always accepted (with a deprecation warning from `ProblemDetectingValueSource`). | Accepted **only for non-`BUILD_PROJECT` request types**; `${pom.version}` does not resolve while building a project. |
| 7 | **Interpolation extras** | No `:-`/`:+` operators, no `\{`/`\}` escaping, no `${project.rootDirectory}`; build-timestamp source registered only when the project directory is known. | `${x:-y}` / `${x:+y}`, escaping, `${project.rootDirectory}` / `${project.baseUri}` sub-expressions, timestamp always available. |
| 8 | **Interpolation failure modes** | A cycle raises an `InterpolationException` reported as ERROR; unresolvable expressions are left as written. | A cycle reports ERROR **and sets the whole field to `null`**; unresolvable expressions are left as written. |
| 9 | **`packaging` profile activation** | Only via `<property><name>packaging</name>`, resolved from the *user* properties which the builder pre-seeds with the **top-level** POM's packaging. | Same property path (with a model-packaging fallback per activation context) **plus** a dedicated `<activation><packaging>` element and a `<condition>` expression activator (model 4.1.0+). |
| 10 | **`os.family` / `os.version` case handling** | `family` lower-cased before `Os.isFamily`; `version` compared with `equalsIgnoreCase` and the `regex:` pattern used as written. | `family` passed as written; `version` lower-cased and compared with `equals`, and the `regex:` pattern is lower-cased too. |
| 11 | **`file` activation with both `exists` and `missing`** | `missing` silently ignored. | Same behaviour plus a WARNING. |
| 12 | **BOM import exclusions (MNG-5600)** | Not supported — `<exclusions>` on an import entry is ignored. | Supported: matching managed entries are filtered out and the exclusions are appended to the survivors. |
| 13 | **Conflicting BOM imports** | Silently first-wins. | First-wins **plus** a WARNING (MNG-8004). |
| 14 | **`<type>bom</type>` in dependencyManagement** | No such type; any non-`pom` type is simply not an import. | Explicitly excluded from the import scan and left in `dependencyManagement`. The extra clause is redundant with the `type == "pom"` test, so **no behavioural difference**. |
| 15 | **Reporting steps in phase 2** | `reportConfigurationExpander` and `reportingConverter` run between `injectDefaultValues` and `pluginConfigurationExpander`. | Both removed. Also, `DefaultPluginConfigurationExpander.expandReport` computes a new reporting-plugin list and **discards it**, so reporting `<configuration>` is *not* pushed into report sets at all. |
| 16 | **URL child-path adjustment source list** | `parent.getModules()` — the only option. | Still `parent.getModules()` only, so a 4.1.0 parent using `<subprojects>` gets an empty adjustment. Implement `modules`. |
| 17 | **XML `<configuration>` merge** | `plexus-utils` `Xpp3DomUtils.mergeIntoXpp3Dom`; no `combine.self="remove"`. | `DefaultXmlService.doMerge`; adds `combine.self="remove"`, and the dominant's text value always wins. |
| 18 | **Super POM content** | Contains the `central` repository and pluginRepository plus a small `<pluginManagement>` block (verify against a real 3.9 distribution — this clone's compat copy has been stripped to match Maven 4). | No repositories in the super POM; central is supplied by the resolver/settings layer. |
| 19 | **Model-level features with no 3.9 equivalent** | — | `<subprojects>`, `<mixins>`, `Build.sources`, `root`/`preserveModelVersion`, subproject auto-discovery, `Dependency` version/groupId inference from the reactor (`transformFileToRaw`), parent-model caching keyed by activation record, `MODEL_VERSION_4_1_0`. All gated on model version > 4.0.0, so a 4.0.0-only implementation can ignore them. |
| 20 | **`distributionManagement.relocation`** | Never inherited (absent from the generated merge method list). | Never inherited (explicit no-op override). **No behavioural difference** — listed because the two implementations look different. |

---

## 11. Implementation checklist for the Rust port (3.9 target)

1. Read POM → apply schema defaults (`packaging=jar`, `type=jar`, `plugin.groupId`,
   `execution.id=default`, `inherited=true`, `child*InheritAppendPath=true`).
2. Build the lineage: for each model, normalize duplicates, interpolate its profiles' `<activation>`
   values (model props → user → system), select active profiles, inject them into **that** model,
   then read its parent; terminate at the super POM.
3. Assemble inheritance from the super POM downwards, one `merge(child, parent, sourceDominant=false)`
   per level, with the `child-directory` / `child-path-adjustment` hints recomputed per level.
4. Interpolate the resulting model once (value-source order per §5.3 but with prefixed reflection
   *before* user/model properties, per divergence #5), then interpolate `parent.version`, then
   collapse `/../` in the five URL fields.
5. Phase 2 in exactly this order: path translation → pluginManagement injection → lifecycle bindings →
   BOM import → dependencyManagement injection → `scope=compile` defaults → plugin configuration
   expansion → effective-model validation.
6. Everywhere a collection is merged, use the exact key from §2.1 and remember that a key collision
   **replaces the whole element** — never a field-wise merge — except for the four hand-written
   collections (`plugins`, `executions`, `reportSets`, `repositories`) and the four management/
   normalization passes, which do merge element fields.
