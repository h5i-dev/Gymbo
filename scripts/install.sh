#!/usr/bin/env sh
#
# Installs jv.
#
#   curl -LsSf https://raw.githubusercontent.com/Koukyosyumei/jv/main/scripts/install.sh | sh
#
# Options, as environment variables:
#
#   JV_VERSION      a tag to install, e.g. v0.1.0. Default: the latest release.
#   JV_INSTALL_DIR  where to put the binaries. Default: ~/.local/bin.
#   JV_NO_MODIFY_PATH=1
#                   do not offer to change your shell profile.
#
# This is a pipe-to-shell installer, which means you are trusting it before you
# have read it. Two things it does to deserve that, and one it does not:
#
#   * It verifies the archive's SHA-256 against the SHA256SUMS published with
#     the release, and refuses to install on a mismatch. An installer that
#     downloads over TLS and stops there is trusting the release host as much as
#     the transport.
#   * It never writes outside the install directory, and never uses sudo. If the
#     directory needs privileges, it says so and stops rather than escalating.
#   * It does not edit your shell profile without asking, and never silently.
#
# POSIX sh on purpose: this has to run in whatever `sh` a container happens to
# ship, which is not always bash.

set -eu

REPOSITORY="Koukyosyumei/jv"
install_dir="${JV_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'jv: %s\n' "$1" >&2; }
die() { printf 'jv: error: %s\n' "$1" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "this installer needs $1, which is not on your PATH"
}

# --- Which build ---------------------------------------------------------

detect_target() {
    kernel="$(uname -s)"
    machine="$(uname -m)"

    case "$machine" in
        x86_64 | amd64) arch=x86_64 ;;
        # `arm64` is what macOS calls it and `aarch64` is what Linux does; they
        # are the same thing and the release names use the Rust spelling.
        aarch64 | arm64) arch=aarch64 ;;
        *) die "unsupported architecture: $machine" ;;
    esac

    case "$kernel" in
        Linux) echo "$arch-unknown-linux-gnu" ;;
        Darwin) echo "$arch-apple-darwin" ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "Windows is not supported yet; see ROADMAP.md" ;;
        *) die "unsupported operating system: $kernel" ;;
    esac
}

latest_version() {
    # The redirect from /releases/latest names the tag, which avoids needing a
    # JSON parser and avoids the GitHub API's rate limit on unauthenticated
    # requests — the two things that make installers fail in CI.
    url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPOSITORY/releases/latest")" \
        || die "cannot reach GitHub to find the latest release"
    tag="${url##*/}"
    [ -n "$tag" ] && [ "$tag" != "releases" ] || die "no release found for $REPOSITORY"
    echo "$tag"
}

# --- Verification --------------------------------------------------------

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "no sha256sum or shasum available, so the download cannot be verified"
    fi
}

# Refuses unless the file's digest is the one the release published.
#
# A missing entry is a failure, not a pass: "no checksum was published for this
# file" and "this file matches its checksum" must never take the same branch.
verify_checksum() {
    file="$1"; sums="$2"; name="$3"; release="$4"
    expected="$(grep "  *$name\$" "$sums" | cut -d' ' -f1 | head -1)"
    [ -n "$expected" ] || die "$release publishes no checksum for $name"
    actual="$(sha256_of "$file")"
    if [ "$expected" != "$actual" ]; then
        die "checksum mismatch for $name
  published: $expected
  found:     $actual
Nothing was installed."
    fi
}

# --- Install -------------------------------------------------------------

need curl
need tar

target="$(detect_target)"
version="${JV_VERSION:-$(latest_version)}"
archive="jv-$target.tar.gz"
base="https://github.com/$REPOSITORY/releases/download/$version"

say "installing jv $version for $target"

workspace="$(mktemp -d)"
# Cleans up on failure too, which matters because the failure paths here are
# network failures and those are the common case.
trap 'rm -rf "$workspace"' EXIT INT TERM

curl -fsSL "$base/$archive" -o "$workspace/$archive" \
    || die "cannot download $base/$archive"
curl -fsSL "$base/SHA256SUMS" -o "$workspace/SHA256SUMS" \
    || die "cannot download the checksums for $version"

verify_checksum "$workspace/$archive" "$workspace/SHA256SUMS" "$archive" "$version"
say "checksum verified"

tar -xzf "$workspace/$archive" -C "$workspace" || die "the archive is not readable"
extracted="$workspace/jv-$target"

mkdir -p "$install_dir" 2>/dev/null || die "cannot create $install_dir"
[ -w "$install_dir" ] || die "$install_dir is not writable. Set JV_INSTALL_DIR to somewhere you own."

installed=""
for binary in jv jvx; do
    [ -f "$extracted/$binary" ] || continue
    # Install through a temporary name and rename, so an interrupted install
    # cannot leave a half-written binary where a working one used to be.
    cp "$extracted/$binary" "$install_dir/.$binary.new"
    chmod +x "$install_dir/.$binary.new"
    mv "$install_dir/.$binary.new" "$install_dir/$binary"
    installed="$installed $binary"
done
[ -n "$installed" ] || die "the archive contained no binaries"

say "installed$installed to $install_dir"

# --- PATH ----------------------------------------------------------------

case ":$PATH:" in
    *":$install_dir:"*) exit 0 ;;
esac

say ""
say "$install_dir is not on your PATH. Add it with:"
say ""
say "    export PATH=\"$install_dir:\$PATH\""
say ""
if [ "${JV_NO_MODIFY_PATH:-0}" != "1" ]; then
    say "(jv does not edit your shell profile for you.)"
fi
