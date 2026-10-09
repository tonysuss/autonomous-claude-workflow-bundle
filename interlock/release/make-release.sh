#!/usr/bin/env bash
# Builds the release zip, interlock-<version>.zip, from the committed tree at
# HEAD: the guide, the installer, the example, a task template, the source,
# and a static Linux program (x86_64, musl, so it runs on any distribution).
#
#   release/make-release.sh OUT_DIR [--macos FILE]
#
# --macos adds a macOS program (the universal build from GitHub Actions, as
# the downloaded zip, the tar.gz or the program). Without it, install.sh gets
# one on the Mac. Needs git, cargo with the x86_64-unknown-linux-musl target,
# musl-gcc, strip and zip.

set -euo pipefail

out="${1:?usage: release/make-release.sh OUT_DIR [--macos FILE]}"
shift
macos=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --macos) macos="${2:?--macos needs a file}" && shift 2 ;;
    *) echo "unknown option $1" >&2 && exit 2 ;;
  esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
[[ -z "$(git status --porcelain -- .)" ]] || { echo "commit your changes first: the release is built from HEAD" >&2; exit 1; }
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
commit="$(git rev-parse HEAD)"
name="interlock-$version"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
pkg="$stage/$name"
mkdir -p "$pkg/bin/linux-x86_64" "$pkg/templates" "$pkg/examples" "$pkg/source"

# The source, as committed, without the recorded evidence (13 MB of run records).
git archive --format=tar HEAD -- . ':(exclude)evidence' | tar -xf - -C "$pkg/source"
cp -R "$pkg/source/examples/export-retry" "$pkg/examples/"
cp release/README.md "$pkg/README.md"
cp release/install.sh "$pkg/install.sh"
cp release/task-template.toml "$pkg/templates/task.toml"
cp ../LICENSE "$pkg/LICENSE" 2> /dev/null || git show HEAD:../LICENSE > "$pkg/LICENSE"
chmod 755 "$pkg/install.sh"

echo "building the static Linux program"
cargo build --release --locked -p interlock-cli --target x86_64-unknown-linux-musl
cp target/x86_64-unknown-linux-musl/release/interlock "$pkg/bin/linux-x86_64/interlock"
strip "$pkg/bin/linux-x86_64/interlock"

if [[ -n "$macos" ]]; then
  mkdir -p "$pkg/bin/macos-universal" "$stage/mac"
  case "$macos" in
    *.zip) unzip -q "$macos" -d "$stage/mac" && tar -xzf "$(find "$stage/mac" -name '*.tar.gz' | head -n1)" -C "$stage/mac" ;;
    *.tar.gz) tar -xzf "$macos" -C "$stage/mac" ;;
    *) cp "$macos" "$stage/mac/interlock" ;;
  esac
  cp "$stage/mac/interlock" "$pkg/bin/macos-universal/interlock"
  chmod 755 "$pkg/bin/macos-universal/interlock"
fi

{
  echo "interlock $version"
  echo "commit: $commit"
  echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "source: https://github.com/tonysuss/autonomous-claude-workflow-bundle/tree/$commit/interlock"
  echo
  echo "sha256:"
  (cd "$pkg" && find bin -type f | sort | xargs sha256sum)
} > "$pkg/RELEASE.txt"

mkdir -p "$out"
zipfile="$(cd "$out" && pwd)/$name.zip"
rm -f "$zipfile"
(cd "$stage" && zip -qry "$zipfile" "$name")
echo "$zipfile ($(du -h "$zipfile" | cut -f1)) from $commit"
