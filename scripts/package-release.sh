#!/usr/bin/env bash
# Build the Linux release archive without publishing a raw binary.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <vX.Y.Z> <binary> <output-directory>" >&2
  exit 2
fi

tag="$1"
binary="$2"
output_dir="$3"
version="${tag#v}"

if [[ "$tag" != v* || "$version" == "$tag" || -z "$version" ]]; then
  echo "release package tag must have the form vX.Y.Z: $tag" >&2
  exit 2
fi
if [[ ! -f "$binary" ]]; then
  echo "release binary is missing: $binary" >&2
  exit 1
fi
for required in README.md LICENSE docs/spec.md; do
  if [[ ! -f "$required" ]]; then
    echo "release package is missing required file: $required" >&2
    exit 1
  fi
done

mkdir -p "$output_dir"
package_name="firelang-${version}-linux-x86_64"
staging_dir="$output_dir/$package_name"
archive="$output_dir/${package_name}.tar.gz"
checksum_file="$archive.sha256"

rm -rf "$staging_dir"
rm -f "$archive" "$checksum_file"
mkdir -p "$staging_dir/docs"

# Keep the executable bit in the archive even when the build came from a
# checkout or an artifact transfer that did not preserve modes.
install -m 0755 "$binary" "$staging_dir/fl"
chmod 0755 "$staging_dir/fl"
install -m 0644 README.md LICENSE "$staging_dir/"
install -m 0644 docs/spec.md "$staging_dir/docs/spec.md"

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
  sha256 fl README.md LICENSE docs/spec.md > SHA256SUMS
)
tar -czf "$archive" -C "$output_dir" "$package_name"
(
  cd "$output_dir"
  sha256 "$(basename "$archive")" > "$(basename "$checksum_file")"
)

# Do not leave the unpacked staging directory beside the release assets.
rm -rf "$staging_dir"

printf 'created %s and %s\n' "$archive" "$checksum_file"
