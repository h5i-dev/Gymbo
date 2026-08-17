#!/usr/bin/env bash
# Clones the upstream sources jv is verified against into _reference/.
#
# Nothing upstream is vendored into this repository: the differential tests
# compile and read out of these clones directly. That keeps jv's own history
# free of Apache Maven's sources, at the cost of needing this step.
#
# This script is the single place the list lives. CI calls it, and
# docs/development.md points at it — a second copy of the list is how CI once
# ended up cloning only part of what the tests need.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p _reference

# Each clone earns its place; the tests that need it are named so an unused one
# can be removed rather than carried forever.
#
#   maven-resolver          the version-comparison oracle (jv-version), and the
#                           authority on resolution semantics
#   maven                   the POM/settings/metadata corpus — most of the 2800+
#                           POMs the parser is checked against live here
#   maven-dependency-plugin more POMs, plus the go-offline and tree specs
#   maven-dependency-tree   the text renderer `mvn dependency:tree` uses, needed
#                           for output parity from M5 on
repositories=(
  "https://github.com/apache/maven-resolver"
  "https://github.com/apache/maven"
  "https://github.com/apache/maven-dependency-plugin"
  "https://github.com/apache/maven-dependency-tree"
)

for url in "${repositories[@]}"; do
  name="${url##*/}"
  target="_reference/$name"
  if [ -e "$target" ]; then
    echo "already present: $target"
    continue
  fi
  # Shallow: only the working tree matters, and full history would be gigabytes.
  git clone --depth 1 --quiet "$url" "$target"
  echo "cloned: $target"
done
