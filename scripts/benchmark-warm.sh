#!/usr/bin/env bash
#
# The CI cache *hit*: what each tool costs when its cache was restored.
#
# `benchmark-sync.sh` measures the miss — an empty cache, everything
# downloaded. This measures the case a runner is in most of the time, and the
# two tools are not symmetric in it, which is the whole reason this is a
# separate script.
#
#   Maven caches `~/.m2/repository`. On a hit the repository *is* the cache, so
#   there is nothing to prepare: the job runs `mvn -o verify` and that is all.
#
#   jv caches its own store, which is keyed by URL and shared across projects.
#   On a hit it still has to materialise into `~/.m2` before Maven can read it,
#   so jv pays a step Maven does not.
#
# Reporting anything else would flatter jv. `benchmark-sync.sh -w` warms jv's
# cache and leaves Maven's cold, which is not a comparison at all.
#
# Both caches are populated online first, untimed. Only what happens on the
# subsequent run is measured.
#
# The cache *size* is reported too, because a CI runner pays to upload,
# download and unpack it on every job — often more than either prepare step
# costs.
#
# Usage:
#   scripts/benchmark-warm.sh [-n RUNS] [-d PROJECT]
#
# Requires: git, a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build
# of jv. Set $JV to point elsewhere.

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"

project_directory=""
runs=3

while getopts "d:n:h" option; do
    case "$option" in
        d) project_directory="$OPTARG" ;;
        n) runs="$OPTARG" ;;
        h) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

[[ -n "$project_directory" ]] || { echo "-d PROJECT is required" >&2; exit 2; }
project_directory="$(cd "$project_directory" && pwd)"

if [[ ! -x "$jv" ]]; then
    echo "no jv binary at $jv; run: cargo build --release" >&2
    exit 1
fi

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT
settings="$workspace/settings.xml"
echo '<settings/>' > "$settings"

elapsed_ms() {
    python3 - "$@" <<'PY'
import subprocess, sys, time
start = time.perf_counter()
result = subprocess.run(sys.argv[2:], capture_output=True, cwd=sys.argv[1])
print(f"{(time.perf_counter() - start) * 1000:.0f} {result.returncode}")
PY
}

median() { python3 -c '
import sys
v = sorted(int(x) for x in sys.argv[1:] if x)
print(v[len(v)//2] if v else 0)' "$@"; }

kb() { du -sk "$1" 2>/dev/null | cut -f1; }

echo "project: $project_directory"
echo "jv:      $("$jv" --version)"
echo "mvn:     $("$mvn" -v 2>/dev/null | head -1)"
echo "runs:    $runs"
echo

# --- Populate both caches, untimed -----------------------------------------
echo "populating both caches online (untimed)..." >&2

maven_cache="$workspace/m2-maven"
mkdir -p "$maven_cache"
(cd "$project_directory" && "$mvn" -B -s "$settings" \
    "-Dmaven.repo.local=$maven_cache" \
    org.apache.maven.plugins:maven-dependency-plugin:3.7.0:go-offline >/dev/null 2>&1) || true

jv_cache="$workspace/jv-cache"
"$jv" sync --recursive -f "$project_directory/pom.xml" -s "$settings" \
    --cache-dir "$jv_cache" --cache-only >/dev/null 2>&1 || true

# --- Measure ----------------------------------------------------------------
mvn_builds=(); jv_prepares=(); jv_builds=()
mvn_rc=0; jv_rc=0

for _ in $(seq "$runs"); do
    # Maven: the restored repository is the cache. No prepare step exists.
    read -r ms rc <<< "$(elapsed_ms "$project_directory" \
        "$mvn" -o -B -s "$settings" "-Dmaven.repo.local=$maven_cache" \
        -DskipTests verify)"
    mvn_builds+=("$ms"); mvn_rc="$rc"

    # jv: materialise from the store into a fresh local repository, then build.
    local_repository="$workspace/m2-jv"
    rm -rf "$local_repository"; mkdir -p "$local_repository"
    read -r ms rc <<< "$(elapsed_ms "$project_directory" \
        "$jv" sync --recursive -f "$project_directory/pom.xml" -s "$settings" \
        --cache-dir "$jv_cache" --local-repository "$local_repository")"
    jv_prepares+=("$ms")
    [[ "$rc" != 0 ]] && jv_rc="$rc"

    read -r ms rc <<< "$(elapsed_ms "$project_directory" \
        "$mvn" -o -B -s "$settings" "-Dmaven.repo.local=$local_repository" \
        -DskipTests verify)"
    jv_builds+=("$ms"); [[ "$rc" != 0 ]] && jv_rc="$rc"
done

mvn_build="$(median "${mvn_builds[@]}")"
jv_prepare="$(median "${jv_prepares[@]}")"
jv_build="$(median "${jv_builds[@]}")"

ok() { [[ "$1" == "0" ]] && echo ok || echo FAILED; }

printf '%-26s %11s %11s %11s %10s\n' "" "prepare" "build" "total" "cache MB"
printf '%-26s %10s %10sms %10sms %10s\n' \
    "mvn, cached ~/.m2" "none" "$mvn_build" "$mvn_build" "$(( $(kb "$maven_cache") / 1024 ))"
printf '%-26s %9sms %10sms %10sms %10s\n' \
    "jv sync, cached store" "$jv_prepare" "$jv_build" "$(( jv_prepare + jv_build ))" \
    "$(( $(kb "$jv_cache") / 1024 ))"
echo
printf 'offline build, mvn cache: %s\n' "$(ok "$mvn_rc")"
printf 'offline build, jv  cache: %s\n' "$(ok "$jv_rc")"
echo
cat <<'NOTE'
Reading this honestly: on a cache hit Maven has no prepare step, because what
it cached is already the repository Maven reads. jv pays one — materialising
its store into `~/.m2` — so on a single project jv cannot win the clock here.

What jv trades that for is not visible in one project: its store is keyed by
URL, so one cache serves every project, branch and worktree on the runner,
where a cached `~/.m2` is per-configuration and diverges as soon as two builds
want different versions. The cache size column is the part that does transfer.
NOTE
