#!/usr/bin/env bash
#
# Ring 4: `jv sync` then `mvn -o`, on real projects.
#
# Ring 3 (scripts/ring3.sh) asks whether jv's dependency *graph* matches
# Maven's. This asks a different and harder question: whether a repository jv
# populated is one Maven can actually build from, with the network off. That
# exercises resolution, download, checksums, plugins, plugin dependencies, the
# lifecycle plugins no POM mentions, and `_remote.repositories` tracking, all at
# once — and it is the claim `jv sync` exists to make.
#
# A pass means: jv resolved everything, wrote it where Maven looks, and Maven
# then built with `-o` and never reached for the network.
#
# Usage:
#   scripts/corpus.sh                  # the default tier
#   scripts/corpus.sh -t full          # everything in projects.tsv
#   scripts/corpus.sh -p commons-io -p gson
#   scripts/corpus.sh -l               # list the corpus
#   scripts/corpus.sh -b OLD_JV        # also ask an older jv, to catch regressions
#   scripts/corpus.sh -k               # keep clones and repositories for triage
#   scripts/corpus.sh -T               # run tests too (slow, and flakier)
#
# Requires: git, a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build
# of jv (`cargo build --release`). Set $JV to point elsewhere.
#
# The corpus lives in scripts/corpus/projects.tsv so that growing it is data
# rather than code. Every ref there is pinned and was checked with
# `git ls-remote` before being added; an unpinned corpus makes a failure
# impossible to attribute, because the cause could be a dependency published
# yesterday.

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
corpus_file="$repository_root/scripts/corpus/projects.tsv"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"

tier="default"
baseline=""
selected=()
keep=0
run_tests=0
list_only=0

while getopts "t:p:klTb:h" option; do
    case "$option" in
        t) tier="$OPTARG" ;;
        b) baseline="$OPTARG" ;;
        p) selected+=("$OPTARG") ;;
        k) keep=1 ;;
        l) list_only=1 ;;
        T) run_tests=1 ;;
        h) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

[[ -f "$corpus_file" ]] || { echo "no corpus at $corpus_file" >&2; exit 1; }

# --- The corpus -----------------------------------------------------------
# tier <TAB> name <TAB> repository <TAB> ref <TAB> why it is here
mapfile -t rows < <(grep -vE '^\s*(#|$)' "$corpus_file")

if (( list_only )); then
    printf '%-10s %-26s %s\n' TIER NAME WHY
    for row in "${rows[@]}"; do
        IFS=$'\t' read -r row_tier name _repo _ref why <<< "$row"
        printf '%-10s %-26s %s\n' "$row_tier" "$name" "$why"
    done
    exit 0
fi

