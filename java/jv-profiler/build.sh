#!/usr/bin/env bash
#
# Builds the EventSpy jar.
#
# Deliberately javac and jar rather than Maven: this measures Maven, and a
# measuring tool that needs the thing it measures in order to build is a
# bootstrap problem nobody wants during an investigation.
#
# Usage: java/jv-profiler/build.sh [MAVEN_HOME]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
maven_home="${1:-${MAVEN_HOME:-$(dirname "$(dirname "$(command -v mvn)")")}}"
[[ -d "$maven_home/lib" ]] || { echo "no Maven at $maven_home" >&2; exit 1; }

# $JAVA_HOME first: the JDK that will run the build is the one to compile
# against, and a machine can easily have a `javac` on PATH from another.
javac="javac"
jar="jar"
if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/javac" ]]; then
    javac="$JAVA_HOME/bin/javac"
    jar="$JAVA_HOME/bin/jar"
fi
command -v "$javac" >/dev/null || { echo "no javac; set JAVA_HOME" >&2; exit 1; }

out="$here/target"
rm -rf "$out"
mkdir -p "$out/classes"
classpath="$(ls "$maven_home"/lib/*.jar | tr '\n' ':')"

"$javac" -nowarn -cp "$classpath" -d "$out/classes" \
    "$here/src/main/java/dev/jv/profiler/JvProfiler.java"
cp -r "$here/src/main/resources/." "$out/classes/"
"$jar" --create --file "$out/jv-profiler.jar" -C "$out/classes" .

echo "built $out/jv-profiler.jar"
echo "use:  mvn -Dmaven.ext.class.path=$out/jv-profiler.jar test"
