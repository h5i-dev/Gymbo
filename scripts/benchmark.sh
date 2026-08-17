#!/usr/bin/env bash
#
# Times `jv tree` against `mvn dependency:tree` on the same project.
#
# The README quotes numbers from this script, so it exists to make them
# reproducible rather than to make them look good. Three things it does on
# purpose:
#
#   * Cold means cold. Both tools get an empty cache and an empty local
#     repository, so neither is credited for work the other already did.
#   * Warm is measured after a discarded run, because the first warm run pays
#     for page cache that the second does not.
#   * It verifies the two tools agree before reporting how fast they disagreed.
#     A benchmark against wrong output is a benchmark of nothing.
#
# Usage:
#   scripts/benchmark.sh [-n RUNS] [-p POM]
#
# Requires: a Maven 3.9 as `mvn` or in $JV_MVN, and a release build of jv
# (`cargo build --release`). Set $JV to point at a different binary.

set -euo pipefail

runs=5
pom=""
while getopts "n:p:h" option; do
    case "$option" in
        n) runs="$OPTARG" ;;
        p) pom="$OPTARG" ;;
        h) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"

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

# The default project is deliberately ordinary: a web service's worth of
# dependencies, deep enough that resolution is the cost rather than startup.
if [[ -z "$pom" ]]; then
    pom="$workspace/project/pom.xml"
    mkdir -p "$workspace/project"
    cat > "$pom" <<'POM'
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example.bench</groupId>
  <artifactId>bench</artifactId>
  <version>1.0.0</version>
  <dependencies>
    <dependency>
      <groupId>com.fasterxml.jackson.core</groupId>
      <artifactId>jackson-databind</artifactId>
      <version>2.17.1</version>
    </dependency>
    <dependency>
      <groupId>org.apache.httpcomponents.client5</groupId>
      <artifactId>httpclient5</artifactId>
      <version>5.3.1</version>
    </dependency>
    <dependency>
      <groupId>com.google.guava</groupId>
      <artifactId>guava</artifactId>
      <version>33.2.0-jre</version>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>
POM
fi
project="$(cd "$(dirname "$pom")" && pwd)"

settings="$workspace/settings.xml"
echo '<settings/>' > "$settings"

jv_cache="$workspace/jv-cache"
mvn_repository="$workspace/m2"

run_jv() {
    "$jv" tree -f "$pom" --cache-dir "$jv_cache" --no-local-repository -s "$settings"
}

run_mvn() {
    (cd "$project" && "$mvn" -q --batch-mode \
        "-Dmaven.repo.local=$mvn_repository" -s "$settings" \
        org.apache.maven.plugins:maven-dependency-plugin:3.6.1:tree \
        -DoutputFile="$workspace/mvn-tree.txt" -DappendOutput=false >/dev/null)
    cat "$workspace/mvn-tree.txt"
}

# Milliseconds, wall clock. `date +%s%N` is not portable to macOS, so this uses
# the shell's own SECONDS-free arithmetic via python, which is present wherever
# this script is worth running.
elapsed_ms() {
    python3 - "$@" <<'PY'
import subprocess, sys, time
start = time.perf_counter()
result = subprocess.run(sys.argv[1:], capture_output=True)
elapsed = (time.perf_counter() - start) * 1000
if result.returncode != 0:
    sys.stderr.write(result.stderr.decode("utf-8", "replace"))
    sys.exit(result.returncode)
print(f"{elapsed:.1f}")
PY
}

summarize() {
    python3 -c '
import sys
values = sorted(float(line) for line in sys.stdin if line.strip())
n = len(values)
print(f"{values[n // 2]:8.1f} ms   (min {values[0]:.1f}, max {values[-1]:.1f}, n={n})")
'
}

echo "project: $pom"
echo "jv:      $("$jv" --version)"
echo "mvn:     $("$mvn" -v 2>/dev/null | head -1)"
echo

# --- Correctness first ---------------------------------------------------
echo "checking the two agree before timing them..."
run_mvn > "$workspace/expected.txt"
run_jv  > "$workspace/actual.txt"
if ! diff -q <(sed -e 's/[[:space:]]*$//' "$workspace/expected.txt") \
              <(sed -e 's/[[:space:]]*$//' "$workspace/actual.txt") >/dev/null; then
    echo "jv and mvn disagree; timing them would measure nothing:" >&2
    diff <(sed -e 's/[[:space:]]*$//' "$workspace/expected.txt") \
         <(sed -e 's/[[:space:]]*$//' "$workspace/actual.txt") >&2 || true
    exit 1
fi
echo "they agree byte for byte."
echo

# --- Cold ----------------------------------------------------------------
echo "cold (empty cache, one run each — network-bound, so it varies)"
rm -rf "$jv_cache"; printf '  jv  %s ms\n' "$(elapsed_ms "$jv" tree -f "$pom" --cache-dir "$jv_cache" --no-local-repository -s "$settings")"
rm -rf "$mvn_repository"
mvn_cold="$(cd "$project" && elapsed_ms "$mvn" -q --batch-mode "-Dmaven.repo.local=$mvn_repository" -s "$settings" org.apache.maven.plugins:maven-dependency-plugin:3.6.1:tree "-DoutputFile=$workspace/mvn-tree.txt")"
printf '  mvn %s ms\n' "$mvn_cold"
echo

# --- Warm ----------------------------------------------------------------
echo "warm (everything cached, $runs runs, median)"
run_jv >/dev/null   # discard: the first warm run pays for page cache
printf '  jv  '
for _ in $(seq "$runs"); do
    elapsed_ms "$jv" tree -f "$pom" --cache-dir "$jv_cache" --no-local-repository -s "$settings"
done | summarize

run_mvn >/dev/null
printf '  mvn '
for _ in $(seq "$runs"); do
    (cd "$project" && elapsed_ms "$mvn" -q --batch-mode "-Dmaven.repo.local=$mvn_repository" -s "$settings" org.apache.maven.plugins:maven-dependency-plugin:3.6.1:tree "-DoutputFile=$workspace/mvn-tree.txt")
done | summarize
