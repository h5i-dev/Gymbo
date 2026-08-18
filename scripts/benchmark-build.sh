#!/usr/bin/env bash
#
# What jv is worth on a real build, measured against Maven doing the same job.
#
# The comparison is "from nothing to a built project", both arms online, both
# arms ending in the same artifacts on disk:
#
#   maven:  mvn verify                      (empty ~/.m2, downloads as it goes)
#   jv:     jv sync && mvn -o verify        (empty everything)
#
# That is the only pairing where both sides do the same work. Comparing against
# `mvn dependency:go-offline` is not it: go-offline is documented as incomplete,
# so its repository often cannot build at all, and timing a failure against a
# success says nothing. `scripts/benchmark-sync.sh` still does that comparison
# because "which of these produces a usable repository" is a real question — it
# is just not a speed question.
#
# Warm is measured too, and it is the case jv loses: a restored `~/.m2` *is* the
# repository Maven reads, so Maven has no preparation step at all while jv has
# one. Reporting only the cold number would be choosing the flattering half.
#
# Both arms must reach BUILD SUCCESS or the round is discarded. A fast failure
# is not a fast build, and this script has produced that mistake before.
#
# Usage:
#   scripts/benchmark-build.sh -d PROJECT [-n ROUNDS]
#
# Requires: a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build of
# jv. Set $JV to point elsewhere.

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"

project_directory=""
rounds=3
goal="verify"

while getopts "d:n:g:h" option; do
    case "$option" in
        d) project_directory="$OPTARG" ;;
        n) rounds="$OPTARG" ;;
        g) goal="$OPTARG" ;;
        h) sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

[[ -n "$project_directory" ]] || { echo "-d PROJECT is required" >&2; exit 2; }
project_directory="$(cd "$project_directory" && pwd)"
[[ -x "$jv" ]] || { echo "no jv at $jv; cargo build --release" >&2; exit 1; }

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT
settings="$workspace/settings.xml"
# $JV_MIRROR points both arms at a mirror of Central, through settings.xml, so
# Maven and jv both honour it and both talk to the same host. Central rate
# limits, and a cold benchmark downloads a project's whole dependency set twice
# per round — which is how this machine got throttled for most of a day.
if [[ -n "${JV_MIRROR:-}" ]]; then
    cat > "$settings" <<XML
<settings>
  <mirrors>
    <mirror>
      <id>benchmark-mirror</id>
      <mirrorOf>central</mirrorOf>
      <url>${JV_MIRROR}</url>
    </mirror>
  </mirrors>
</settings>
XML
    echo "mirror:  ${JV_MIRROR}"
else
    echo '<settings/>' > "$settings"
fi

elapsed() {
    python3 - "$@" <<'PY'
import subprocess, sys, time
start = time.perf_counter()
result = subprocess.run(sys.argv[2:], capture_output=True, cwd=sys.argv[1])
print(f"{(time.perf_counter() - start) * 1000:.0f} {result.returncode}")
PY
}

