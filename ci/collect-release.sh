#!/usr/bin/env bash
# Turns the build outputs of a CI run into the files of a release: one file
# per system with a name that does not change from version to version (so a
# "latest" download link keeps working), plus a checksum list.
#
#   ci/collect-release.sh <folder with the downloaded artifacts> <output folder>
#
# The artifacts are the ones the Installer workflow keeps:
#   windows-installer   the setup program and the portable program (nsis/…)
#   linux-desktop       .deb and .AppImage
#   macos-desktop       .dmg
#   linux-server        the server tarball
# It fails if any of them is missing, so a release is never half a release.
set -euo pipefail

in="${1:?usage: collect-release.sh <artifacts> <out>}"
out="${2:?usage: collect-release.sh <artifacts> <out>}"
mkdir -p "$out"

# One file matching a pattern under a folder, or fail.
one() {
  local dir="$1" pattern="$2" found
  found="$(find "$in/$dir" -type f -name "$pattern" 2>/dev/null | sort)"
  if [ -z "$found" ]; then
    echo "missing: $dir/$pattern" >&2
    exit 1
  fi
  if [ "$(echo "$found" | wc -l)" -ne 1 ]; then
    echo "expected one $dir/$pattern but found:" >&2
    echo "$found" >&2
    exit 1
  fi
  echo "$found"
}

copy() { # <folder> <pattern> <new name>
  local source
  source="$(one "$1" "$2")"
  cp "$source" "$out/$3"
  echo "  $3"
}

echo "release files:"
copy windows-installer '*-setup.exe'         'Prometheus-setup-windows-x64.exe'
copy windows-installer 'Prometheus-portable.exe' 'Prometheus-portable-windows-x64.exe'
copy linux-desktop     '*.deb'               'Prometheus-linux-x64.deb'
copy linux-desktop     '*.AppImage'          'Prometheus-linux-x64.AppImage'
copy macos-desktop     '*.dmg'               'Prometheus-macos-apple-silicon.dmg'
copy linux-server      'rc-server-linux-x86_64.tar.gz' 'Prometheus-server-linux-x64.tar.gz'

# Checksums of the files as they are named here (the ones in the artifacts
# name the files before renaming).
(
  cd "$out"
  if command -v sha256sum >/dev/null; then
    sha256sum -- * | grep -v ' SHA256SUMS.txt$' > SHA256SUMS.txt
  else
    shasum -a 256 -- * | grep -v ' SHA256SUMS.txt$' > SHA256SUMS.txt
  fi
  echo "checksums:"
  sed 's/^/  /' SHA256SUMS.txt
)
