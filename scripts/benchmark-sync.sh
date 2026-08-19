#!/usr/bin/env bash
#
# `jv sync` against `mvn dependency:go-offline`, both followed by an offline build.
#
# `scripts/benchmark.sh` times `jv tree` against `dependency:tree`, which is a
# demo: nobody waits on a dependency tree. This times the thing people actually
# wait for — preparing a fresh Maven environment and then building it offline,
# which is what a CI runner or a container does on every cache miss.
#
#   arm A:  mvn -B dependency:go-offline   →  mvn -B -o clean verify
#   arm B:  jv sync --recursive            →  mvn -B -o clean verify
#
# Both arms start from an empty local repository, and by default jv starts from
# an empty cache too, because a CI cache miss is the case worth measuring. The
# build half is identical in both arms and uses the same Maven, so the only
# variable is who populated the repository.
#
# Two things are reported besides time, because time alone would be misleading:
#
#   * whether the offline build actually succeeded. `dependency:go-offline` is
#     documented as incomplete and frequently leaves a repository that `mvn -o`
#     cannot build from. An arm that is fast because it fetched less has not
#     won anything, and a benchmark that hid this would be measuring nothing.
#   * how many files landed in the repository, which is what makes a time
#     difference explainable rather than magic.
#
# Usage:
#   scripts/benchmark-sync.sh                     # spring-petclinic, pinned
#   scripts/benchmark-sync.sh -d /path/to/project
#   scripts/benchmark-sync.sh -n 3                # repeat and take the median
#   scripts/benchmark-sync.sh -w                  # let jv reuse a warm cache
#
# Requires: git, a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build
# of jv (`cargo build --release`). Set $JV to point elsewhere.

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"

project_directory=""
runs=1
warm_jv=0

# Pinned, for the same reason the corpus is: a moving target makes a change in
# the number impossible to attribute.
DEMO_REPOSITORY="https://github.com/spring-projects/spring-petclinic"
DEMO_COMMIT="88e37c15cf6fc8490b01bc3e8e2c800cec1ac272"
PLUGIN="org.apache.maven.plugins:maven-dependency-plugin:3.7.0"

while getopts "d:n:wh" option; do
    case "$option" in
        d) project_directory="$OPTARG" ;;
        n) runs="$OPTARG" ;;
        w) warm_jv=1 ;;
        h) sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

if [[ ! -x "$jv" ]]; then
    echo "no jv binary at $jv; run: cargo build --release" >&2
    exit 1
fi
if ! command -v "$mvn" >/dev/null 2>&1 && [[ ! -x "$mvn" ]]; then
    echo "no mvn found; install Maven 3.9 or set JV_MVN" >&2
    exit 1
fi

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT

settings="$workspace/settings.xml"
echo '<settings/>' > "$settings"

if [[ -z "$project_directory" ]]; then
    project_directory="$workspace/project"
    echo "cloning spring-petclinic at ${DEMO_COMMIT:0:12}..." >&2
    git init --quiet "$project_directory"
    git -C "$project_directory" remote add origin "$DEMO_REPOSITORY"
    git -C "$project_directory" fetch --quiet --depth 1 origin "$DEMO_COMMIT"
    git -C "$project_directory" checkout --quiet FETCH_HEAD
fi
project_directory="$(cd "$project_directory" && pwd)"

# Milliseconds, wall clock, via python for portability.
elapsed_ms() {
    python3 - "$@" <<'PY'
import subprocess, sys, time
start = time.perf_counter()
result = subprocess.run(sys.argv[2:], capture_output=True, cwd=sys.argv[1])
elapsed = (time.perf_counter() - start) * 1000
sys.stderr.write(result.stdout.decode("utf-8", "replace")[-4000:])
sys.stderr.write(result.stderr.decode("utf-8", "replace")[-4000:])
print(f"{elapsed:.0f} {result.returncode}")
PY
}

# Artifacts, not files. Maven writes a `.sha1`, a `.md5`, a `_remote.repositories`
# and sometimes a `.lastUpdated` beside every download, so a raw file count says
# more about bookkeeping conventions than about what was fetched.
count_files() {
    find "$1" -type f \( -name '*.jar' -o -name '*.pom' \) 2>/dev/null | wc -l | tr -d ' '
}
repository_bytes() { du -sk "$1" 2>/dev/null | cut -f1; }

