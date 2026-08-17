# Maven's local repository, as `jv sync` must leave it

What `~/.m2/repository` has to contain for `mvn -o verify` to succeed against a
directory jv populated and Maven never touched.

Extracted from **maven-resolver 1.9.22**, which is what Maven 3.9.9 ships. The
clone in `_reference/maven-resolver` is 2.0.x master; where the two differ this
document says so, and the 1.9.22 claims were cross-checked against the shipped
jar and against a real `~/.m2` that Maven 3.9.9 populated.

> Line numbers below refer to the 2.0.x clone unless marked otherwise. The
> behaviour is identical in 1.9.22 except where noted.

## Which manager reads what

Maven 3.9 uses `EnhancedLocalRepositoryManager`, not `SimpleLocalRepositoryManager`.
Selection is by priority: `EnhancedLocalRepositoryManagerFactory` declares
`priority = 10.0f`, `SimpleLocalRepositoryManagerFactory` leaves the field at
`0.0f`, and `DefaultLocalRepositoryProvider` takes the first enabled factory that
accepts the repository's content type. Maven creates the local repository with
content type `""`, which Enhanced accepts.

This matters because **only Enhanced reads `_remote.repositories`**. Simple's
`find` is pure file-existence and its `add` is a documented no-op.

## `_remote.repositories`

A `java.util.Properties` file in each *version* directory — so one file covers
every artifact of one groupId:artifactId:version, with one key per file name per
repository.

```
#NOTE: This is a Maven Resolver internal implementation file, its format can be changed without prior notice.
#Mon Aug 17 13:23:43 EDT 2026
xmlpull-1.1.3.1.jar>central=
xmlpull-1.1.3.1.pom>central=
```

| Element | Value |
|---|---|
| Key | `<file name>><repository id>` — the bare file name, a single `>` (U+003E), the id |
| Value | **Always empty.** No code path writes a non-empty one |
| Locally installed | Repository id is the empty string: `artifact-1.0.jar>=` |
| Comment lines | Two, both ignored on read; **neither is required** |

`EnhancedLocalRepositoryManager.getKey` is `path.getFileName() + ">" + repository`,
and `LOCAL_REPO_ID` is `""`. `add()` uses `LOCAL_REPO_ID` when the request carries
no repository, which is what `mvn install` does.

### Escaping