wanted() {
    local row_tier="$1" name="$2"
    if (( ${#selected[@]} )); then
        local pick
        for pick in "${selected[@]}"; do [[ "$pick" == "$name" ]] && return 0; done
        return 1
    fi
    [[ "$tier" == "full" ]] && return 0
    [[ "$row_tier" == "$tier" ]]
}

if [[ ! -x "$jv" ]]; then
    echo "no jv binary at $jv; run: cargo build --release" >&2
    exit 1
fi
if ! command -v "$mvn" >/dev/null 2>&1 && [[ ! -x "$mvn" ]]; then
    echo "no mvn found; install Maven 3.9 or set JV_MVN" >&2
    exit 1
fi

workspace="${JV_CORPUS_DIR:-$(mktemp -d)}"
mkdir -p "$workspace"
if (( ! keep )); then
    trap 'rm -rf "$workspace"' EXIT
else
    echo "keeping everything under $workspace"
fi

# Clones are shared between runs when -k or $JV_CORPUS_DIR is used, because
# re-cloning jetty to re-run one case is minutes of nothing.
clones="$workspace/clones"
mkdir -p "$clones"

settings="$workspace/settings.xml"
echo '<settings/>' > "$settings"

# --- Reporting ------------------------------------------------------------
passed=0
failed=0
skipped=0
shared=0
declare -a failures=()

note() { printf '    %s\n' "$1"; }

# Three outcomes, not two.
#
#   project  — the build fails here for its own reasons (a JDK it wants, an
#              enforcer rule, a test needing a database). Not jv's.
#   maven    — something is missing offline that `mvn dependency:go-offline`
#              also fails to provide. A shared limit, not a jv regression: a
#              plugin naming an artifact in its own <configuration> (japicmp's
#              previous release, for instance) is invisible to any dependency
#              tool that does not execute the plugin.
#   jv       — missing offline, and Maven's own go-offline *does* provide it.
#              Only this is a jv bug.
#
# Telling `maven` from `jv` needs the control arm, so it is only run when jv's
# arm has already failed: no cost on the happy path, full attribution when it
# matters.
missing_offline() {
    grep -qE "in offline mode|has not been downloaded from it before|Could not resolve dependencies|Could not find artifact|was not found in" "$1"
}

# The artifacts an offline build complained about, so the two arms can be
# compared on the same terms rather than on exit codes.
missing_artifacts() {
    grep -oE "the artifact [^ ]+ has not been downloaded" "$1" \
        | sed 's/the artifact //; s/ has not been downloaded//' | sort -u
}

classify_offline_failure() {
    local log="$1" clone="$2" name="$3"
    missing_offline "$log" || { echo "project"; return; }

    # Control arm: prepare the same project with Maven's own go-offline and
    # build it offline too.
    local control_repository="$workspace/m2-control-$name"
    rm -rf "$control_repository"; mkdir -p "$control_repository"
    local control_log="$workspace/$name.control.log"
    (cd "$clone" && "$mvn" -B -s "$settings" \
        "-Dmaven.repo.local=$control_repository" \
        "org.apache.maven.plugins:maven-dependency-plugin:3.7.0:go-offline" \
        > "$control_log" 2>&1) || true
    (cd "$clone" && "$mvn" -o -B -s "$settings" \
        "-Dmaven.repo.local=$control_repository" \
        -DskipTests verify >> "$control_log" 2>&1) && { echo "jv"; return; }

    # Maven failed offline too, whatever its reason. Not a jv regression, and
    # not a win to claim either.
    echo "maven"
}

run_project() {
    local name="$1" repo="$2" ref="$3" why="$4"
    echo "==> $name ($why)"

    local clone="$clones/$name"
    if [[ ! -d "$clone/.git" ]]; then
        # A tag needs no history; a bare sha does. --depth 1 with an explicit
        # ref covers both without fetching the whole repository.
        if ! git clone --quiet --depth 1 --branch "$ref" "$repo" "$clone" 2>/dev/null; then
            git init --quiet "$clone"
            git -C "$clone" remote add origin "$repo"
            if ! git -C "$clone" fetch --quiet --depth 1 origin "$ref" 2>/dev/null; then
                note "SKIP: cannot fetch $ref"
                (( ++skipped ))
                return 0
            fi
            git -C "$clone" checkout --quiet FETCH_HEAD
        fi
    fi

    local local_repository="$workspace/m2/$name"
    rm -rf "$local_repository"
    mkdir -p "$local_repository"

    # --- jv sync ---
    local sync_log="$workspace/$name.sync.log"
    if ! "$jv" sync --recursive \
            -f "$clone/pom.xml" \
            -s "$settings" \
            --cache-dir "$workspace/cache" \
            --local-repository "$local_repository" \
            > "$sync_log" 2>&1; then
        note "FAIL: jv sync"
        note "$(tail -3 "$sync_log" | sed 's/^/      /')"
        failures+=("$name: jv sync failed")
        (( ++failed ))
        return 0
    fi

    # --- jv sync again, warm ---
    #
    # The CI case `jv sync` exists for is the *second* run, and only the second
    # run exercises the cached-answer paths. A cached 404 skipped through
    # `recently_missing` once stopped counting as that repository's answer, so
    # an unreachable repository decided the outcome and the sync failed —
    # exclusively on warm caches. Cold-only testing passed it twice.
    local warm_repository="$workspace/m2-warm/$name"
    rm -rf "$warm_repository"; mkdir -p "$warm_repository"
    local warm_log="$workspace/$name.warm.log"
    if ! "$jv" sync --recursive \
            -f "$clone/pom.xml" \
            -s "$settings" \
            --cache-dir "$workspace/cache" \
            --local-repository "$warm_repository" \
            > "$warm_log" 2>&1; then
        note "FAIL: jv sync failed on a warm cache after succeeding cold"
        note "$(tail -3 "$warm_log" | sed 's/^/      /')"
        failures+=("$name: warm-cache sync failed")
        (( ++failed ))
        return 0
    fi
    # The two runs must place the same set; a warm run that quietly places less
    # is the same bug wearing a different hat.
    local cold_count warm_count
    cold_count=$(find "$local_repository" -type f \( -name '*.jar' -o -name '*.pom' \) | wc -l)
    warm_count=$(find "$warm_repository" -type f \( -name '*.jar' -o -name '*.pom' \) | wc -l)
    if [[ "$cold_count" != "$warm_count" ]]; then
        note "FAIL: cold placed $cold_count artifacts, warm placed $warm_count"
        failures+=("$name: warm and cold syncs disagree")
        (( ++failed ))
        return 0
    fi

    # --- mvn -o ---
    local goal="verify"
    local skip=(-DskipTests)
    if (( run_tests )); then skip=(); fi

    local build_log="$workspace/$name.build.log"
    if (cd "$clone" && "$mvn" -o --batch-mode -s "$settings" \
            "-Dmaven.repo.local=$local_repository" \
            "${skip[@]}" "$goal" > "$build_log" 2>&1); then
        note "ok: synced and built offline"
        (( ++passed ))
        return 0
    fi

    local cause
    note "offline build failed; running the mvn control arm to attribute it..."
    cause="$(classify_offline_failure "$build_log" "$clone" "$name")"
    if [[ "$cause" == "jv" ]]; then
        note "FAIL: missing offline, and mvn dependency:go-offline provides it"
        note "$(missing_artifacts "$build_log" | head -3 | sed 's/^/      /')"
        failures+=("$name: offline build missing artifacts mvn does provide")
        (( ++failed ))
    elif [[ "$cause" == "maven" ]]; then
        # "Maven cannot do this either" is true and not the whole question. A
        # regression in jv lands here too, because a project jv used to build
        # is still one `go-offline` never could — which is exactly how a
        # `<reporting>` ordering change that cost commons-io its offline build
        # got filed as an ecosystem limitation. So when a baseline is given,
        # ask the older binary before accepting the label.
        if regressed_against_baseline "$name" "$clone" "$goal"; then
            note "REGRESSION: the baseline builds this offline and this jv does not"
            failures+=("$name: the baseline builds it offline, this jv does not")
            (( ++failed ))
        else
            note "SHARED: mvn dependency:go-offline cannot build this offline either"
            note "$(missing_artifacts "$build_log" | head -3 | sed 's/^/      /')"
            (( ++shared ))
        fi
    else
        # A project that does not build on this machine for its own reasons —
        # a JDK it needs, a plugin that wants the network by design, a test
        # that assumes a database. Not jv's, and not silently swallowed.
        note "SKIP: builds offline as far as jv is concerned, but the build itself failed"
        note "$(grep -m 2 -E "^\[ERROR\]" "$build_log" | sed 's/^/      /')"
        (( ++skipped ))
    fi
}

# Whether a previous build of jv can do what this one cannot.
#
# Consulted only once jv's arm has already failed, so it costs nothing on the
# happy path. Without `-b` it is always false, and a run reports exactly what it
# reported before.
regressed_against_baseline() {
    local name="$1" clone="$2" goal="$3"
    [[ -n "$baseline" && -x "$baseline" ]] || return 1
    note "asking the baseline whether this used to build..."

    local repository="$workspace/$name-baseline"
    rm -rf "$repository"
    mkdir -p "$repository"
    "$baseline" sync --recursive -f "$clone/pom.xml" -s "$settings" \
        --cache-dir "$workspace/cache" --local-repository "$repository" \
        > "$workspace/$name.baseline.sync.log" 2>&1 || return 1
    (cd "$clone" && "$mvn" -o -B -s "$settings" \
        "-Dmaven.repo.local=$repository" "${skip[@]}" "$goal" \
        > "$workspace/$name.baseline.build.log" 2>&1)
}

for row in "${rows[@]}"; do
    IFS=$'\t' read -r row_tier name repo ref why <<< "$row"
    wanted "$row_tier" "$name" || continue
    run_project "$name" "$repo" "$ref" "$why"
done

echo
echo "corpus: $passed passed, $failed failed, $shared shared-limit, $skipped skipped"
if (( failed )); then
    echo
    echo "attributable to jv:"
    printf '  %s\n' "${failures[@]}"
    exit 1
fi
echo "every project jv synced was buildable offline"