# --- One measurement of one arm ------------------------------------------
# Prints: prepare_ms prepare_rc build_ms build_rc files kilobytes
measure() {
    local arm="$1" index="$2"
    local local_repository="$workspace/m2-$arm-$index"
    local cache="$workspace/jv-cache"
    rm -rf "$local_repository"
    mkdir -p "$local_repository"
    (( warm_jv )) || rm -rf "$cache"

    local prepare
    if [[ "$arm" == "mvn" ]]; then
        prepare="$(elapsed_ms "$project_directory" \
            "$mvn" -B -s "$settings" "-Dmaven.repo.local=$local_repository" \
            "$PLUGIN:go-offline" 2> "$workspace/$arm-$index-prepare.log")"
    else
        prepare="$(elapsed_ms "$project_directory" \
            "$jv" sync --recursive -s "$settings" \
            --cache-dir "$cache" --local-repository "$local_repository" \
            2> "$workspace/$arm-$index-prepare.log")"
    fi

    local files kilobytes
    files="$(count_files "$local_repository")"
    kilobytes="$(repository_bytes "$local_repository")"

    # The build half is identical in both arms: same Maven, same goals, offline.
    local build
    build="$(elapsed_ms "$project_directory" \
        "$mvn" -B -o -s "$settings" "-Dmaven.repo.local=$local_repository" \
        clean verify -DskipTests 2> "$workspace/$arm-$index-build.log")"

    echo "$prepare $build $files $kilobytes"
}

median() {
    python3 -c '
import sys
values = sorted(int(v) for v in sys.argv[1:] if v)
print(values[len(values)//2] if values else 0)
' "$@"
}

echo "project: $project_directory"
echo "jv:      $("$jv" --version)"
echo "mvn:     $("$mvn" -v 2>/dev/null | head -1)"
echo "runs:    $runs   (jv cache: $( ((warm_jv)) && echo warm || echo cold ))"
echo

declare -A results
for arm in mvn jv; do
    prepares=(); builds=(); prepare_rcs=(); build_rcs=(); files=""; kilobytes=""
    for index in $(seq "$runs"); do
        read -r p_ms p_rc b_ms b_rc f kb <<< "$(measure "$arm" "$index")"
        prepares+=("$p_ms"); builds+=("$b_ms")
        prepare_rcs+=("$p_rc"); build_rcs+=("$b_rc")
        files="$f"; kilobytes="$kb"
    done
    results[$arm.prepare]="$(median "${prepares[@]}")"
    results[$arm.build]="$(median "${builds[@]}")"
    results[$arm.prepare_rc]="${prepare_rcs[0]}"
    results[$arm.build_rc]="${build_rcs[0]}"
    results[$arm.files]="$files"
    results[$arm.kb]="$kilobytes"
done

ok() { [[ "$1" == "0" ]] && echo "ok" || echo "FAILED"; }

printf '%-26s %12s %12s %12s %10s %10s\n' "" "prepare" "offline build" "total" "artifacts" "MB"
for arm in mvn jv; do
    label=$([[ "$arm" == mvn ]] && echo "mvn dependency:go-offline" || echo "jv sync --recursive")
    total=$(( ${results[$arm.prepare]} + ${results[$arm.build]} ))
    printf '%-26s %11sms %11sms %11sms %10s %10s\n' \
        "$label" "${results[$arm.prepare]}" "${results[$arm.build]}" "$total" \
        "${results[$arm.files]}" "$(( ${results[$arm.kb]} / 1024 ))"
done
echo
printf 'offline build after mvn dependency:go-offline: %s\n' "$(ok "${results[mvn.build_rc]}")"
printf 'offline build after jv sync:                   %s\n' "$(ok "${results[jv.build_rc]}")"

# A faster arm that produced an unbuildable repository has not won. Say so
# rather than printing a ratio that means nothing.
if [[ "${results[mvn.build_rc]}" != "0" || "${results[jv.build_rc]}" != "0" ]]; then
    echo
    echo "NOTE: at least one arm could not build offline, so the times are not"
    echo "comparable as a speed result. The interesting number there is which"
    echo "arm produced a usable repository at all."
    for arm in mvn jv; do
        if [[ "${results[$arm.build_rc]}" != "0" ]]; then
            echo
            echo "--- $arm: why the offline build failed ---"
            grep -m 5 -E "^\[ERROR\]" "$workspace/$arm-1-build.log" || true
        fi
    done
    exit 0
fi

python3 - "${results[mvn.prepare]}" "${results[jv.prepare]}" \
           "$(( ${results[mvn.prepare]} + ${results[mvn.build]} ))" \
           "$(( ${results[jv.prepare]} + ${results[jv.build]} ))" <<'PY'
import sys
mp, jp, mt, jt = (int(v) for v in sys.argv[1:5])
print()
print(f"prepare:   {mp/jp:.1f}x faster" if jp else "prepare:   n/a")
print(f"end to end: {mt/jt:.2f}x faster" if jt else "end to end: n/a")
print()
print("The end-to-end ratio is the honest one: the build half is identical in")
print("both arms, so it bounds what any dependency tool can win.")
PY