median() { python3 -c '
import sys
values = sorted(int(value) for value in sys.argv[1:] if value)
print(values[len(values)//2] if values else 0)' "$@"; }

echo "project: $project_directory"
echo "goal:    $goal (tests skipped)"
echo "jv:      $("$jv" --version)"
echo "mvn:     $("$mvn" -v 2>/dev/null | head -1)"
echo "rounds:  $rounds, arms alternated"
echo "in flight: ${JV_IN_FLIGHT:-default (32)}"
echo

maven_cold=(); jv_cold=(); maven_warm=(); jv_warm=()
discarded=0

# Warm arms reuse these across rounds, which is what makes them warm.
warm_maven_repository="$workspace/warm-m2"
warm_jv_cache="$workspace/warm-jv-cache"
warm_jv_repository="$workspace/warm-jv-m2"
mkdir -p "$warm_maven_repository" "$warm_jv_cache" "$warm_jv_repository"

for round in $(seq "$rounds"); do
    echo "round $round" >&2

    # --- cold: Maven alone, from an empty local repository -----------------
    repository="$workspace/cold-m2-$round"
    mkdir -p "$repository"
    read -r ms rc <<< "$(elapsed "$project_directory" \
        "$mvn" -B -s "$settings" "-Dmaven.repo.local=$repository" -DskipTests "$goal")"
    maven_rc="$rc"; maven_ms="$ms"
    rm -rf "$repository"

    # --- cold: jv sync, then an offline build ------------------------------
    cache="$workspace/cold-jv-cache-$round"
    repository="$workspace/cold-jv-m2-$round"
    mkdir -p "$cache" "$repository"
    read -r sync_ms sync_rc <<< "$(elapsed "$project_directory" \
        "$jv" sync --recursive -f "$project_directory/pom.xml" -s "$settings" \
        --cache-dir "$cache" --local-repository "$repository")"
    read -r build_ms build_rc <<< "$(elapsed "$project_directory" \
        "$mvn" -o -B -s "$settings" "-Dmaven.repo.local=$repository" -DskipTests "$goal")"
    rm -rf "$cache" "$repository"

    if [[ "$maven_rc" == 0 && "$sync_rc" == 0 && "$build_rc" == 0 ]]; then
        maven_cold+=("$maven_ms")
        jv_cold+=("$(( sync_ms + build_ms ))")
    else
        echo "  cold round discarded (mvn=$maven_rc jv sync=$sync_rc jv build=$build_rc)" >&2
        (( ++discarded ))
    fi

    # --- warm: both caches already populated -------------------------------
    read -r ms rc <<< "$(elapsed "$project_directory" \
        "$mvn" -B -s "$settings" "-Dmaven.repo.local=$warm_maven_repository" \
        -DskipTests "$goal")"
    maven_warm_rc="$rc"; maven_warm_ms="$ms"

    read -r sync_ms sync_rc <<< "$(elapsed "$project_directory" \
        "$jv" sync --recursive -f "$project_directory/pom.xml" -s "$settings" \
        --cache-dir "$warm_jv_cache" --local-repository "$warm_jv_repository")"
    read -r build_ms build_rc <<< "$(elapsed "$project_directory" \
        "$mvn" -o -B -s "$settings" "-Dmaven.repo.local=$warm_jv_repository" \
        -DskipTests "$goal")"

    if [[ "$maven_warm_rc" == 0 && "$sync_rc" == 0 && "$build_rc" == 0 ]]; then
        maven_warm+=("$maven_warm_ms")
        jv_warm+=("$(( sync_ms + build_ms ))")
    else
        echo "  warm round discarded (mvn=$maven_warm_rc jv sync=$sync_rc jv build=$build_rc)" >&2
        (( ++discarded ))
    fi
done

if (( ${#maven_cold[@]} == 0 || ${#maven_warm[@]} == 0 )); then
    echo
    echo "no round completed with both arms green; nothing measured." >&2
    echo "a fast failure is not a fast build, so no number is reported." >&2
    exit 1
fi

printf '%-34s %11s %11s %9s\n' "" "maven" "jv" "speedup"
python3 - "$(median "${maven_cold[@]}")" "$(median "${jv_cold[@]}")" \
          "$(median "${maven_warm[@]}")" "$(median "${jv_warm[@]}")" <<'PY'
import sys
cold_maven, cold_jv, warm_maven, warm_jv = (int(value) for value in sys.argv[1:5])
for label, maven, jv in (
    ("cold: nothing -> built", cold_maven, cold_jv),
    ("warm: caches -> built", warm_maven, warm_jv),
):
    print(f"{label:<34} {maven:>9}ms {jv:>9}ms {maven / jv:>8.2f}x")
PY

echo
echo "rounds used: cold ${#maven_cold[@]}, warm ${#maven_warm[@]}; discarded $discarded"
echo "raw cold  maven: ${maven_cold[*]}"
echo "raw cold  jv:    ${jv_cold[*]}"
echo "raw warm  maven: ${maven_warm[*]}"
echo "raw warm  jv:    ${jv_warm[*]}"
echo
cat <<'NOTE'
Read the warm row as the honest cost: a restored `~/.m2` is already the
repository Maven reads, so Maven prepares nothing and jv still materialises.
Anything below 1.00x there is jv's overhead, and it is real.
NOTE
