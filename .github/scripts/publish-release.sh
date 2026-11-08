#!/usr/bin/env bash
# Publish GitHub release $1 (e.g. v0.2.0) on commit $GITHUB_SHA from the
# tarballs in ./dist. Creates the tag if it doesn't exist. Needs GH_TOKEN.
set -euo pipefail
tag="$1"
cd dist
ls ./*.tar.gz >/dev/null
cat ./*.sha256 > SHA256SUMS
cp ../install.sh .
prerelease=()
case "$tag" in *-*) prerelease=(--prerelease) ;; esac
gh release create "$tag" \
  --repo "$GITHUB_REPOSITORY" \
  --title "$tag" \
  --target "$GITHUB_SHA" \
  --generate-notes \
  "${prerelease[@]}" \
  ./*.tar.gz ./*.sha256 SHA256SUMS install.sh
