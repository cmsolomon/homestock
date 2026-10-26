#!/usr/bin/env bash
# Assemble a release's install files in OUTDIR:
#   homestock-deploy-VERSION.tar.gz  everything under deploy/, plus release.env naming the
#                                    exact image (by digest) this release installs
#   install.sh                       the standalone bootstrap installer (deploy/install.sh)
#   SHA256SUMS                       checksums of the two files above
# The release workflow runs this, then signs each file with cosign. Set SOURCE_DATE_EPOCH
# for reproducible archive timestamps.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 VERSION IMAGE@sha256:DIGEST OUTDIR" >&2
  exit 2
fi
VERSION="$1"
IMAGE="$2"
OUT="$3"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "package-release: VERSION must look like 1.2.3 (got '$VERSION')" >&2
  exit 2
fi
if [[ ! "$IMAGE" =~ @sha256:[0-9a-f]{64}$ ]]; then
  echo "package-release: the image must be pinned by digest (…@sha256:<64 hex>)" >&2
  exit 2
fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
NAME="homestock-deploy-$VERSION"
STAGE="$(mktemp -d)"
trap 'rm -rf -- "$STAGE"' EXIT

mkdir "$STAGE/$NAME"
cp -R "$ROOT/deploy/." "$STAGE/$NAME/"
cat > "$STAGE/$NAME/release.env" <<EOF
# Written by scripts/package-release.sh: what this bundle installs.
HOMESTOCK_VERSION=$VERSION
HOMESTOCK_IMAGE=$IMAGE
EOF

mkdir -p "$OUT"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${SOURCE_DATE_EPOCH:-0}" \
  --mode='u+rwX,go+rX,go-w' -C "$STAGE" -cf - "$NAME" | gzip -9n > "$OUT/$NAME.tar.gz"
cp "$ROOT/deploy/install.sh" "$OUT/install.sh"
(cd "$OUT" && sha256sum "$NAME.tar.gz" install.sh > SHA256SUMS)
echo "Packaged $OUT/$NAME.tar.gz, $OUT/install.sh and $OUT/SHA256SUMS"
