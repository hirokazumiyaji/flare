#!/usr/bin/env bash
# Verify that a release tag names the version declared by Cargo.
set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tag="${1:-${GITHUB_REF_NAME:-}}"
if [[ -z "$tag" ]]; then
  echo "usage: $0 <vX.Y.Z>" >&2
  exit 2
fi
if [[ "$tag" != v* || "$tag" == "v" ]]; then
  echo "release tag must have the form vX.Y.Z: $tag" >&2
  exit 2
fi

metadata="$(cargo metadata --no-deps --format-version 1)"
package_version="$(
  printf '%s\n' "$metadata" |
    python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
packages = [package for package in metadata["packages"] if package["name"] == "firelang"]
if len(packages) != 1:
    raise SystemExit(f"expected one firelang package, found {len(packages)}")
print(packages[0]["version"])
'
)"

tag_version="${tag#v}"
if [[ "$tag_version" != "$package_version" ]]; then
  echo "release tag $tag (version $tag_version) does not match Cargo package version $package_version" >&2
  exit 1
fi

printf 'release tag %s matches Cargo package version %s\n' "$tag" "$package_version"
