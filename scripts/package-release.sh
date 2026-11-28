#!/usr/bin/env bash
# Build the Linux release archive without publishing a raw binary.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ $# -lt 3 || $# -gt 4 ]]; then
  echo "usage: $0 <vX.Y.Z> <binary> <output-directory> [target-platform]" >&2
  exit 2
fi

tag="$1"
binary="$2"
output_dir="$3"
platform="${4:-linux-x86_64}"
version="${tag#v}"

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release package tag must have the form vX.Y.Z: $tag" >&2
  exit 2
fi
if [[ ! -f "$binary" ]]; then
  echo "release binary is missing: $binary" >&2
  exit 1
fi
for required in README.md LICENSE docs/spec.md docs/firebase-security.md docs/release-checklist.md; do
  if [[ ! -f "$required" ]]; then
    echo "release package is missing required file: $required" >&2
    exit 1
  fi
done
shopt -s nullglob
example_scripts=(examples/scripts/*.fl)
shopt -u nullglob
if [[ ${#example_scripts[@]} -eq 0 ]]; then
  echo "release package is missing required file: examples/scripts/*.fl" >&2
  exit 1
fi

mkdir -p "$output_dir"
package_name="flare-${version}-${platform}"
staging_dir="$output_dir/$package_name"
archive="$output_dir/${package_name}.tar.gz"
checksum_file="$archive.sha256"

rm -rf "$staging_dir"
rm -f "$archive" "$checksum_file"
mkdir -p "$staging_dir/docs" "$staging_dir/examples/scripts"

# Keep the executable bit in the archive even when the build came from a
# checkout or an artifact transfer that did not preserve modes.
install -m 0755 "$binary" "$staging_dir/fl"
chmod 0755 "$staging_dir/fl"
install -m 0644 README.md LICENSE "$staging_dir/"
install -m 0644 docs/spec.md docs/firebase-security.md docs/release-checklist.md \
  "$staging_dir/docs/"
install -m 0644 "${example_scripts[@]}" "$staging_dir/examples/scripts/"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

# A checksum manifest travels with the files it describes.  The archive also
# gets a separate sidecar so a downloader can verify it before extraction.
(
  cd "$staging_dir"
  paths=()
  while IFS= read -r path; do
    paths+=("$path")
  done < <(find . -type f | sed 's|^\./||' | sort)
  sha256 "${paths[@]}" > SHA256SUMS
)
tar -czf "$archive" -C "$output_dir" "$package_name"
(
  cd "$output_dir"
  sha256 "$(basename "$archive")" > "$(basename "$checksum_file")"
)

# Do not leave the unpacked staging directory beside the release assets.
rm -rf "$staging_dir"

printf 'created %s and %s\n' "$archive" "$checksum_file"