`Properties.store` escapes, in the key: space → `\ `, `:` → `\:`, `=` → `\=`,
`#` → `\#`, `!` → `\!`, `\` → `\\`, tab/newline/CR/FF → `\t`/`\n`/`\r`/`\f`.
**`>` is never escaped.** The file is written as ISO-8859-1, so anything above
U+007E becomes `\uXXXX` — a raw UTF-8 byte would be read back as a different
character, and the key would silently fail to match.

Verified against a real JDK 21 `Properties.store` run rather than quoted from
memory.

### How it is consulted, and the trap

`EnhancedLocalRepositoryManager.checkFind` (`:182-211`) decides availability in
three steps, in this order:

1. If `<name>>=` is present — the locally-installed form — the artifact is
   **available unconditionally**, before the request's repositories are looked at.
2. Otherwise, if any `<name>><id>` matches a repository the current request
   carries, it is available.
3. Otherwise, if `isTracked` finds **no** key at all with the prefix `<name>>`,
   it is available — the inter-op escape hatch for a Simple-managed repository.

Step 3 is why a repository jv populated and left entirely untracked resolves
offline just fine, and it is confirmed by upstream's own `testFindUntrackedFile`.

**The trap is between steps 2 and 3.** A file that *is* mentioned but not under a
configured repository fails both, and the escape hatch does not fire. Offline,
that produces:

> Cannot access central (…) in offline mode and the artifact … has not been
> downloaded from it before.

Maven writes a tracking file whenever it downloads anything into a directory, so
a later online build can turn jv's untracked files into exactly this state.

**Rule: write no tracking file, or write a complete one. Never a partial one.**

### Repository ids, and why the empty id is the safe one

The key function in Maven 3.9 is `SimpleLocalRepositoryManager.getRepositoryKey`,
preserved in 2.x as `RepositoryIdHelper.simpleRepositoryKey`, whose javadoc calls
it "the default `repositoryKey` method in Maven 3":

- A plain remote repository's key is **literally `RemoteRepository.getId()`**.
  Not the URL, not the policies — `RepositoryPolicy` plays no part.
- **A mirror is not special.** `isRepositoryManager()` is false for an ordinary
  `settings.xml` mirror, so the key is the *mirror's* id. Maven substitutes the
  mirror before resolution, so a user with a mirror produces `>my-mirror=`, never
  `>central=`. This is the single most likely mismatch for a hand-populated
  repository.
- A repository *manager* (`isRepositoryManager()`) keys as
  `<id>-<sha1(context + sorted mirrored ids)>`, where `context` is the request
  context (`""`, `"project"`, `"plugin"`), so the same repository yields different
  keys per context.
- `idToPathSegment` sanitization is 2.0.11+; 1.9.22 uses the raw id. Identical for
  ordinary ids.

Because the effective id is not knowable from a POM alone, **jv writes the
locally-installed `<name>>=` form for every file it places**, which step 1 accepts
regardless of configuration, and writes the real repository id alongside it
because it is true and harmless.

## What is *not* required

| | Required for `mvn -o`? | Why |
|---|---|---|
| `.sha1` / `.md5` | **No** | Checksums are verified in the transport layer during download; nothing in the local-repository read path opens one |
| `_maven.repositories` | **No** | Dead. Zero occurrences across every jar in Maven 3.9.9's `lib/` |
| `resolver-status.properties` | **No** | The metadata update-check touch file; offline returns before any update check |
| `<file>.lastUpdated` | **No**, and actively harmful | Records a *failed* download and suppresses retries until the interval elapses. jv must not write these |
| Split repository layout | **No** | `aether.enhancedLocalRepository.split` defaults off; the classic M2 layout applies |

## What *is* required beyond the files themselves

- **Every artifact's `.pom` as well as its main file.** Maven reads descriptors on
  every resolve.
- **Plugins**, and their runtime dependencies. `dependency:go-offline` resolves
  `<reporting><plugins>`, `<build><plugins>` and
  `<build><pluginManagement><plugins>`, in that order — plus the plugins the
  *lifecycle* binds for the project's packaging, which are in none of those three
  and which `jv-model-builder` injects.
- **`maven-metadata-<repoKey>.xml`** whenever Maven actually resolves metadata:
  version ranges, `LATEST`/`RELEASE`, snapshot timestamps, plugin-prefix lookup.
  Offline, `DefaultMetadataResolver` accepts the file purely on presence at the
  exact path — metadata is *not* tracked in `_remote.repositories`, and
  disambiguation is entirely by the `-<repoKey>` infix in the file name. A
  dependency graph of fixed release versions needs none of this.

  The `-<repoKey>` infix means the same mirror-id problem applies here, with no
  equivalent of the locally-installed escape hatch. jv's answer for a locally
  installed snapshot is `maven-metadata-local.xml` with
  `<versioning><snapshot><localCopy>true</localCopy>`.

## Unverified

- `aether.artifactResolver.simpleLrmInterop`'s default in 1.9.22. The config key
  is present in the shipped bytecode; the boolean default is not recoverable from
  the constant pool, and 2.x defaults it to `false`. It only controls whether
  resolver *writes back* a tracking entry after an untracked hit, so it cannot
  stop a hand-populated repository from resolving — but if it is `true`, Maven may
  create or append `_remote.repositories` entries in jv's tree as a side effect.
- The exact snapshot fallback when neither `maven-metadata-local.xml` nor
  `maven-metadata-<repoKey>.xml` exists. `DefaultVersionResolver` lives in
  `maven-resolver-provider`, which is not in the reference clone.
