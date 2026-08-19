#!/usr/bin/env bash
#
# `jv outdated` against `versions:display-dependency-updates`, which is the
# thing it replaces.
#
# Both arms answer the same question — "which declared dependencies have newer
# versions" — over the network, with their caches already warm. That is the
# state a developer is in when they ask, and it is the only pairing where the
# two tools are doing the same work.
#
# Both arms must exit zero or the round is discarded. A tool that fails fast is
# not a fast tool, and this repository has published that mistake before.
#
# Usage:
#   scripts/benchmark-outdated.sh -d PROJECT [-n ROUNDS]
#
# Requires: a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build of
# jv. Set $JV to point elsewhere.

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"
# Pinned: the goal's cost depends on the plugin, so a floating version would
# make two runs of this script incomparable.
versions_plugin="org.codehaus.mojo:versions-maven-plugin:2.16.2"
# The plugin checks `<dependencyManagement>` as well as `<dependencies>` by
# default, and `jv outdated` reports only what a POM declares. Left alone, the
# plugin looks up roughly three times as many artifacts on commons-io — 19
# managed entries inherited from the Apache parent chain against 10 declared —
# and the ratio then measures a difference in scope rather than in speed.
#
# jv arguably *should* report managed entries too, since bumping one is
# actionable. Until it does, the comparison is equalised here rather than
# quietly banked.
same_question=(-DprocessDependencyManagement=false)

project_directory=""
rounds=5

while getopts "d:n:h" option; do
    case "$option" in
        d) project_directory="$OPTARG" ;;
        n) rounds="$OPTARG" ;;
        h) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

[[ -n "$project_directory" ]] || { echo "-d PROJECT is required" >&2; exit 2; }
project_directory="$(cd "$project_directory" && pwd)"
[[ -x "$jv" ]] || { echo "no jv at $jv; cargo build --release" >&2; exit 1; }

workspace="$(mktemp -d)"
trap 'rm -rf "$workspace"' EXIT
settings="$workspace/settings.xml"
# $JV_MIRROR points both arms at a mirror of Central.
#
# Not a thumb on the scale: a mirror is configured in `settings.xml`, so Maven
# and jv both honour it and both arms talk to the same host. It exists because
# Central rate-limits, and a benchmark that can only run when Central is in a
# good mood is a benchmark that never runs. Google's GCS mirror is the usual
# choice and is what the corpus cache already contains entries from.
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
echo "jv:      $("$jv" --version)"
echo "mvn:     $("$mvn" -v 2>/dev/null | head -1)"
echo "plugin:  $versions_plugin"
echo "rounds:  $rounds, arms alternated, caches warm"
echo

# --- warm both sides, untimed -----------------------------------------------
# Downloading the versions plugin is not part of what is being measured, and
# neither is jv's first look at any metadata.
echo "warming both arms (untimed)..." >&2
maven_repository="$workspace/m2"
jv_cache="$workspace/cache"
mkdir -p "$maven_repository" "$jv_cache"

(cd "$project_directory" && "$mvn" -B -q -s "$settings" \
    "-Dmaven.repo.local=$maven_repository" \
    "${same_question[@]}" "$versions_plugin:display-dependency-updates" >/dev/null 2>&1) || true
"$jv" outdated -f "$project_directory/pom.xml" -s "$settings" \
    --cache-dir "$jv_cache" >/dev/null 2>&1 || true

# The warm-up must have worked, or the numbers below measure a cold run and a
# failure rather than two answers.
if [[ ! -d "$maven_repository/org/codehaus/mojo/versions-maven-plugin" ]]; then
    echo "the versions plugin did not download; nothing measured." >&2
    exit 1
fi

maven_times=(); jv_times=(); discarded=0
for round in $(seq "$rounds"); do
    read -r ms rc <<< "$(elapsed "$project_directory" \
        "$mvn" -B -q -s "$settings" "-Dmaven.repo.local=$maven_repository" \
        "${same_question[@]}" "$versions_plugin:display-dependency-updates")"
    maven_ms="$ms"; maven_rc="$rc"

    read -r ms rc <<< "$(elapsed "$project_directory" \
        "$jv" outdated -f "$project_directory/pom.xml" -s "$settings" \
        --cache-dir "$jv_cache")"
    jv_ms="$ms"; jv_rc="$rc"

    if [[ "$maven_rc" == 0 && "$jv_rc" == 0 ]]; then
        maven_times+=("$maven_ms")
        jv_times+=("$jv_ms")
    else
        echo "  round $round discarded (mvn=$maven_rc jv=$jv_rc)" >&2
        (( ++discarded ))
    fi
done

if (( ${#maven_times[@]} == 0 )); then
    echo "no round completed with both arms green; nothing measured." >&2
    exit 1
fi

maven="$(median "${maven_times[@]}")"
jv_median="$(median "${jv_times[@]}")"
python3 - "$maven" "$jv_median" <<'PY'
import sys
maven, jv = (int(value) for value in sys.argv[1:3])
print(f"versions:display-dependency-updates {maven:>8}ms")
print(f"jv outdated                         {jv:>8}ms")
print(f"                                    {maven / jv:>8.1f}x")
PY

echo
echo "rounds used: ${#maven_times[@]}, discarded $discarded"
echo "raw mvn: ${maven_times[*]}"
echo "raw jv:  ${jv_times[*]}"
echo
cat <<'NOTE'
Both arms were warm and online, and both were asked about declared
dependencies only: the plugin's `<dependencyManagement>` pass is turned off,
because jv does not report managed entries and leaving it on would measure a
difference in scope rather than in speed.
NOTE
