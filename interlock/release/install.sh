#!/bin/sh
# Installs interlock, the one program this package delivers, into a directory
# on your PATH. Run it from the unpacked package:
#
#   ./install.sh                     into ~/.local/bin
#   ./install.sh --prefix DIR        into DIR
#   ./install.sh --from FILE         a build you downloaded: the interlock-macos-universal
#                                    zip or tar.gz from GitHub Actions, or the program itself
#   ./install.sh --build             build it from the source in this package (needs Rust)
#
# It picks the program for this machine: the Linux one in bin/, or on a Mac the
# build GitHub made from this package's exact commit (fetched with your own
# `gh` sign-in), or a build from source. It never needs root.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix="${INTERLOCK_PREFIX:-$HOME/.local/bin}"
from=""
build=0
repo="tonysuss/autonomous-claude-workflow-bundle"
commit=$(sed -n 's/^commit: //p' "$here/RELEASE.txt" 2>/dev/null | head -n1)

say() { printf '%s\n' "$*"; }
die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a directory}"; shift 2 ;;
    --from) from="${2:?--from needs a file}"; shift 2 ;;
    --build) build=1; shift ;;
    -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option $1 (see ./install.sh --help)" ;;
  esac
done

os=$(uname -s)
arch=$(uname -m)
work=$(mktemp -d "${TMPDIR:-/tmp}/interlock-install.XXXXXX")
trap 'rm -rf "$work"' EXIT

# unpack FILE: leaves the program at $work/interlock, from a zip, a tar.gz or the program.
unpack() {
  case "$1" in
    *.zip)
      (cd "$work" && unzip -q -o "$1")
      found=$(find "$work" -name '*.tar.gz' | head -n1)
      if [ -n "$found" ]; then tar -xzf "$found" -C "$work"; fi
      ;;
    *.tar.gz | *.tgz) tar -xzf "$1" -C "$work" ;;
    *) cp "$1" "$work/interlock" ;;
  esac
  [ -f "$work/interlock" ] || die "no interlock program inside $1"
}

# The macOS build GitHub Actions made from this package's commit, through gh.
from_github() {
  command -v gh > /dev/null 2>&1 || return 1
  gh auth status > /dev/null 2>&1 || {
    say "gh is installed but not signed in; run 'gh auth login' to fetch the ready-made build"
    return 1
  }
  [ -n "$commit" ] || return 1
  say "fetching the macOS build GitHub made from commit $commit"
  run=$(gh run list -R "$repo" --workflow interlock.yml --commit "$commit" --status success \
    --json databaseId --jq '.[0].databaseId' 2> /dev/null || true)
  [ -n "$run" ] && [ "$run" != "null" ] || {
    say "no finished build for that commit on GitHub (builds are kept for 90 days)"
    return 1
  }
  gh run download "$run" -R "$repo" -n interlock-macos-universal -D "$work/dl" > /dev/null || return 1
  found=$(find "$work/dl" -name '*.tar.gz' | head -n1)
  [ -n "$found" ] && tar -xzf "$found" -C "$work" && [ -f "$work/interlock" ]
}

from_source() {
  command -v cargo > /dev/null 2>&1 ||
    die "building needs Rust 1.89 or later: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  [ -f "$here/source/Cargo.toml" ] || die "this package has no source/ to build from"
  say "building interlock from source (a few minutes the first time)"
  (cd "$here/source" && cargo build --release --locked -p interlock-cli) || die "the build failed"
  cp "$here/source/target/release/interlock" "$work/interlock"
}

if [ -n "$from" ]; then
  case "$from" in /*) ;; *) from="$PWD/$from" ;; esac
  unpack "$from"
elif [ "$build" = 1 ]; then
  from_source
elif [ "$os" = Linux ] && [ "$arch" = x86_64 ] && [ -f "$here/bin/linux-x86_64/interlock" ]; then
  cp "$here/bin/linux-x86_64/interlock" "$work/interlock"
elif [ "$os" = Darwin ] && [ -f "$here/bin/macos-universal/interlock" ]; then
  cp "$here/bin/macos-universal/interlock" "$work/interlock"
elif [ "$os" = Darwin ] && from_github; then
  :
elif [ "$os" = Darwin ] || [ "$os" = Linux ]; then
  say "no ready-made program for $os $arch here; building from source instead"
  from_source
else
  die "interlock runs on macOS and Linux, not $os"
fi

chmod 755 "$work/interlock"
# macOS holds back a downloaded program until its quarantine mark is cleared.
if [ "$os" = Darwin ]; then xattr -d com.apple.quarantine "$work/interlock" 2> /dev/null || true; fi
version=$("$work/interlock" --version 2> /dev/null) || die "the program does not run on this machine ($os $arch)"

mkdir -p "$prefix"
cp "$work/interlock" "$prefix/interlock"
say "installed $version at $prefix/interlock"

case ":$PATH:" in
  *":$prefix:"*) ;;
  *)
    profile="$HOME/.profile"
    case "${SHELL:-}" in */zsh) profile="$HOME/.zshrc" ;; */bash) profile="$HOME/.bashrc" ;; esac
    say ""
    say "$prefix is not on your PATH. Add it, then open a new terminal:"
    say "  echo 'export PATH=\"$prefix:\$PATH\"' >> $profile"
    ;;
esac

say ""
say "Next: README.md, \"Try it in ten minutes\"."
for tool in git copilot; do
  command -v "$tool" > /dev/null 2>&1 || say "Note: $tool is not installed yet; README.md says how."
done
