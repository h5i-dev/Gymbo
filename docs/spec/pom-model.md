# POM Model — Compatibility Specification for `jv`

> **Provenance.** This document is derived from Apache Maven schema and implementation sources,
> licensed under the **Apache License, Version 2.0**. It is a description of the observable POM
> contract, written for a clean-room Rust reimplementation.
>
> **Clone:** `_reference/maven`
> **Commit:** `945813a7d4d91f32fe92d2c5a81d0a8223bc10b9`
> **Version in root `pom.xml`:** `4.1.0-SNAPSHOT` (Maven 4 development line)
>
> **Primary source:**
> - `_reference/maven/api/maven-api-model/src/main/mdo/maven.mdo` (Modello schema, 3617 lines)
>
> **Cross-checked against:**
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelValidator.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelNormalizer.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultInheritanceAssembler.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/MavenModelMerger.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultProfileSelector.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultProfileInjector.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/DefaultModelBuilder.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/model/profile/*.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/type/DefaultTypeProvider.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/type/DefaultType.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/ArtifactDescriptorUtils.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/DefaultRepositoryFactory.java`
> - `_reference/maven/impl/maven-impl/src/main/java/org/apache/maven/impl/resolver/scopes/Maven3ScopeManagerConfiguration.java`
> - `_reference/maven/impl/maven-core/src/site/markdown/artifact-handlers.md` (the Maven 3 legacy handler table)
> - `_reference/maven/impl/maven-core/src/main/java/org/apache/maven/lifecycle/providers/packaging/*.java`
> - `_reference/maven/impl/maven-core/src/main/java/org/apache/maven/artifact/resolver/filter/ExclusionArtifactFilter.java`
>
> **Target behaviour: Maven 3.9.** Where the Maven 4 sources in this clone differ from Maven 3.9 in a
> way that changes parsing or defaults, the divergence is marked **[M4]** inline and collected in
> [§14](#14-maven-3--maven-4-divergences-summary). `jv` implements the **Maven 3** column.

---

## Table of contents

0. [Conventions and reading rules](#0-conventions-and-reading-rules)
1. [`<project>` top-level elements](#1-project-top-level-elements)
2. [`<parent>`](#2-parent)
3. [Coordinates, versioning and `<packaging>`](#3-coordinates-versioning-and-packaging)
4. [`<dependency>`, `<exclusions>`](#4-dependency-exclusions)
5. [`<dependencyManagement>`](#5-dependencymanagement)
6. [`<properties>`](#6-properties)
7. [`<modules>` / `<subprojects>`](#7-modules--subprojects)
8. [`<build>`](#8-build)
9. [`<repositories>` and `<pluginRepositories>`](#9-repositories-and-pluginrepositories)
10. [`<profiles>` and `<activation>`](#10-profiles-and-activation)
11. [`<distributionManagement>`](#11-distributionmanagement)
12. [Dependency `type` table](#12-dependency-type-table)
13. [Effective-POM construction order](#13-effective-pom-construction-order)
14. [Maven 3 / Maven 4 divergences summary](#14-maven-3--maven-4-divergences-summary)

---

## 0. Conventions and reading rules

### 0.1 Modello → XML mapping

The POM schema is a Modello model. The mapping rules that matter:

- A `<class>` maps to an XML element; the element name is the lower-camel class name unless
  `xml.tagName` overrides it (`Model` → `project`; `ReportPlugin` → `plugin`).
- A `<field>` maps to a child element named after the field, unless `xml.attribute="true"`
  (then it is an XML **attribute**) or `xml.tagName` overrides the name.
- A field with `<association><multiplicity>*</multiplicity>` is a **list**: a wrapper element named
  after the field, containing repeated items named by **singularising the field name** — not by the
  association's class name. So `pluginRepositories` (of class `Repository`) serialises as
  `<pluginRepositories><pluginRepository>…`, and `modules` (of class `String`) as
  `<modules><module>…`.
- `<type>Properties</type>` with `xml.mapStyle="inline"` is a **map**: each entry is
  `<key>value</key>` directly under the wrapper.
- `<type>DOM</type>` is **opaque XML**: an arbitrary element tree that must be preserved verbatim
  (attributes included) — see [§8.5](#85-configuration-is-opaque).
- `<field xml.transient="true">` is **not** serialised in XML (`Model.pomFile`,
  `PluginExecution.priority`, `Resource.mergeId`). Never parse or emit these.
- `<version>X+</version>` / `<version>X/Y</version>` on a class or field is the **model version
  range** in which the element exists. `4.0.0+` means "since modelVersion 4.0.0" — i.e. present in
  Maven 3. `4.1.0+` means **Maven 4 only**. `4.0.0/4.0.99` means removed in 4.1.0. Note that the
  mdo's `3.0.0+` markers are vestigial (Maven 1/2 lineage) and behave as "always present".

### 0.2 Two kinds of default

Distinguish carefully; conflating them breaks inheritance and merge semantics.

1. **Schema defaults** — an mdo `<defaultValue>`. The generated model pre-populates the field, so a
   reader observes the default as if it had been written in the XML. `jv` should materialise these
   at parse time. Examples: `packaging=jar`, `dependency/type=jar`, `plugin/groupId=org.apache.maven.plugins`.
2. **Accessor defaults** — the field is declared `<type>String</type>` (or is absent from the mdo
   entirely) and the default is applied by a Java accessor or by a build phase. Here **absent and
   explicitly-set-to-the-default-value are different states**, and the difference is visible to
   inheritance/merging. Examples: `dependency/optional` (`isOptional()` → `false` when null),
   `repositoryPolicy/enabled` (`isEnabled()` → `true` when null), `plugin/inherited`
   (`isInherited()` → `true` when null), `plugin/extensions` (`isExtensions()` → `false` when null),
   and `dependency/scope` (injected as `compile` by the model normaliser — deliberately *not* an
   mdo default, so that `<dependencyManagement>` can supply a scope).

The mdo comment on `Dependency.scope` states this explicitly: the `compile` default is commented out
so it can be injected from `<dependencyManagement>` instead.

### 0.3 Boolean-typed-as-String fields

Several semantically-boolean fields are `String` in the model, precisely so that "unset" is
representable. Parse with Java `Boolean.parseBoolean` semantics: **case-insensitive `"true"` is
true; every other non-null string, including garbage, is `false`.**

| Element | Owner | Accessor default when absent |
|---|---|---|
| `optional` | `dependency` | `false` |
| `inherited` | `plugin`, `execution`, `reportPlugin`, `reportSet` | `true` |
| `extensions` | `plugin` | `false` |
| `enabled` | `releases`, `snapshots` | `true` |
| `filtering` | `resource`, `testResource` | `false` |
| `excludeDefaults` | `reporting` | `false` |
| `child.project.url.inherit.append.path` | `project` (attribute) | `true` |
| `child.scm.*.inherit.append.path` | `scm` (attributes) | `true` |
| `child.site.url.inherit.append.path` | `site` (attribute) | `true` |

Fields declared `<type>boolean</type>` (真 booleans, no unset state): `activation/activeByDefault`
(`false`), `deploymentRepository/uniqueVersion` (`true`), `notifier/sendOn*` (`true`),
`project@root` **[M4]** (`false`), `project@preserve.model.version` **[M4]** (`false`),
`source/stringFiltering` **[M4]** (`false`), `source/enabled` **[M4]** (`true`).

### 0.4 XML namespace and `modelVersion`

- Namespace: `http://maven.apache.org/POM/${version}` where `${version}` is the model version,
  e.g. `http://maven.apache.org/POM/4.0.0`.
- `<modelVersion>` is **required** (ERROR if empty). Known values in this clone:
  `4.0.0` (Maven 3), `4.1.0`, `4.2.0` (both **[M4]**).
- If `<modelVersion>` is absent but the namespace URI starts with `http://maven.apache.org/POM/`,
  Maven infers the model version from the namespace suffix (`DefaultModelBuilder`).
- **`jv` accepts only `4.0.0`.** Under `4.0.0` the validator raises ERROR for any 4.1.0+ element,
  e.g. `<subprojects>` → *"unexpected subprojects element"*.

### 0.5 Validation severities

`DefaultModelValidator` classifies problems as `FATAL` / `ERROR` / `WARNING`, at validation levels
`MINIMAL` (0), `MAVEN_2_0` (20), `MAVEN_3_0` (30), `MAVEN_3_1` (31), `MAVEN_4_0` (40),
`MAVEN_4_1` (41), `MAVEN_4_2` (42); `STRICT` = the highest (42 in this clone, 31 in Maven 3.9).
"Required" in the tables below means the validator emits ERROR/FATAL when missing, not merely that
the mdo says `<required>true</required>`. The two do not always agree; the validator wins.

Where a rule below is described as "`errOn30`" / "`errOn31`", the helper is
`getSeverity(level, threshold)`: **WARNING** when `level < threshold`, **ERROR** otherwise. Since a
normal build runs at `STRICT`, every `errOn30`/`errOn31` rule is an **ERROR** in practice; the WARNING
form only appears when a POM is read as a dependency's artifact descriptor (which uses a lower level,
so a malformed third-party POM warns instead of failing the build). `jv` should mirror this: strict
for the project under build, lenient for POMs pulled from a repository.

Banned-character sets (from `DefaultModelValidator`):
- `ILLEGAL_FS_CHARS = \ / : " < > | ? *` — applied to versions (`ILLEGAL_VERSION_CHARS`) and
  repository ids (`ILLEGAL_REPO_ID_CHARS`).
- Coordinate ids (`groupId`, `artifactId`) must match the "coordinates id" rule; with wildcards
  allowed for exclusions (see [§4.4](#44-exclusion-matching-semantics)).

---

## 1. `<project>` top-level elements

`Model` (`xml.tagName="project"`, root element) extends `ModelBase`. The table merges both classes,
which is what a parser sees. "Inherited" describes parent → child inheritance as implemented by
`DefaultInheritanceAssembler` + `MavenModelMerger`.

### 1.1 Attributes on `<project>`

| Attribute | Type | Default | Inherited | Since | Notes |
|---|---|---|---|---|---|
| `child.project.url.inherit.append.path` | String (bool) | `"true"` | no | Maven 3.6.1 | Controls whether children append a path segment to the inherited `<url>`. |
| `root` | boolean | `false` | no | **[M4]** 4.1.0 | Marks the source-tree root (may hold `.mvn`). Reject/ignore for 4.0.0. |
| `preserve.model.version` | boolean | `false` | no | **[M4]** 4.1.0 | Build-POM downgrade control. Reject/ignore for 4.0.0. |

### 1.2 Child elements

| XML element | Type | Cardinality | Default | Inherited | Since | Notes |
|---|---|---|---|---|---|---|
| `modelVersion` | String | 0..1 (required) | — | **no** (`mergeModel_ModelVersion` is a no-op) | 4.0.0 | See [§0.4](#04-xml-namespace-and-modelversion). |
| `parent` | `Parent` | 0..1 | — | no | 4.0.0 | [§2](#2-parent). |
| `mixins` | list `<mixins><mixin>` of `Mixin` | 0..n | — | **no** (`mergeModel_Mixins` no-op) | **[M4]** 4.2.0 | Not in Maven 3. |
| `groupId` | String | 0..1 | inherited from parent | **yes** | 4.0.0 | Required in the effective model. |
| `artifactId` | String | 1 (required) | — | **no** (`mergeModel_ArtifactId` no-op) | 4.0.0 | |
| `version` | String | 0..1 | inherited from parent | **yes** | 4.0.0 | Required in the effective model. |
| `packaging` | String | 0..1 | **`jar`** (schema default) | **yes** | 4.0.0 | [§3.4](#34-legal-packaging-values). |
| `name` | String | 0..1 | — | **no** (source-dominant only) | 3.0.0 | |
| `description` | String | 0..1 | — | yes | 3.0.0 | |
| `url` | String | 0..1 | parent url `[+ path adjustment]` | yes, with path append | 3.0.0 | Child appends `artifactId` (or `project.directory` property) unless the append-path attribute is `false`. |
| `inceptionYear` | String | 0..1 | — | yes | 3.0.0 | Informational. |
| `organization` (alias `organisation`) | `Organization` | 0..1 | — | yes, only if child has none | 3.0.0 | Irrelevant to resolution. |
| `licenses` | list `<licenses><license>` | 0..n | — | yes, all-or-nothing (child list wins if non-empty) | 3.0.0 | Irrelevant to resolution. |
| `developers` | list `<developers><developer>` | 0..n | — | yes, all-or-nothing | 3.0.0 | Irrelevant. |
| `contributors` | list `<contributors><contributor>` | 0..n | — | yes, all-or-nothing | 3.0.0 | Irrelevant. |
| `mailingLists` | list `<mailingLists><mailingList>` | 0..n | — | yes, all-or-nothing | 3.0.0 | Irrelevant. |
| `prerequisites` | `Prerequisites` | 0..1 | `maven` = `2.0` | **no** (`mergeModel_Prerequisites` no-op) | 4.0.0 | Only `<maven>`; meaningful for `maven-plugin` packaging. |
| `scm` | `Scm` | 0..1 | `tag` = `HEAD` | yes, with path append | 4.0.0 | Irrelevant to resolution. |
| `issueManagement` | `IssueManagement` | 0..1 | — | yes, only if child has none | 4.0.0 | Irrelevant. |
| `ciManagement` | `CiManagement` | 0..1 | — | yes, only if child has none | 4.0.0 | Irrelevant. |
| `distributionManagement` | `DistributionManagement` | 0..1 | — | partly — see [§11](#11-distributionmanagement) | 4.0.0 | `<relocation>` is **never** inherited. |
| `properties` | inline map `<properties><key>value</key>` | 0..1 | empty | **yes** (key-wise merge, child wins) | 4.0.0 | [§6](#6-properties). |
| `dependencyManagement` | `DependencyManagement` | 0..1 | — | yes (list merge by management key) | 4.0.0 | [§5](#5-dependencymanagement). |
| `dependencies` | list `<dependencies><dependency>` | 0..n | — | **yes** (parent entries prepended, merged by management key) | 3.0.0 | [§4](#4-dependency-exclusions). |
| `repositories` | list `<repositories><repository>` | 0..n | — | yes (merge by `id`) | 4.0.0 | [§9](#9-repositories-and-pluginrepositories). |
| `pluginRepositories` | list `<pluginRepositories><pluginRepository>` | 0..n | — | yes (merge by `id`) | 4.0.0 | [§9](#9-repositories-and-pluginrepositories). |
| `modules` | list `<modules><module>` of String | 0..n | — | **no** | 4.0.0/4.2.0, `@Deprecated(since="4.0.0")` | [§7](#7-modules--subprojects). |
| `subprojects` | list `<subprojects><subproject>` of String | 0..n | — | **no** | **[M4]** 4.1.0 | [§7](#7-modules--subprojects). |
| `build` | `Build` | 0..1 | Super POM values | yes (deep merge) | 3.0.0 | [§8](#8-build). |
| `profiles` | list `<profiles><profile>` | 0..n | — | **no** (`mergeModel_Profiles` no-op) | 4.0.0 | [§10](#10-profiles-and-activation). Parent profiles are injected into the parent model *before* inheritance, so their **effects** propagate while the `<profile>` elements themselves do not. |
| `reporting` | `Reporting` | 0..1 | `outputDirectory` = `${project.build.directory}/site`, `excludeDefaults` = `false` | yes (by plugin key, honouring `inherited`) | 4.0.0 | Site only; irrelevant to resolution. Contains `<plugins><plugin>` of class `ReportPlugin`. |
| `reports` | DOM | 0..1 | — | n/a | 4.0.0 only, `@Deprecated` | Ignored by Maven. Parse-and-drop. |
| `pomFile` | (transient) | — | — | — | **[M4]** 4.1.0 | `xml.transient` — never in XML. |

**Not relevant to resolution / effective-POM for `jv`, but they exist and must parse without error:**
`reporting`, `organization`, `licenses`, `developers`, `contributors`, `mailingLists`,
`issueManagement`, `ciManagement`, `distributionManagement/site`, `scm`, `prerequisites`,
`inceptionYear`, `description`, `name`, `url`. Their sub-fields are deliberately not enumerated here.

---

## 2. `<parent>`

Class `Parent`, model version `4.0.0+`. **The children of `<parent>` are not interpolated** — they
must be literal values (the mdo says so explicitly, and `DefaultModelValidator` raises FATAL on
expressions in `parent.groupId` / `parent.artifactId` / `parent.version` for modelVersion ≥ 4.1.0,
WARNING for 4.0.0).

| XML element | Type | Required | Default | Notes |
|---|---|---|---|---|
| `groupId` | String | **yes** (FATAL if empty) | — | |
| `artifactId` | String | **yes** (FATAL if empty) | — | |
| `version` | String | **yes** (FATAL if empty) | — | mdo does not mark it required, but the validator does. `LATEST` / `RELEASE` → WARNING (deprecated). |
| `relativePath` | String | no | **Maven 3.9: `../pom.xml`**; **[M4]** `..` | See [§2.1](#21-relativepath-resolution). |

Additional validator rules:
- FATAL if `parent.groupId:parent.artifactId` equals the project's own `groupId:artifactId`
  (*"the parent element cannot have the same groupId:artifactId as the project"*).

### 2.1 `relativePath` resolution

**Maven 3.9 (what `jv` implements).** The mdo schema default is the literal string `../pom.xml`, so
the field is never null after parsing. Lookup order:
1. Resolve `relativePath` against the child POM's directory. If it names a directory, append the
   POM file name. If a POM exists there **and** its coordinates match the declared
   `groupId`/`artifactId` (when both are given), use it.
2. Otherwise look the parent up by `groupId:artifactId:version` in the reactor.
3. Otherwise resolve it as an artifact from the local repository, then remote repositories.

An explicitly **empty** `<relativePath/>` (or `<relativePath></relativePath>`) disables the
filesystem lookup entirely and forces repository resolution. This is a common idiom and must be
supported: empty string ≠ absent.

If `relativePath` points at a POM whose coordinates do **not** match the declared parent
coordinates, Maven 3.9 emits a WARNING (*"points at … instead of …, please verify your project
structure"*) and falls through to repository resolution.

**[M4] divergence.** In this clone the mdo carries **no** `<defaultValue>` for `relativePath`; the
description says it "defaults to `..`", i.e. the *directory*, and the builder implements a different
order — explicit `relativePath` first, then **reactor by GA**, then the default `..` directory, then
local repo, then remote (`DefaultModelBuilder.readParentLocally`). A coordinate mismatch is FATAL
rather than WARNING when modelVersion ≥ 4.1.0 *and* `relativePath` was explicit. `jv` must implement
the Maven 3 order and the `../pom.xml` default.

### 2.2 `<mixins>` **[M4]**

`Mixin` extends `Parent`, adding `classifier` and `extension` (both String, 4.1.0+). Model version
`4.2.0+`, not inherited. Does not exist in Maven 3 — reject under modelVersion 4.0.0.

---

## 3. Coordinates, versioning and `<packaging>`

### 3.1 Coordinates

| Element | Required in effective model | Inheritable | Notes |
|---|---|---|---|
| `groupId` | yes (ERROR if empty) | yes | Conventionally a reverse-DNS package name. |
| `artifactId` | yes (FATAL if empty) | **no** | |
| `version` | yes (ERROR if empty) | yes | |
| `packaging` | yes (ERROR if empty) | yes | schema default `jar` |

Model id string (used in log/tree output and `InputSource.modelId`):
`groupId:artifactId:packaging:version`, with the literal `[inherited]` substituted for a null
`groupId` or `version`. Parent id string: `groupId:artifactId:pom:version`.

### 3.2 Version strings

- A `<dependency><version>` is a **version requirement**, not necessarily a literal: a soft
  requirement (`3.2.1`) or a range (`[3.2.0,)`, `[1.0,2.0)`, `(,1.0]`, `[1.0]`).
- `LATEST` and `RELEASE` are accepted with a WARNING (deprecated) for both `parent.version` and
  dependency versions.
- Versions may not contain `\ / : " < > | ? *`.
- `-SNAPSHOT` suffix marks a snapshot; `<uniqueVersion>` in a deployment repository controls
  timestamped snapshot naming on deploy only.

### 3.3 What `packaging` implies

`packaging` is *not* the same axis as a dependency's `type`, though the value spaces overlap.
`packaging` selects (a) the lifecycle mapping — which plugin goals are bound to which phases — and
(b) the artifact handler, hence the produced artifact's **extension**. When another project depends
on this artifact, that consumer's `<type>` (default `jar`) drives extension/classifier lookup
([§12](#12-dependency-type-table)); for resolution purposes the producer's `packaging` matters only
in that a `pom`-packaged project produces no main artifact beyond the POM itself.

### 3.4 Legal `packaging` values

The mdo does **not** enumerate them — it says only "Plugins can create their own packaging … so this
list does not contain all possible types." The authoritative built-in set is the set of registered
lifecycle-mapping providers
(`impl/maven-core/src/main/java/org/apache/maven/lifecycle/providers/packaging/`, each `@Named(...)`):

| `packaging` | Produced artifact extension | Implies | Maven 3.9? |
|---|---|---|---|
| `pom` | `pom` (the POM itself; no other main artifact) | Required for aggregator projects (see below). BOM idiom: `pom` + `<dependencyManagement>` with `import` scope. | yes |
| `jar` | `jar` | Default packaging. Java classes archive. | yes |
| `maven-plugin` | `jar` | Also produces plugin descriptor; `<prerequisites><maven>` applies. | yes |
| `ejb` | `jar` | Consumers may depend on it as type `ejb` or `ejb-client`. | yes |
| `war` | `war` | Bundles its own dependencies (`includesDependencies = true`), so it is *not* added to a consumer's classpath. | yes |
| `ear` | `ear` | Same self-contained semantics as `war`. | yes |
| `rar` | `rar` | Same self-contained semantics. | yes |
| `bom` | `pom` | **[M4] only.** Bill-of-materials; behaves like `pom`. Not a legal built-in packaging in Maven 3.9 (a `bom`-packaged POM fails there with "Unknown packaging"). | **no** |

Notes:
- `par` has a dependency **type** ([§12](#12-dependency-type-table)) but **no** lifecycle mapping in
  this clone, so it is not a usable built-in packaging.
- Additional packaging values become legal when a build extension or a plugin with
  `<extensions>true</extensions>` contributes a `LifecycleMapping` — e.g. `bundle` (maven-bundle-plugin),
  `nbm`, `swf`, `takari-jar`. A POM using such a packaging is well-formed; `jv` should not reject an
  unknown packaging outright, but it cannot know the lifecycle. For resolution and
  `dependency:tree` purposes an unknown packaging behaves like `jar`.
- **Aggregator rule (validator, ERROR):** if `<modules>` or `<subprojects>` is non-empty, `packaging`
  **must** be `pom`; message: *"with value '…' is invalid. Aggregator projects require 'pom' as
  packaging."*
- `packaging` may not contain expressions (WARNING if it does).

---

## 4. `<dependency>`, `<exclusions>`

Class `Dependency`. XML: `<dependencies><dependency>…</dependency></dependencies>`. The same element
shape is used in four places: `project/dependencies`, `project/dependencyManagement/dependencies`,
`project/build/plugins/plugin/dependencies`, and the profile equivalents of the first two.

### 4.1 Fields

| XML element | Type | Cardinality | Default | Required | Since | Notes |
|---|---|---|---|---|---|---|
| `groupId` | String | 1 | — | **yes** (ERROR) | 3.0.0 | Coordinate id rules. |
| `artifactId` | String | 1 | — | **yes** (ERROR) | 3.0.0 | Coordinate id rules. |
| `version` | String | 0..1 | from `<dependencyManagement>` | **yes in the effective model** for `project/dependencies` and plugin dependencies (ERROR *"is missing"*); **optional** inside `<dependencyManagement>` | 3.0.0 | Version requirement, see [§3.2](#32-version-strings). |
| `type` | String | 0..1 | **`jar`** (schema default — materialise at parse time, in `<dependencyManagement>` too) | ERROR if empty in the effective model (non-management) | 4.0.0 | Maps to extension + classifier + path types, see [§12](#12-dependency-type-table). |
| `classifier` | String | 0..1 | none (null) | no | 4.0.0 | Appended after the version in the file name. An explicit `<classifier>` **overrides** the classifier implied by `type`. |
| `scope` | String | 0..1 | **`compile`**, injected by `DefaultModelNormalizer.injectDependency` — *not* an mdo default | no | 4.0.0 | See [§4.2](#42-scope). Deliberately absent from the schema so `<dependencyManagement>` can supply it. |
| `systemPath` | String | 0..1 | — | required **iff** `scope == system` | 4.0.0 | Must be an **absolute** path (ERROR otherwise); ERROR if present with any non-`system` scope; WARNING if it is a hard-coded path rather than an expression, if it contains `${basedir}`/`${project.basedir}`, or if the file does not exist. |
| `optional` | String (bool) | 0..1 | accessor default `false` | no | 4.0.0 | Optional dependencies are **not** propagated transitively; they still participate in version mediation when the artifact is reached another way. Validated as a boolean (WARNING/ERROR by level). |
| `exclusions` | list `<exclusions><exclusion>` | 0..n | empty | no | 4.0.0 | [§4.3](#43-exclusions)–[§4.4](#44-exclusion-matching-semantics). |

There is **no** `<optional>`-like `<import>` element and no `<scope>import</scope>` outside
`<dependencyManagement>` (ERROR: *"The 'import' scope is only valid in `<dependencyManagement>`
sections."*).

### 4.2 `scope`

Legal values for **Maven 3.9** (the `Maven3ScopeManagerConfiguration` dependency-scope universe):

| Scope | Transitive | On compile path | On runtime path | On test path | Notes |
|---|---|---|---|---|---|
| `compile` | yes | yes | yes | yes | **Default.** |
| `provided` | no | yes | no | yes (compile + runtime for tests) | Expected from the container/JDK. |
| `runtime` | yes | no | yes | yes | |
| `test` | no | no | no | yes | |
| `system` | no | yes | yes | yes | Requires `<systemPath>`; **deprecated** — WARNING at validation level ≥ `MAVEN_3_1`. |
| `import` | n/a | n/a | n/a | n/a | **Only** inside `<dependencyManagement>`, and only with `<type>pom</type>` (WARNING *"must be 'pom' to import the managed dependencies"* otherwise). Not a real dependency: it splices the target POM's `<dependencyManagement>` in place. |

Validation is intentionally lenient: an unrecognised scope is a **WARNING**, not an error, because
extensions historically used custom scopes (`merged`, `internal`, `external`, …). For
**plugin** dependencies the legal set is narrower and checked with a strict enum:
**`compile`, `runtime`, `system`** only.

**[M4]** Maven 4 adds `none`, `compile-only`, `test-only`, `test-runtime` to the scope universe.
`DefaultModelValidator` raises **ERROR** for `compile-only` / `test-only` / `test-runtime` under
modelVersion `4.0.0` (*"scope '…' is not supported with modelVersion 4.0.0"*). `jv` should treat
these four as unknown scopes.

### 4.3 `<exclusions>`

```xml
<dependency>
  …
  <exclusions>
    <exclusion>
      <groupId>org.example</groupId>
      <artifactId>unwanted</artifactId>
    </exclusion>
  </exclusions>
</dependency>
```

Class `Exclusion` (model version `4.0.0+`), exactly two fields:

| XML element | Type | Required | Default | Notes |
|---|---|---|---|---|
| `groupId` | String | **yes** (`<required>true</required>`; validator severity WARNING) | — | Wildcards allowed, see below. |
| `artifactId` | String | **yes** (`<required>true</required>`; validator severity WARNING) | — | Wildcards allowed, see below. |

There is **no** `classifier`, `type`, `version` or `scope` on `<exclusion>`. An exclusion therefore
matches on `groupId` + `artifactId` only, and removes the matched node **and its whole subtree** from
the dependency graph. Exclusions are inherited down the transitive path: an exclusion declared on a
direct dependency applies to that dependency's entire transitive closure, at any depth.

### 4.4 Exclusion matching semantics

Two matchers exist in Maven and they are **not** equivalent. Both must be understood.

1. **Transitive-resolution matcher (resolver `ExclusionDependencySelector`) — this is the one that
   shapes the dependency graph and therefore `dependency:tree`.** This class lives in
   **maven-resolver**, which is an external dependency of this clone (root `pom.xml`:
   `<resolverVersion>2.0.21</resolverVersion>`) and is *not* vendored here; the clone only wires it up
   in `impl/maven-impl/.../resolver/MavenSessionBuilderSupplier` (`new ExclusionDependencySelector()`).
   The rule stated below is the resolver contract, not a quote from a file in this tree. Each field is
   compared with *whole-field equality, plus the single special token* `*`:
   - `*` matches any value.
   - Anything else must be an exact, case-sensitive, full-string match.
   - `org.example.*` does **not** work here: it is compared literally.
   - When the POM model is converted to a resolver `Exclusion`, the `classifier` and `extension`
     components are set to `*`, so they never constrain the match.
   - Consequences: `<groupId>*</groupId><artifactId>*</artifactId>` excludes the entire subtree
     (the "exclude all transitives" idiom). A missing/empty field does **not** behave as `*` — it
     compares as the empty string and matches nothing, so both fields must be written.
2. **Glob matcher (`ExclusionArtifactFilter`, `impl/maven-core/.../filter/ExclusionArtifactFilter.java`).**
   Used when Maven builds the legacy `MavenProject` artifact set for plugins (i.e. the
   already-resolved artifact list of the current project), not during graph collection. It compiles
   each field as a **filesystem glob** (`FileSystems.getDefault().getPathMatcher("glob:" + pattern)`),
   so `org.example.*`, `*-test`, `commons-?` all work, and a dependency is excluded only when **both**
   the groupId glob and the artifactId glob match. The mdo's `Exclusion` description reflects this
   matcher ("interpreted as glob patterns", `@see java.nio.file.FileSystem#getPathMatcher`).

**Implementation guidance for `jv`.** For building the resolution graph and rendering
`dependency:tree`, implement matcher (1) — exact match with `*` as the only wildcard, per field,
independently. Optionally offer matcher (2) behind the same field syntax for parity with plugin-side
filtering; note that (2) is a superset of (1) for these two fields (a bare `*` is also a valid glob
matching any single-segment string), so a glob implementation is a safe superset **provided** you keep
the "both fields must match" conjunction and treat an empty pattern as matching nothing.

Validation of exclusion coordinates: at validation level < `MAVEN_3_0` the plain coordinate-id rule
is applied (WARNING); at ≥ `MAVEN_3_0` the wildcard-tolerant rule
(`validateCoordinatesIdWithWildcards`) is applied (WARNING). Either way a bad exclusion coordinate
never fails the build.

### 4.5 Dependency identity and de-duplication

- **Management key** (used to match a dependency against `<dependencyManagement>`, and to detect
  duplicate declarations): `groupId:artifactId:type[:classifier]` — the classifier segment is
  appended only when the classifier is non-null and non-empty. Note `type` is part of the key and is
  `jar` by default, so a managed `test-jar` entry does **not** manage the plain `jar` dependency.
- Duplicate declarations with the same management key are a violation at
  `errOn31` severity (WARNING at level 3.0, ERROR at 3.1+), with the message *"must be unique: …"*.
- A dependency whose `groupId:artifactId:version[:classifier]` equals the project's own coordinates
  is **FATAL** (*"is referencing itself"*).

---

## 5. `<dependencyManagement>`

Class `DependencyManagement`, model version `4.0.0+`. A single field:

| XML element | Type | Cardinality | Default | Inherited |
|---|---|---|---|---|
| `dependencies` | list `<dependencies><dependency>` | 0..n | empty | yes |

Full XML shape:

```xml
<dependencyManagement>
  <dependencies>
    <dependency>…</dependency>
  </dependencies>
</dependencyManagement>
```

Semantics:

- Entries are **not** resolved on their own. They supply defaults — `version`, `scope`, `optional`,
  `systemPath`, `exclusions`, `classifier` — to any dependency in this POM or a descendant POM whose
  **management key** ([§4.5](#45-dependency-identity-and-de-duplication)) matches. A value already
  present on the declaring dependency wins.
- `<version>` is **optional** inside `<dependencyManagement>` (the validator skips the
  "version is missing" check when `management == true`), and the `compile` scope default is **not**
  injected into management entries.
- Managed `<exclusions>` are *merged into*, not replaced by, the declaring dependency's exclusions.
- Duplicate management keys within one `<dependencyManagement>` are reported at `errOn31` severity.
- `<scope>import</scope>` with `<type>pom</type>`: the referenced POM's **effective**
  `<dependencyManagement>` is imported in place of the entry. `<classifier>` on an import is a
  violation at `errOn30` severity for modelVersion 4.0.0 (*"must be empty, imported POM cannot have
  a classifier"*). Imports are processed transitively; the importing POM's own entries take
  precedence over imported ones, and earlier imports over later ones.
- Inheritance: parent management entries are merged with the child's by management key; the child's
  values dominate. Because inheritance runs before the child's own management is applied,
  a child can override a single field of an inherited managed dependency.

---

## 6. `<properties>`

Declared on `ModelBase`, so it appears both at project level and inside a `<profile>`.

```xml
<properties>
  <my.prop>value</my.prop>
  <maven.compiler.release>17</maven.compiler.release>
</properties>
```

| XML element | Type | Cardinality | Default | Inherited | Since |
|---|---|---|---|---|---|
| `properties` | inline map, `<key>value</key>` | 0..1 | empty map | **yes**, key-wise | 4.0.0 |

Rules:

- Element **name is the property key**, text content is the value. Keys may contain dots and dashes;
  they are XML element names, so they must be valid NCNames (no leading digit, no spaces).
- Inheritance is a **key-wise merge** (`DefaultInheritanceAssembler.mergeModelBase_Properties`):
  parent entries are copied in first, then the child's overwrite on collision. The child does not
  need to redeclare inherited properties.
  - One key is **excluded** from inheritance: the "child directory" property (`CHILD_DIRECTORY_PROPERTY`)
    that Maven uses internally for child URL path adjustment. Everything else propagates. **[M4]**
    behaviour; harmless to ignore for Maven 3 parity.
- Profile injection (`DefaultProfileInjector`) merges an active profile's properties over the model's
  with **source (profile) dominant**.
- Precedence when interpolating `${…}`, strongest first: CLI user properties (`-D`), then system
  properties (JVM + `env.*` environment variables), then POM/profile properties, then the
  `project.*` / `pom.*` model-derived expressions and `settings.*`. Note that `-D` beats POM
  properties, which is what makes `-Dmy.prop=x` overrides work.

---

## 7. `<modules>` / `<subprojects>`

Both are declared on `ModelBase`, so both are legal inside a `<profile>` as well as at project level.

| XML element | Item element | Type | Cardinality | Default | Inherited | Since | Notes |
|---|---|---|---|---|---|---|---|
| `modules` | `<module>` | String (relative path) | 0..n | empty | **no** | `4.0.0/4.2.0`, `@Deprecated(since="4.0.0")` | Path to a directory containing the subproject, or directly to its POM file. |
| `subprojects` | `<subproject>` | String (relative path) | 0..n | empty | **no** | **[M4]** `4.1.0+` | Maven 4 rename of `modules`. |

```xml
<modules>
  <module>core</module>
  <module>../sibling</module>
</modules>
```

Rules:

- **Not inherited.** `MavenModelMerger.mergeModelBase_Modules` / `_Subprojects` only merge when
  `sourceDominant` is true, which is false in the parent → child direction. A child never receives its
  parent's module list.
- Aggregator constraint: a non-empty list forces `packaging` = `pom` (ERROR otherwise).
- A blank/whitespace-only entry is an ERROR (*"has been specified without a path to the project
  directory"*).
- Duplicate entries are an ERROR (*"specifies duplicate child module …"*).
- **[M4] parse rules `jv` must enforce for modelVersion 4.0.0:** `<subprojects>` present under
  modelVersion `4.0.0` is an **ERROR** (*"unexpected subprojects element"*). Under 4.1.0+, using
  `<modules>` is a WARNING (*"deprecated modules element, use subprojects instead"*) and using
  **both** is an ERROR (*"cannot use both modules and subprojects element"*). For a Maven 3.9 target,
  `<subprojects>` is simply not part of the schema.

---

## 8. `<build>`

Class hierarchy: `Build` extends `BuildBase` extends `PluginConfiguration` extends `PluginContainer`.
This layering is load-bearing: a `<profile><build>` is a **`BuildBase`**, so the `Build`-only fields
are illegal there ([§10.5](#105-what-a-profile-may-contain)).

### 8.1 `<build>` fields relevant to resolution

| XML element | Declared on | Type | Cardinality | Default | Inherited | Since | Notes |
|---|---|---|---|---|---|---|---|
| `plugins` | `PluginContainer` | list `<plugins><plugin>` | 0..n | empty | yes, per-plugin, honouring `inherited` | 4.0.0 | [§8.3](#83-plugin). |
| `pluginManagement` | `PluginConfiguration` | `PluginManagement` | 0..1 | — | yes | 4.0.0 | [§8.4](#84-pluginmanagement). |
| `extensions` | `Build` **only** | list `<extensions><extension>` | 0..n | empty | yes | 4.0.0 | [§8.2](#82-extension). Not legal in a profile. |
| `defaultGoal` | `BuildBase` | String | 0..1 | — | yes | 3.0.0 | Whitespace-separated goals/phases. |
| `directory` | `BuildBase` | String | 0..1 | `target` (Super POM) | yes | 4.0.0 | |
| `finalName` | `BuildBase` | String | 0..1 | `${artifactId}-${version}` | yes | 4.0.0 | |
| `filters` | `BuildBase` | list `<filters><filter>` | 0..n | empty | yes (parent-first concat) | 4.0.0 | |
| `resources` / `testResources` | `BuildBase` | list `<resources><resource>` / `<testResources><testResource>` | 0..n | `src/main/resources` / `src/test/resources` | yes, all-or-nothing | 3.0.0/4.0.0, `@Deprecated(since="4.0.0")` | Not resolution-relevant. |
| `sourceDirectory`, `scriptSourceDirectory`, `testSourceDirectory`, `outputDirectory`, `testOutputDirectory` | `Build` only | String | 0..1 each | `src/main/java`, `src/main/scripts`, `src/test/java`, `target/classes`, `target/test-classes` | yes | 3.0.0/4.0.0; first four `@Deprecated(since="4.0.0")` | Not resolution-relevant. Not legal in a profile. |
| `sources` | `Build` only | list `<sources><source>` | 0..n | — | yes | **[M4]** 4.1.0 | Replaces the deprecated source/resource directory fields. Reject under 4.0.0. |

### 8.2 `<extension>`

```xml
<build><extensions><extension>…</extension></extensions></build>
```

Class `Extension`, model version `4.0.0+`. Build extensions are resolved as artifacts (type `jar`)
from `<pluginRepositories>` **before** the rest of the build runs, and can contribute lifecycle
mappings, artifact/dependency types, and wagon providers — so they can change the meaning of
`packaging` and `type` for the project.

| XML element | Type | Required | Default | Since | Notes |
|---|---|---|---|---|---|
| `groupId` | String | **yes** | — | 4.0.0 | No default (unlike `<plugin>`). |
| `artifactId` | String | **yes** | — | 4.0.0 | |
| `version` | String | no | resolved from `<pluginManagement>` / latest | 4.0.0 | |
| `configuration` | DOM | no | — | **[M4]** 4.1.0 | Opaque XML. Reject/ignore under 4.0.0. |

Equality (used for de-duplication) is over `groupId` + `artifactId` + `version`.

### 8.3 `<plugin>`

Class `Plugin` extends `ConfigurationContainer`, model version `4.0.0+`. XML:
`<build><plugins><plugin>` and `<build><pluginManagement><plugins><plugin>`.

Only the fields that affect resolving the plugin and its dependencies are specified here.

| XML element | Declared on | Type | Cardinality | Default | Inherited | Since | Notes |
|---|---|---|---|---|---|---|---|
| `groupId` | `Plugin` | String | 0..1 | **`org.apache.maven.plugins`** (schema default) | yes | 4.0.0 | Validator: FATAL if null or blank — i.e. `<groupId></groupId>` is fatal, but a wholly absent element receives the default. |
| `artifactId` | `Plugin` | String | 1 | — | yes | 4.0.0 | FATAL if null or blank. |
| `version` | `Plugin` | String | 0..1 | from `<pluginManagement>`, else the packaging's lifecycle-default version, else "latest" resolution (WARNING) | yes | 4.0.0 | FATAL if present but blank (`<version/>`). |
| `extensions` | `Plugin` | String (bool) | 0..1 | accessor default `false` | yes | 4.0.0 | `true` ⇒ load this plugin's Maven extensions (packaging and type handlers) into the build. Affects which `packaging`/`type` values are legal. |
| `dependencies` | `Plugin` | list `<dependencies><dependency>` | 0..n | empty | yes | 4.0.0 | Additional artifacts injected into the **plugin's** classloader — resolved from `<pluginRepositories>`, a separate graph from the project's dependencies. `<version>` is required (`errOn30`); `<scope>` restricted to the strict enum `compile` / `runtime` / `system`. |
| `inherited` | `ConfigurationContainer` | String (bool) | 0..1 | accessor default `true` | — | 4.0.0 | `false` ⇒ this plugin declaration is not propagated to child POMs. See [§8.3.1](#831-inherited-and-plugin-merging). |
| `configuration` | `ConfigurationContainer` | DOM | 0..1 | — | yes, unless `inherited=false` | 4.0.0 | Opaque, see [§8.5](#85-configuration-is-opaque). |
| `executions` | `Plugin` | list `<executions><execution>` | 0..n | empty | yes (merged by execution `id`) | 4.0.0 | `PluginExecution`: `id` (default **`default`**), `phase`, `goals` (`<goals><goal>`), `inherited`, `configuration`. Duplicate execution `id` is an ERROR. |
| `goals` | `Plugin` | DOM | 0..1 | — | n/a | 4.0.0 only, `@Deprecated` | Unused by Maven. Parse-and-drop. |

Plugin identity: `getKey()` = `groupId:artifactId` (version excluded). `getId()` =
`groupId:artifactId:version` with `[unknown-group-id]` / `[unknown-artifact-id]` /
`[unknown-version]` placeholders. Duplicate plugin keys within one `<plugins>` are reported at
`errOn31` severity.

#### 8.3.1 `inherited` and plugin merging

From `DefaultInheritanceAssembler.mergePluginContainer_Plugins` / `mergePlugin`:

- A parent plugin is offered to the child only if `element.isInherited()` **or** it has at least one
  `<execution>` (executions carry their own `inherited` flag, so they must be walked).
- `mergePlugin` merges the `ConfigurationContainer` part (`inherited` + `configuration`) **only when
  the source plugin is inherited**; `groupId`/`artifactId`/`version`/`extensions`/`executions`/
  `dependencies` are always merged.
- Ordering: parent (`source`) plugins keep their relative order and form the "master" sequence;
  child-only plugins are inserted before the next matching master entry, with any remainder appended.

### 8.4 `<pluginManagement>`

Class `PluginManagement` extends `PluginContainer` — it adds nothing, so its content is exactly
`<plugins><plugin>…`:

```xml
<build>
  <pluginManagement>
    <plugins>
      <plugin>…</plugin>
    </plugins>
  </pluginManagement>
</build>
```

Semantics: supplies defaults (notably `<version>`, `<configuration>`, `<dependencies>`) to any
`<build><plugins><plugin>` with the same `groupId:artifactId` in this POM or a descendant. It does
**not** by itself cause the plugin to run or to be resolved. Local declarations override the managed
definition. `<pluginManagement>` is declared on `PluginConfiguration`, which `BuildBase` extends,
so it is legal inside a profile's `<build>`.

### 8.5 `<configuration>` is opaque

`<configuration>` (on `<plugin>`, `<execution>`, `<reportPlugin>`, `<reportSet>`, and **[M4]**
`<extension>`) is `<type>DOM</type>`: an arbitrary XML subtree. `jv` must **preserve it verbatim** —
element names, order, text, and attributes — because plugin parameter binding depends on all of them.

Behaviours to preserve, even if `jv` never interprets the contents:

- Element text is **trimmed** by default; `xml:space="preserve"` on an element keeps its whitespace
  (since Maven 3.1.0).
- Two merge-control attributes exist on children of `<configuration>`:
  - `combine.children` — `merge` (default) or `append`.
  - `combine.self` — `merge` (default) or `override`. **[M4]** additionally accepts `remove`.
  Any other value is an ERROR at validation level `MAVEN_4_0`.
- Interpolation of `${…}` applies inside configuration values.

---

## 9. `<repositories>` and `<pluginRepositories>`

Both are `List<Repository>` on `ModelBase`, so both are legal at project level and inside a
`<profile>`. Item element names come from the **field** name, not the class name:

```xml
<repositories>
  <repository>…</repository>
</repositories>
<pluginRepositories>
  <pluginRepository>…</pluginRepository>
</pluginRepositories>
```

| Wrapper | Item | Purpose | Inherited |
|---|---|---|---|
| `repositories` | `<repository>` | Where to look for **dependencies** and build extensions. | yes, merged by `id` |
| `pluginRepositories` | `<pluginRepository>` | Where to look for **plugins** and their dependencies. | yes, merged by `id` |

### 9.1 `<repository>` fields

`Repository` extends `RepositoryBase`. Model version `4.0.0+` throughout.

| XML element | Type | Cardinality | Default | Required | Notes |
|---|---|---|---|---|---|
| `id` | String | 1 | — | **yes** (`<required>true</required>`, `<identifier>true</identifier>`) | The merge/override key: matches `<server>`/`<mirror>` entries in `settings.xml`, and is the key for inheritance and profile injection. Must not contain `\ / : " < > \| ? *` (severity `errOn31`). `id` = `local` is a violation at `errOn31` (*"this identifier is reserved for the local repository"*). |
| `name` | String | 0..1 | — | no | Human-readable label. |
| `url` | String | 1 | — | **yes** (ERROR if empty) | Form `protocol://hostname/path`. An **uninterpolated** `${…}` left in the url is a WARNING and **the repository is skipped** (*"contains an uninterpolated expression; the repository will be skipped"*) — Version.V40 message; in Maven 3.9 an uninterpolated url simply fails at transport time. |
| `layout` | String | 0..1 | **`default`** (schema default) | no | Only `default` is supported. `legacy` (the Maven 1 layout) parses but produces a WARNING: *"uses the unsupported value 'legacy', artifact resolution might fail."* |
| `releases` | `RepositoryPolicy` | 0..1 | see [§9.2](#92-releases--snapshots-policy) | no | Policy for non-`-SNAPSHOT` versions. |
| `snapshots` | `RepositoryPolicy` | 0..1 | see [§9.2](#92-releases--snapshots-policy) | no | Policy for `-SNAPSHOT` versions. |

`DeploymentRepository` (used only by `<distributionManagement>`) extends `Repository` and adds
`uniqueVersion` (`boolean`, schema default **`true`**).

### 9.2 `<releases>` / `<snapshots>` policy

Class `RepositoryPolicy`, model version `4.0.0+`. Three fields, all `String`:

| XML element | Type | Cardinality | Default when the field is absent | Default when the whole `<releases>`/`<snapshots>` element is absent | Notes |
|---|---|---|---|---|---|
| `enabled` | String (bool) | 0..1 | **`true`** (`isEnabled()` returns `true` for null) | `true` | `false` ⇒ do not consult this repository for that version class at all. Parsed with `Boolean.parseBoolean`. |
| `updatePolicy` | String | 0..1 | **`daily`** | `daily` | Legal values: `always`, `daily`, `interval:XXX` (XXX = minutes), `never` (use only what is already in the local repository). Governs re-checking of `maven-metadata.xml` and of snapshot artifacts. |
| `checksumPolicy` | String | 0..1 | **Maven 3.9: `warn`**; **[M4]** `fail` | same | Legal values: `ignore`, `warn`, `fail`. What to do when a downloaded artifact's checksum does not verify. |

Exact source of the defaults (both files agree on `enabled`/`updatePolicy`, and bracket the
`checksumPolicy` divergence):

- `impl/maven-impl/.../resolver/ArtifactDescriptorUtils.toRepositoryPolicy` — used when converting a
  **dependency POM's** `<repositories>` during transitive resolution: `enabled = true`,
  `updates = UPDATE_POLICY_DAILY`, `checksums = CHECKSUM_POLICY_WARN` with the trailing comment
  `// the default`.
- `impl/maven-impl/.../DefaultRepositoryFactory.buildRepositoryPolicy` — Maven 4's session-level
  path: `enabled = true`, `updatePolicy = UPDATE_POLICY_DAILY`,
  `checksumPolicy = CHECKSUM_POLICY_FAIL`.
- The mdo description states it plainly: `fail` "(default for Maven 4 and above)", `warn`
  "(default for Maven 3)".

**`jv` targets Maven 3.9 ⇒ default `checksumPolicy` is `warn`.**

Common shape:

```xml
<repository>
  <id>central</id>
  <url>https://repo.maven.apache.org/maven2</url>
  <releases><enabled>true</enabled><updatePolicy>daily</updatePolicy><checksumPolicy>warn</checksumPolicy></releases>
  <snapshots><enabled>false</enabled></snapshots>
</repository>
```

### 9.3 Merging and precedence

- Inheritance and profile injection merge repository lists **by `id`**
  (`MavenModelMerger.mergeModelBase_Repositories` / `_PluginRepositories`; the key computer is
  `RepositoryBase::getId`). The merge is **whole-element, not field-wise**: a child or profile
  repository with the same `id` as an inherited one **replaces** it entirely — this is how projects
  override `central`. Do not merge policies field by field.
- **Ordering** (matters, because it is the order repositories are contacted): the dominant list is
  emitted first in its declared order, then the recessive list's non-colliding entries. In parent →
  child inheritance the **child** is dominant, so the child's own repositories precede the inherited
  ones. In profile injection the **profile** is dominant, so profile repositories precede the model's.
- The implicit Super POM repository `central`
  (`https://repo.maven.apache.org/maven2`, `releases` enabled, `snapshots` disabled) is present
  unless overridden by `id` = `central`.
- `settings.xml` `<mirror>` entries rewrite the effective URL by `<mirrorOf>` matching after the POM
  is assembled; `<mirrorOf>*</mirrorOf>` matches everything. Mirrors are outside the POM model but
  determine which URL is actually contacted.
- Validation runs over both project-level and profile-level lists; for profile repositories the
  expression check is skipped (`skipExpressionCheck = true`) because they are validated before
  interpolation.

---

## 10. `<profiles>` and `<activation>`

```xml
<profiles>
  <profile>
    <id>…</id>
    <activation>…</activation>
    <!-- ModelBase content + <build> (BuildBase) -->
  </profile>
</profiles>
```

### 10.1 `<profile>` fields

Class `Profile` extends **`ModelBase`**, model version `4.0.0+`.

| XML element | Type | Cardinality | Default | Notes |
|---|---|---|---|---|
| `id` | String | 0..1 | **`default`** (schema default) | Used for `-P` activation and for merging profiles during inheritance. Must be unique per POM (violation at `errOn30`: *"must be unique but found duplicate profile with id …"*). Validated by `validateProfileId` (ERROR, Version.V40). |
| `activation` | `Activation` | 0..1 | — | [§10.2](#102-activation-activators). Absent ⇒ the profile can only be activated explicitly by `-P`. |
| `build` | `BuildBase` (`xml.tagName="build"`) | 0..1 | — | **`BuildBase`, not `Build`** — see [§10.5](#105-what-a-profile-may-contain). |
| *(inherited from `ModelBase`)* | | | | `modules`, `subprojects` **[M4]**, `distributionManagement`, `properties`, `dependencyManagement`, `dependencies`, `repositories`, `pluginRepositories`, `reporting`, `reports` (deprecated). |

A profile also carries a non-XML `source` marker, `pom` (`Profile.SOURCE_POM`) or `settings.xml`
(`Profile.SOURCE_SETTINGS`). It is not parseable from the POM but it changes activation semantics
([§10.4](#104-activation-evaluation-and-activebydefault)).

### 10.2 `<activation>` activators

Class `Activation`, model version `4.0.0+`. **Seven** activator fields; `jv` implements the first
five (the last two are Maven 4 only):

| XML element | Type | Cardinality | Default | Since | Activator implementation |
|---|---|---|---|---|---|
| `activeByDefault` | boolean | 0..1 | `false` | 4.0.0 | Handled by `DefaultProfileSelector`, **not** by a `ProfileActivator`. |
| `jdk` | String | 0..1 | — | 4.0.0 | `JdkVersionProfileActivator` (`@Named("jdk-version")`) |
| `os` | `ActivationOS` | 0..1 | — | 4.0.0 | `OperatingSystemProfileActivator` (`@Named("os")`) |
| `property` | `ActivationProperty` | 0..1 | — | 4.0.0 | `PropertyProfileActivator` (`@Named("property")`) |
| `file` | `ActivationFile` | 0..1 | — | 4.0.0 | `FileProfileActivator` (`@Named("file")`) |
| `packaging` | String | 0..1 | — | **[M4]** 4.1.0 | `PackagingProfileActivator` (`@Named("packaging")`) |
| `condition` | String | 0..1 | — | **[M4]** 4.1.0 | `ConditionProfileActivator` |

A commented-out `custom` / `ActivationCustom` (DOM `configuration` + `type`) exists in the mdo but is
**not** part of the model. Do not implement it.

#### 10.2.1 `<jdk>` — exact matching rules

Source: `JdkVersionProfileActivator.isJavaVersionCompatible(requiredJdkRange, currentJavaVersion)`,
where `currentJavaVersion` is the **system property `java.version`**.

Precedence, in order:

1. **Negation.** If the pattern starts with `!`:
   `active = !currentJavaVersion.startsWith(pattern.substring(1))`.
   So `!1.4` is true for every JDK whose `java.version` does not begin with `1.4`. Note this is
   negated *prefix* matching, not negated range matching — `![1.5,)` is treated as
   `!"[1.5,)"` as a prefix, which never matches, so the profile is always active. Do not "fix" this.
2. **Range.** Otherwise, if `isRange(pattern)` — which is **exactly**
   `pattern.startsWith("[") || pattern.startsWith("(")`; there is *no* check that the pattern ends
   with `]` or `)` — the current version is compared against the range bounds.
3. **Prefix.** Otherwise: `active = currentJavaVersion.startsWith(pattern)`. `1.4` matches
   `1.4.2_08`; prefix matching is on the whole string, so `11` does not match `1.1`, but `1` **does**
   match both `1.8.0` and `17` (a real gotcha).

Range parsing (`getRange`), exactly:
- The pattern is split on `,`. For each token, in this order: `startsWith("[")` → inclusive bound with
  `[` removed; else `startsWith("(")` → exclusive bound with `(` removed; else `endsWith("]")` →
  inclusive bound with `]` removed; else `endsWith(")")` → exclusive bound with `)` removed; else if
  the token is empty → an **exclusive** bound with the empty value. A token matching none of these
  (e.g. a bare `1.5`) is **silently dropped**.
- Bound values are `trim()`ed.
- If fewer than two bounds were produced, an **exclusive upper bound of `99999999`** is appended.
- Consequence: the single-value form `[1.5]` parses as one bound `"1.5]"` (the `]` is *not* stripped,
  because the `startsWith("[")` branch wins and only replaces `[`) plus the synthetic `99999999`
  upper bound. `Integer.parseInt("5]")` then throws, so `[1.5]` always yields a WARNING and an
  inactive profile. Do not implement it as an exact-version range.

Range comparison (`isInRange` / `getRelationOrder`) — a custom numeric comparison, **not** Maven's
generic version comparator:
- The current version is filtered by removing every character that is not a digit, `.`, `_`, or `-`
  (`[^\d._-]` → `""`), then split on `[._-]`.
- The bound value is split on `.` only.
- Both token lists are zero-padded to **three** components, and only the first three are compared,
  numerically via `Integer.parseInt`. A non-numeric token throws `NumberFormatException`, which the
  activator catches and reports as a WARNING with the profile inactive.
- An **empty** bound value short-circuits: it returns `1` for the lower bound and `-1` for the upper
  bound, i.e. unbounded. `[1.5,)` = 1.5 or above, `(,1.8]` = up to and including 1.8.
- When all three components are equal, an **exclusive** bound returns `-1` for the lower bound and
  `1` for the upper bound (so equality fails an exclusive bound); an inclusive bound returns `0`.
- **Quirk to reproduce:** `isInRange` returns `true` immediately when the lower-bound comparison is
  `0` — i.e. a version exactly equal to an inclusive lower bound is in range **without the upper
  bound being consulted at all**. A negative lower-bound comparison returns false; otherwise the
  upper bound must compare `<= 0`.
- Consequence of the three-component truncation: `1.8.0_292` compares as `1.8.0`, so
  `[1.8.0_100,)` and `[1.8.0_300,)` behave identically.

If `java.version` is null or empty the activator emits an ERROR (*"Failed to determine Java version
for profile …"*) and the profile is inactive.

#### 10.2.2 `<os>` — exact matching rules

Class `ActivationOS`, model version `4.0.0+`. All four fields are `String`, all optional, but
**at least one must be non-null** or the activator returns false (`ensureAtLeastOneNonNull`).

| XML element | Compared against | Matching |
|---|---|---|
| `name` | `os.name`, lower-cased | Exact equality, `!` negation. |
| `family` | `os.name`, lower-cased | Family predicate (below), `!` negation. |
| `arch` | `os.arch`, lower-cased | Exact equality, `!` negation. |
| `version` | `os.version`, lower-cased | Exact equality, `!` negation, **or** `regex:` prefix. |

Evaluation (`OperatingSystemProfileActivator`):

- The three actual values are read from the **system properties** `os.name`, `os.arch`, `os.version`
  and lower-cased with `Locale.ENGLISH`. If a system property is missing, the activator falls back to
  the JVM-captured `Os.OS_NAME` / `Os.OS_ARCH` / `Os.OS_VERSION` (already lower-cased).
- The declared `name`, `arch` and `version` values are lower-cased with `Locale.ENGLISH` before
  comparison. **`family` is the exception:** `determineFamilyMatch` passes it through *without*
  lower-casing, and `Os.isFamily` switches on the raw string — so the dedicated family cases below
  only fire for a lower-case declaration (see the default-branch fallback).
- All present sub-elements are ANDed, evaluated in the order **family, name, arch, version**, with
  short-circuiting.
- **Negation:** a leading `!` on `name`, `family`, `arch`, or `version` inverts that single
  comparison. Implementation is `reverse != result`, i.e. an exclusive-or, applied per field.
- **`regex:` prefix — `version` only.** If the (lower-cased) declared version starts with `regex:`,
  the remainder is used as a Java regular expression and matched with
  `actualVersion.matches(...)` (a **full-string** match, not a search). `!` negation is **not**
  combined with `regex:` — the `regex:` branch is checked first and ignores any `!`, so
  `!regex:…` is treated as a literal equality test against the string `!regex:…`.
- No wildcard or glob support on any OS field.

**Family values** recognised by `Os.isFamily(family, actualOsName)`:

| Family | True when |
|---|---|
| `windows` | `os.name` contains `windows` |
| `win9x` | Windows **and** the name contains one of the substrings `95`, `98`, `me`, or `ce` (note: bare `ce`, not `windows ce`) |
| `winnt` | Windows and **not** `win9x` |
| `dos` | the `path.separator` system property is `;`, and not netware, and not windows |
| `mac` | name contains `mac` or `darwin` |
| `unix` | `path.separator` is `:`, not openvms, and (not mac **or** name ends with `x` — i.e. `mac os x` counts as unix) |
| `os/2` | name contains `os/2` |
| `netware` | name contains `netware` |
| `tandem` | name contains `nonstop_kernel` |
| `openvms` | name contains `openvms` |
| `z/os` | name contains `z/os` **or** `os/390` |
| `os/390` | (folded into `z/os`) |
| `os/400` | name contains `os/400` |
| `unknown` | *not* a switch case — falls into the default branch below |

**Default branch (important):** any family string that is not one of the cases above is **not**
false — `Os.isFamily` falls through to `actualOsName.contains(family.toLowerCase(Locale.US))`, a plain
substring test. Two consequences:

- `<family>Windows</family>` (capitalised) misses the `windows` case but still matches on Windows via
  the substring fallback, so mixed-case family names usually appear to work.
- Arbitrary strings behave as substring matches: `<family>linux</family>` matches `os.name` =
  `linux`, and `<family>unknown</family>` matches only an `os.name` literally containing `unknown`.

`jv` must reproduce the fallback, not return false.

#### 10.2.3 `<property>` — exact matching rules

Class `ActivationProperty`, model version `4.0.0+`.

| XML element | Type | Required | Notes |
|---|---|---|---|
| `name` | String | **yes** (`<required>true</required>`; ERROR *"The property name is required to activate the profile …"* if null/empty after stripping `!`) | May be prefixed with `!` for existence negation. |
| `value` | String | no | May be prefixed with `!` for value negation. |

Algorithm (`PropertyProfileActivator.isActive`), precisely:

1. Let `name` = `property.name`. If it starts with `!`, set `reverseName = true` and strip the `!`.
2. If the resulting `name` is null or empty → ERROR, inactive.
3. Look the value up, first hit wins:
   1. **user properties** (CLI `-D`);
   2. **[M4] only:** if `name` is exactly `packaging`, the model's packaging;
   3. **system properties** (JVM system properties, including environment variables exposed as
      `env.NAME` — on Windows, `env.` lookups are case-insensitive).
4. Let `propValue` = `property.value`.
   - **If `propValue` is non-null and non-empty** (value test): if it starts with `!`, set
     `reverseValue = true` and strip the `!`. Result = `reverseValue != propValue.equals(sysValue)`.
     Note `equals` on the declared value, so a null `sysValue` never equals a non-null pattern:
     `<value>x</value>` with the property unset is **false**, and `<value>!x</value>` with the
     property unset is **true**.
   - **Else** (existence test): result = `reverseName != (sysValue != null && !sysValue.isEmpty())`.
     An empty-string property value counts as *absent* for the existence test.
5. `reverseName` is used **only** in the existence branch. `<name>!foo</name><value>bar</value>`
   strips the `!` for lookup and then does a plain value comparison — the `!` on the name is
   silently ignored. This is worth replicating exactly.

Idioms:

| Declaration | Meaning |
|---|---|
| `<name>debug</name>` | property `debug` is set to a non-empty value |
| `<name>!debug</name>` | property `debug` is unset or empty |
| `<name>env</name><value>prod</value>` | `env == "prod"` |
| `<name>env</name><value>!prod</value>` | `env != "prod"` (**including** `env` unset) |
| `<name>env.CI</name>` | environment variable `CI` is set |

**[M4] note:** the `packaging` special case in step 3.2 means that in Maven 4 a
`<property><name>packaging</name><value>war</value></property>` activator matches the project's
packaging. In Maven 3.9 it only looks at user/system properties. Maven 4 also has a dedicated
`<packaging>` activator ([§10.2.5](#1025-packaging-m4)).

#### 10.2.4 `<file>` — exact matching rules

Class `ActivationFile`, model version `4.0.0+`.

| XML element | Type | Notes |
|---|---|---|
| `exists` | String | Path that must **exist** for the profile to activate. |
| `missing` | String | Path that must **not exist** for the profile to activate. |

Rules (`FileProfileActivator.isActive`):

- Both fields empty/absent → inactive.
- If **both** are given: `exists` wins and `missing` is **ignored**, with a WARNING
  (*"file activation conflict: Both 'missing' (…) and 'exists' assertions are defined. The 'missing'
  assertion will be ignored."*). The mdo also states they must not be combined.
- The result is `missing != fileExists`, i.e. `exists` activates when the path is present,
  `missing` activates when it is absent.
- **No glob support.** `FileProfileActivator` calls `context.exists(path, /* enableGlob */ false)`, so
  `*` and `?` in `<exists>`/`<missing>` are literal path characters. Globs are available only through
  the **[M4]** `exists()` / `missing()` *condition* functions ([§10.2.6](#1026-condition-m4)).
- **Path interpolation and alignment** (`DefaultProfileActivationContext.interpolatePath`), in
  lookup order: `${basedir}` **and** `${project.basedir}` (both spellings, both resolving to the
  model base directory), `${project.rootDirectory}` (**[M4]**), then POM/model properties, then user
  properties (`-D`), then system properties. The interpolated result is then aligned to the project
  directory, so a **relative path is resolved against the project basedir**.
- The mdo documents this more narrowly ("limited to `${project.basedir}`, system properties and user
  properties"), and `DefaultModelValidator.validate30RawProfileActivation` warns
  (*"expressions are not supported during profile activation"*) for any `${project.*}` expression
  **other than** `${project.basedir}` under `activation.file.*`. The warning is the real constraint:
  `${project.version}` and friends do not work, whereas `${basedir}` and plain POM properties do.
- Existence-check failures (I/O errors) produce an ERROR and an inactive profile.

#### 10.2.5 `<packaging>` **[M4]**

`Activation.packaging`, String, model version `4.1.0+`. `PackagingProfileActivator` compares it with
**`Objects.equals`** against the model's packaging — exact match, no negation, no wildcards. Not
available in Maven 3.9; reject or ignore under modelVersion 4.0.0.

#### 10.2.6 `<condition>` **[M4]**

`Activation.condition`, String, model version `4.1.0+`. A whole expression language evaluated by
`ConditionProfileActivator` / `ConditionParser` / `ConditionFunctions`. Not available in Maven 3.9.
Recorded here for completeness so `jv` can reject it with a clear message:

- **Property access:** `${property.name}`.
- **Comparisons:** `==`, `!=`, `<`, `>`, `<=`, `>=`.
- **Logical:** `&&`, `||`, `not(...)`.
- **Functions:** `length(string)`, `upper(string)`, `lower(string)`,
  `substring(string, start[, end])`, `indexOf(string, substring)`, `contains(string, substring)`,
  `matches(string, regex)`, `not(condition)`, `if(condition, trueValue, falseValue)`,
  `exists(glob)`, `missing(glob)`, `inrange(version, range)`, and `executable(...)`.
- **Available properties:** `project.basedir`, `project.rootDirectory`, `project.artifactId`,
  `project.packaging`, user properties, and system properties (environment variables as `env.*`).
- Only `${project.basedir}` is allowed among `${project.*}` expressions inside `condition`
  (same restriction as `<file>`); anything else is a WARNING.
- Using `executable(` produces a WARNING about non-reproducible POMs.

### 10.3 Interaction of multiple activators

`DefaultProfileSelector.isActive`:

```
isActive = false
for each activator:
    if activator.presentInConfig(profile):
        isActive = true
        if !activator.isActive(profile):
            return false        # any present-and-failing activator vetoes
return isActive                 # true only if at least one activator was present
```

So: **all present activators must pass (AND), and at least one must be present.** An empty
`<activation/>` element activates nothing. A `RuntimeException` from any activator is reported as an
ERROR and makes the profile inactive.

Within a single activator, the sub-fields are also ANDed (notably `<os>`; see
[§10.2.2](#1022-os--exact-matching-rules)).

### 10.4 Activation evaluation and `activeByDefault`

`DefaultProfileSelector.getActiveProfiles`, in order:

1. Profiles explicitly **deactivated** (`-P !id` / `-P -id`) are skipped unconditionally — explicit
   deactivation beats everything.
2. A profile is active if explicitly activated (`-P id`) **or** if `isActive(...)`
   ([§10.3](#103-interaction-of-multiple-activators)) returns true. If such a profile's `source` is
   `pom`, a flag `activatedPomProfileNotByDefault` is set.
3. Otherwise, if `activation/activeByDefault` is `true`:
   - `source == pom` → the profile is held in a **pending** list;
   - `source == settings.xml` → the profile is activated immediately (settings profiles are not
     subject to the suppression rule).
4. After the loop, the pending `activeByDefault` POM profiles are added **only if**
   `activatedPomProfileNotByDefault` is false.

The practical rule: **any** other POM profile becoming active by any means (`-P` or an activator)
suppresses **all** `activeByDefault` POM profiles in that POM. `activeByDefault` profiles in
`settings.xml` are unaffected by POM profile activation.

Activation is evaluated **per POM in the inheritance chain**, against that POM's own profiles, and
the resulting effects are injected into that model before it is inherited from.

### 10.5 What a profile may contain

A profile may contain (from `Profile` = `ModelBase` + `id` + `activation` + `build`):

| Legal inside `<profile>` | Legal inside `<profile><build>` (`BuildBase`) |
|---|---|
| `id`, `activation` | `defaultGoal` |
| `properties` | `directory` |
| `dependencies` | `finalName` |
| `dependencyManagement` | `filters` |
| `repositories` | `resources`, `testResources` (deprecated) |
| `pluginRepositories` | `plugins` |
| `distributionManagement` | `pluginManagement` |
| `modules` (and **[M4]** `subprojects`) | |
| `reporting`, `reports` (deprecated) | |
| `build` (a `BuildBase`) | |

**Illegal inside `<profile>`** (project-level only): `modelVersion`, `parent`, `groupId`,
`artifactId`, `version`, `packaging`, `name`, `description`, `url`, `inceptionYear`, `organization`,
`licenses`, `developers`, `contributors`, `mailingLists`, `prerequisites`, `scm`, `issueManagement`,
`ciManagement`, `profiles` (no nesting), **[M4]** `mixins`.

**Illegal inside `<profile><build>`** (they live on `Build`, not `BuildBase`): `extensions`,
`sourceDirectory`, `scriptSourceDirectory`, `testSourceDirectory`, `outputDirectory`,
`testOutputDirectory`, **[M4]** `sources`. A profile therefore **cannot** contribute build
extensions.

### 10.6 Profile injection

`DefaultProfileInjector.doInjectProfiles` applies active profiles **in declaration order**, each with
**source (profile) dominant**:

1. `merger.mergeModelBase(builder, model, profile)` — merges the `ModelBase` half (properties,
   dependencies, dependencyManagement, repositories, pluginRepositories, modules, reporting,
   distributionManagement).
2. If `profile.build != null`, merge `profile.build` into `model.build` (creating an empty `Build` if
   the model has none), again source-dominant, with plugins merged by `groupId:artifactId` and
   executions by `id`.

Profiles are injected **before** parent inheritance is applied to the child model, so an active
parent profile's effects are inherited normally while the `<profile>` element itself is not.

### 10.7 Raw-model validation of activation

`DefaultModelValidator.validate30RawProfileActivation` walks the whole `<activation>` tree and, for
every string value, warns (`Version.V30`) if it contains a `${project.*}` expression — with two
exemptions: `${project.basedir}` under `activation.file.*`, and `${project.basedir}` in
`activation.condition`. Everything else in an activation is evaluated **before** model
interpolation, so `${project.version}` and friends simply do not work there.

---

## 11. `<distributionManagement>`

Class `DistributionManagement`, model version `4.0.0+`.

| XML element | Type | Cardinality | Default | Inherited | Notes |
|---|---|---|---|---|---|
| `repository` | `DeploymentRepository` | 0..1 | — | yes (with child-path adjustment) | Deploy target for releases. Not read during resolution. |
| `snapshotRepository` | `DeploymentRepository` | 0..1 | falls back to `<repository>` | yes | Deploy target for snapshots. |
| `site` | `Site` | 0..1 | — | yes (with url path append) | Site deployment. Irrelevant to resolution. |
| `downloadUrl` | String | 0..1 | falls back to project `<url>` | yes | Informational pointer for artifacts absent from repositories for licensing reasons. |
| `relocation` | `Relocation` | 0..1 | — | **never** (`mergeDistributionManagement_Relocation` is an empty method) | [§11.1](#111-relocation). |
| `status` | String | 0..1 | `none` | n/a | [§11.2](#112-status). |

`<repository>`/`<snapshotRepository>` are `DeploymentRepository` = `Repository`
([§9.1](#91-repository-fields)) + `uniqueVersion` (boolean, default `true`).

### 11.1 `<relocation>`

Class `Relocation`, model version `4.0.0+`. This is the one part of
`<distributionManagement>` that **changes resolution**, so it matters for `dependency:tree`.

```xml
<distributionManagement>
  <relocation>
    <groupId>org.new</groupId>
    <artifactId>new-artifact</artifactId>
    <version>2.0</version>
    <message>Renamed for clarity.</message>
  </relocation>
</distributionManagement>
```

| XML element | Type | Cardinality | Default | Notes |
|---|---|---|---|---|
| `groupId` | String | 0..1 | **the original groupId** | New group id. |
| `artifactId` | String | 0..1 | **the original artifactId** | New artifact id. |
| `version` | String | 0..1 | **the original version** | New version. |
| `message` | String | 0..1 | — | Human-readable reason, surfaced in the build log. |

Semantics (mdo: *"If any of the values are omitted, it is assumed to be the same as it was before"*;
implementation: `impl/maven-impl/.../resolver/relocation/DistributionManagementArtifactRelocationSource`):

- When a POM being read as an artifact descriptor declares a `<relocation>`, the artifact under
  resolution is **replaced** by a relocated artifact built from
  `(relocation.groupId, relocation.artifactId, relocation.version)`, with **`null` passed for both
  classifier and extension** — i.e. the original artifact's classifier and extension are preserved,
  and `<relocation>` can never change them.
- Any of the three coordinate fields being null means "keep the original", so a relocation may
  change only the groupId, only the version, and so on.
- Resolution then continues from the relocated coordinates: the new POM is fetched and **its**
  dependencies become the node's children. Relocations chain (a relocated POM may itself relocate).
- Maven logs the relocation at debug level with the `<message>`; `dependency:tree` renders the
  relocated coordinates, so a relocation is visible as the artifact appearing under its new GAV.
- The **relocated POM is normally a stub** — group/artifact/version plus
  `<distributionManagement><relocation>` and often a single dependency on the new artifact.
- **Not inherited.** A child POM never picks up its parent's `<relocation>`; `MavenModelMerger`
  overrides the merge with an empty body. This is essential: otherwise every module of a relocated
  parent would relocate.

### 11.2 `<status>`

`String`, `<required>false</required>`, model version `4.0.0+`. Records the artifact's provenance in
a remote repository. Legal values, per the mdo:

| Value | Meaning |
|---|---|
| `none` | **Default.** No special status. |
| `converted` | A repository manager converted this from a Maven 1 POM. |
| `partner` | Synced directly from a partner Maven 2 repository. |
| `deployed` | Deployed from a Maven 2+ instance. |
| `verified` | Hand-verified as correct and final. |

**Critical rule:** `<status>` **must not appear in a project's own POM**. The mdo says *"This must not
be set in your local project, as it is updated by tools placing it in the repository"*, and
`DefaultModelValidator.validateEffectiveModel` raises **ERROR** — `distributionManagement.status` …
*"must not be specified."* — whenever it is non-null. `jv` should parse it (POMs downloaded from
repositories may legitimately carry it) but reject it in the project under build.

---

## 12. Dependency `type` table

`type` is the POM-level abstraction; it expands into (extension, classifier, language, path types,
includesDependencies). The Maven 4 registry is `DefaultTypeProvider.types()`; the identifiers are the
`Type` constants in `api/maven-api-core/.../Type.java`. `includesDependencies = true` means the
artifact already bundles its own dependencies, so Maven does not put its transitive dependencies on a
consumer's build path.

**"On classpath?"** is `DefaultType`'s derived property
`MavenArtifactProperties.CONSTITUTES_BUILD_PATH` = `pathTypes.contains(JavaPathType.CLASSES)`. This
is the direct successor of Maven 3's `ArtifactHandler.addedToClasspath`. A type with only
`MODULES`/`PROCESSOR_*` path types is *not* on the classpath but is on the module path / annotation
processor path.

| `type` | Extension | Classifier | Language | Path types | On classpath? | includesDependencies | Maven 3.9? |
|---|---|---|---|---|---|---|---|
| `pom` | `pom` | *(none)* | `none` | *(none)* | no | no | yes |
| `bom` | `pom` | *(none)* | `none` | *(none)* | no | no | **[M4] no** |
| `maven-plugin` | `jar` | *(none)* | `java` | `CLASSES` | **yes** | no | yes |
| `jar` | `jar` | *(none)* | `java` | `CLASSES`, `MODULES` | **yes** | no | yes |
| `javadoc` | `jar` | `javadoc` | `java` | `CLASSES` | **yes** | no | yes |
| `java-source` | `jar` | `sources` | `java` | *(none)* | no | no | yes |
| `test-jar` | `jar` | `tests` | `java` | `CLASSES`, `PATCH_MODULE` | **yes** | no | yes |
| `test-java-source` | `jar` | `test-sources` | `java` | *(none)* | no | no | **[M4] no** |
| `modular-jar` | `jar` | *(none)* | `java` | `MODULES` | no (module path only) | no | **[M4] no** |
| `classpath-jar` | `jar` | *(none)* | `java` | `CLASSES` | **yes** | no | **[M4] no** |
| `fatjar` | `jar` | *(none)* | `java` | `CLASSES` | **yes** | **yes** | **[M4] no** |
| `processor` | `jar` | *(none)* | `java` | `PROCESSOR_CLASSES`, `PROCESSOR_MODULES` | no | no | **[M4] no** |
| `classpath-processor` | `jar` | *(none)* | `java` | `PROCESSOR_CLASSES` | no | no | **[M4] no** |
| `modular-processor` | `jar` | *(none)* | `java` | `PROCESSOR_MODULES` | no | no | **[M4] no** |
| `ejb` | `jar` | *(none)* | `java` | `CLASSES` | **yes** | no | yes |
| `ejb-client` | `jar` | `client` | `java` | `CLASSES` | **yes** | no | yes |
| `war` | `war` | *(none)* | `java` | *(none)* | no | **yes** | yes |
| `ear` | `ear` | *(none)* | `java` | *(none)* | no | **yes** | yes |
| `rar` | `rar` | *(none)* | `java` | *(none)* | no | **yes** | yes |
| `par` | `par` | *(none)* | `java` | *(none)* | no | **yes** | (legacy; no lifecycle mapping) |

### 12.1 Default scope per type — there is none

**No `type` carries a default scope**, in either Maven 3 or Maven 4. `DefaultType` has exactly the
fields `id`, `language`, `extension`, `classifier`, `includesDependencies`, `pathTypes`, and the
Maven 3 `ArtifactHandler` interface likewise has no scope. The only scope default in the model is the
unconditional `compile` injected by `DefaultModelNormalizer.injectDependency` for direct and plugin
dependencies ([§4.1](#41-fields)/[§4.2](#42-scope)); `<dependencyManagement>` entries get no scope
default at all. A `test-jar` dependency is therefore `compile`-scoped unless the POM says otherwise —
a frequent source of confusion that `jv` must reproduce faithfully.

### 12.2 File-name construction

For a resolved artifact:

```
<artifactId>-<version>[-<classifier>].<extension>
```

Repository path:

```
<groupId with '.' → '/'>/<artifactId>/<version>/<artifactId>-<version>[-<classifier>].<extension>
```

The `classifier` used is the dependency's explicit `<classifier>` if present, otherwise the one
implied by `<type>` (per the table above). The `extension` always comes from the `type`. Because
`test-jar` implies classifier `tests`, `<type>test-jar</type>` and
`<type>jar</type><classifier>tests</classifier>` resolve to the same file — but they have **different
management keys** (`g:a:test-jar` vs `g:a:jar:tests`), so `<dependencyManagement>` treats them as
distinct entries.

### 12.3 Maven 3 legacy handler table (for cross-checking)

From `impl/maven-core/src/site/markdown/artifact-handlers.md` — the Maven 3 `ArtifactHandler` set,
which is what Maven 3.9 actually uses. Blank cells are false/absent. Note the `packaging` column,
which maps a dependency type back onto the packaging that produces it.

| type | classifier | extension | packaging | language | added to classpath | includesDependencies |
|---|---|---|---|---|---|---|
| `pom` | | `pom` | `pom` | none | | |
| `jar` | | `jar` | `jar` | java | `true` | |
| `test-jar` | `tests` | `jar` | `jar` | java | `true` | |
| `maven-plugin` | | `jar` | `maven-plugin` | java | `true` | |
| `ejb` | | `jar` | `ejb` | java | `true` | |
| `ejb-client` | `client` | `jar` | `ejb` | java | `true` | |
| `war` | | `war` | `war` | java | | `true` |
| `ear` | | `ear` | `ear` | java | | `true` |
| `rar` | | `rar` | `rar` | java | | `true` |
| `java-source` | `sources` | `jar` | `java-source` | java | | |
| `javadoc` | `javadoc` | `jar` | `javadoc` | java | `true` | |

The two tables agree on every type Maven 3 knows, with one difference worth noting: the Maven 4
`javadoc` type is `CLASSES` (on classpath) exactly as in Maven 3, while Maven 4 adds `MODULES` to
plain `jar` and `PATCH_MODULE` to `test-jar` — concepts Maven 3 has no equivalent for. An unknown
type in Maven 3 falls back to `extension = type`, `classifier = none`, `addedToClasspath = false`;
`jv` should use the same fallback, with `extension = type`.

---

## 13. Effective-POM construction order

The order matters and is observable. From `DefaultModelBuilder`:

1. **Read** the file model (raw XML → model). No interpolation yet. Infer `modelVersion` from the
   namespace if absent.
2. **Validate the raw/file model** (`validateFileModel`, `validateRawModel`).
3. **Activate profiles** for this POM against the raw model, using the restricted expression set
   ([§10.7](#107-raw-model-validation-of-activation)).
4. **Inject active profiles** into the model, in declaration order, source-dominant
   ([§10.6](#106-profile-injection)).
5. **Resolve and read the parent** ([§2.1](#21-relativepath-resolution)), recursively applying steps
   1–5 to it; the Super POM is the root of every chain.
6. **Assemble inheritance** (`DefaultInheritanceAssembler`): merge the parent's model into the child
   with the child dominant, applying the per-element rules in [§1.2](#12-child-elements).
7. **Normalise** — `mergeDuplicates`, then `injectDefaultValues` (this is where `scope` becomes
   `compile` for direct and plugin dependencies).
8. **Interpolate** `${…}` (`DefaultModelInterpolator`), then translate paths
   (`DefaultModelPathTranslator`).
9. **Inject `<pluginManagement>`**, then lifecycle bindings for the `packaging`.
10. **Inject and import `<dependencyManagement>`** (`DefaultDependencyManagementInjector`,
    `DefaultDependencyManagementImporter`) — including `scope=import` expansion.
11. **Validate the effective model** (`validateEffectiveModel`).

Interpolation happens *after* inheritance, which is why a child can use a property defined in its
parent, and why `<parent>` coordinates must be literal (they are needed at step 5, before step 8).

---

## 14. Maven 3 / Maven 4 divergences summary

Every item below changes **parsing** or **defaults**. `jv` targets Maven 3.9, so implement the
"Maven 3.9" column and reject or ignore the Maven 4 additions under `<modelVersion>4.0.0</modelVersion>`.

| # | Element / behaviour | Maven 3.9 (implement this) | Maven 4 in this clone | Section |
|---|---|---|---|---|
| 1 | `<modelVersion>` | `4.0.0` only | `4.0.0`, `4.1.0`, `4.2.0` | [§0.4](#04-xml-namespace-and-modelversion) |
| 2 | `<parent><relativePath>` default | schema default `../pom.xml`; empty string forces repository lookup | no schema default; described default `..` (a directory); reactor-by-GA probed before the default path | [§2.1](#21-relativepath-resolution) |
| 3 | `<parent>` coordinate mismatch with `relativePath` | WARNING, falls back to repositories | FATAL when modelVersion ≥ 4.1.0 and `relativePath` was explicit | [§2.1](#21-relativepath-resolution) |
| 4 | `<subprojects>` | **does not exist** | 4.1.0+ alias for `<modules>`; ERROR if used under 4.0.0; `<modules>` WARNs as deprecated under 4.1.0+; both together is an ERROR | [§7](#7-modules--subprojects) |
| 5 | `<mixins>` | does not exist | 4.2.0+ (`Mixin` extends `Parent`, adds `classifier`, `extension`); never inherited | [§2.2](#22-mixins-m4) |
| 6 | `project@root`, `project@preserve.model.version` | do not exist | 4.1.0+ boolean attributes, default `false` | [§1.1](#11-attributes-on-project) |
| 7 | `checksumPolicy` default | **`warn`** | `fail` at the session level (`DefaultRepositoryFactory`); still `warn` on the artifact-descriptor path | [§9.2](#92-releases--snapshots-policy) |
| 8 | Repository `<url>` with an uninterpolated `${…}` | fails later at transport time | WARNING and the repository is **skipped** | [§9.1](#91-repository-fields) |
| 9 | `bom` packaging | not a built-in packaging (build fails) | built-in (`BomLifecycleMappingProvider`) | [§3.4](#34-legal-packaging-values) |
| 10 | `bom` dependency type | does not exist | `extension = pom`, no classifier, not on classpath | [§12](#12-dependency-type-table) |
| 11 | New dependency types | 11 types (see [§12.3](#123-maven-3-legacy-handler-table-for-cross-checking)) | adds `bom`, `test-java-source`, `modular-jar`, `classpath-jar`, `fatjar`, `processor`, `classpath-processor`, `modular-processor`, and path-type concepts (`MODULES`, `PATCH_MODULE`, `PROCESSOR_*`) | [§12](#12-dependency-type-table) |
| 12 | Dependency scopes | `compile`, `provided`, `runtime`, `test`, `system` (+ `import` in management) | adds `none`, `compile-only`, `test-only`, `test-runtime`; the last three are an ERROR under modelVersion 4.0.0 | [§4.2](#42-scope) |
| 13 | `<activation><packaging>` | does not exist | 4.1.0+; exact `Objects.equals` against the model packaging | [§10.2.5](#1025-packaging-m4) |
| 14 | `<activation><condition>` | does not exist | 4.1.0+; full expression language | [§10.2.6](#1026-condition-m4) |
| 15 | `<activation><property><name>packaging</name></name>` | resolves against user/system properties only | falls back to the model's packaging when the user property is unset | [§10.2.3](#1023-property--exact-matching-rules) |
| 16 | `<activation><file>` interpolation | `${basedir}`, `${project.basedir}`, POM properties, user properties, system properties | same, plus `${project.rootDirectory}` | [§10.2.4](#1024-file--exact-matching-rules) |
| 17 | `<file>` with both `exists` and `missing` | undefined/inconsistent | `exists` wins, `missing` ignored, WARNING | [§10.2.4](#1024-file--exact-matching-rules) |
| 18 | `<build><sources>` | does not exist | 4.1.0+ `Source` list replacing `sourceDirectory`/`resources` (which are deprecated but still parsed) | [§8.1](#81-build-fields-relevant-to-resolution) |
| 19 | `<extension><configuration>` | does not exist | 4.1.0+ opaque DOM | [§8.2](#82-extension) |
| 20 | `combine.self` values | `merge`, `override` | adds `remove`; invalid values are an ERROR at validation level `MAVEN_4_0` | [§8.5](#85-configuration-is-opaque) |
| 21 | `<reports>` (the DOM one, not `<reporting>`) | parsed, ignored | `4.0.0` only, removed in 4.1.0 | [§1.2](#12-child-elements) |
| 22 | Exclusion `groupId`/`artifactId` | resolver matcher: exact match, `*` as whole-field wildcard only | mdo documents them as **glob** patterns (`ExclusionArtifactFilter` uses `glob:`), applied on the plugin-facing artifact set | [§4.4](#44-exclusion-matching-semantics) |
| 23 | `Resource.mergeId` | internal, `xml.transient` | `4.0.0` only; the accessors are no-ops returning `null` | [§0.1](#01-modello--xml-mapping) |
