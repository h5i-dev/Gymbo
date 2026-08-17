#!/usr/bin/env bash
#
# Ring 3: `jv tree` against `mvn dependency:tree` on real projects, every module.
#
# The differential harness in `crates/jv-cli/tests/mvn_tree_oracle.rs` uses POMs
# written to exercise one resolution rule each. This runs the same comparison on
# projects nobody wrote for jv's benefit — deep Apache parent chains, BOM-driven
# dependency management, multi-module reactors with intra-reactor dependencies.
# It is where the long tail lives.
#
# Not a `cargo test`, because it clones several gigabytes and takes many minutes.
# Run it before a release, or nightly.
#
# Usage:
#   scripts/ring3.sh [-p PROJECT]...   # default: the small ones
#   scripts/ring3.sh -a                # everything, including the giants
#   scripts/ring3.sh -l                # list what is available
#
# Requires: git, a Maven 3.9 as `mvn` or in $JV_MVN, a JDK, and a release build
# of jv (`cargo build --release`). Set $JV to point elsewhere.
#
# Commits are pinned. A moving target makes a failure impossible to attribute:
# it could be jv, or it could be a dependency that published a new version
# yesterday.

set -euo pipefail

# name|repository|commit|why it is in the corpus
PROJECTS=(
"spring-petclinic|https://github.com/spring-projects/spring-petclinic|88e37c15cf6fc8490b01bc3e8e2c800cec1ac272|the demo everyone runs; Spring Boot BOM, single module"
"dropwizard|https://github.com/dropwizard/dropwizard|v4.0.7|clean mid-size multi-module with its own BOM"
"jackson-databind|https://github.com/FasterXML/jackson-databind|jackson-databind-2.17.1|deep parent chain through jackson-base and oss-parent"
"commons-lang|https://github.com/apache/commons-lang|rel/commons-lang-3.14.0|the Apache parent chain, which is four POMs of pluginManagement"
"maven-dependency-plugin|https://github.com/apache/maven-dependency-plugin|maven-dependency-plugin-3.6.1|a Maven plugin, so the plugin's own resolution rules apply to it"
"camel-core|https://github.com/apache/camel|camel-4.6.0|deep Apache parent chain; huge reactor (use -a)"
"quarkus|https://github.com/quarkusio/quarkus|3.11.0|BOM and dependencyManagement stress test (use -a)"
"netty|https://github.com/netty/netty|netty-4.1.110.Final|large multi-module with classifiers (use -a)"
)

# The ones a pre-release run does by default. The rest are hours of cloning.
DEFAULT=(spring-petclinic dropwizard jackson-databind commons-lang maven-dependency-plugin)

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jv="${JV:-$repository_root/target/release/jv}"
mvn="${JV_MVN:-mvn}"
workspace="${JV_RING3_DIR:-${TMPDIR:-/tmp}/jv-ring3}"

selected=()
while getopts "p:alh" option; do
    case "$option" in
        p) selected+=("$OPTARG") ;;
        a) for entry in "${PROJECTS[@]}"; do selected+=("${entry%%|*}"); done ;;
        l) for entry in "${PROJECTS[@]}"; do
               IFS='|' read -r name _ commit why <<< "$entry"
               printf '%-24s %-38s %s\n' "$name" "$commit" "$why"
           done; exit 0 ;;
        h) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) exit 2 ;;
    esac
done
[[ ${#selected[@]} -eq 0 ]] && selected=("${DEFAULT[@]}")

[[ -x "$jv" ]] || { echo "no jv binary at $jv; run: cargo build --release" >&2; exit 1; }
command -v "$mvn" >/dev/null 2>&1 || [[ -x "$mvn" ]] || {
    echo "no mvn found; install Maven 3.9 or set JV_MVN" >&2; exit 1; }

mkdir -p "$workspace"
settings="$workspace/settings.xml"
echo '<settings/>' > "$settings"
# Shared across projects: they overlap heavily, and this is the difference
# between minutes and hours.
m2="$workspace/m2"
cache="$workspace/jv-cache"

# Cuts the tree out of a `dependency:tree` file and normalizes line ends.
normalize() { sed -e 's/[[:space:]]*$//' "$1"; }

total_modules=0
total_diffs=0
failed_projects=()

for name in "${selected[@]}"; do
    entry=""
    for candidate in "${PROJECTS[@]}"; do
        [[ "${candidate%%|*}" == "$name" ]] && entry="$candidate" && break
    done
    [[ -n "$entry" ]] || { echo "unknown project: $name (try -l)" >&2; exit 2; }
    IFS='|' read -r _ url commit why <<< "$entry"

    checkout="$workspace/$name"
    if [[ ! -d "$checkout/.git" ]]; then
        echo "==> cloning $name at $commit"
        # A shallow clone of one commit: the history is not the point.
        git init -q "$checkout"
        git -C "$checkout" remote add origin "$url"
        git -C "$checkout" fetch -q --depth 1 origin "$commit"
        git -C "$checkout" checkout -q FETCH_HEAD
    fi

    echo "==> $name ($why)"
    # `dependency:tree` on the reactor writes one file per module, which is
    # exactly the comparison wanted — and it resolves the reactor's own modules
    # from the reactor rather than from a repository, as jv does.
    if ! (cd "$checkout" && "$mvn" -q --batch-mode \
            "-Dmaven.repo.local=$m2" -s "$settings" \
            org.apache.maven.plugins:maven-dependency-plugin:3.7.0:tree \
            -DoutputFile=jv-ring3-mvn.txt -DappendOutput=false \
            > "$workspace/$name.mvn.log" 2>&1); then
        echo "    mvn failed; see $workspace/$name.mvn.log"
        failed_projects+=("$name (mvn)")
        continue
    fi

    modules=0
    diffs=0
    while IFS= read -r expected; do
        module_dir="$(dirname "$expected")"
        actual="$module_dir/jv-ring3-jv.txt"
        if ! "$jv" tree -f "$module_dir/pom.xml" \
                --cache-dir "$cache" --no-local-repository -s "$settings" \
                > "$actual" 2>"$module_dir/jv-ring3-jv.err"; then
            echo "    jv failed in ${module_dir#$checkout/}"
            head -3 "$module_dir/jv-ring3-jv.err" | sed 's/^/      /'
            diffs=$((diffs + 1))
            continue
        fi
        modules=$((modules + 1))
        if ! diff -q <(normalize "$expected") <(normalize "$actual") >/dev/null; then
            diffs=$((diffs + 1))
            echo "    DIFF in ${module_dir#$checkout/}"
            diff <(normalize "$expected") <(normalize "$actual") | head -12 | sed 's/^/      /'
        fi
    done < <(find "$checkout" -name jv-ring3-mvn.txt)

    echo "    $modules modules, $diffs differing"
    total_modules=$((total_modules + modules))
    total_diffs=$((total_diffs + diffs))
    [[ $diffs -gt 0 ]] && failed_projects+=("$name ($diffs)")
done

echo
echo "ring 3: $total_modules modules compared, $total_diffs differing"
if [[ ${#failed_projects[@]} -gt 0 ]]; then
    echo "projects with differences: ${failed_projects[*]}"
    exit 1
fi
echo "every module matches mvn dependency:tree"
