#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${1:-}"

if [[ ! "$VERSION" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]]; then
  echo "Usage: $0 VERSION (for example: $0 0.2.0)" >&2
  exit 2
fi
if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required to build a Homestock release." >&2
  exit 1
fi

IMAGE="homestock:$VERSION"
PACKAGE="$(mktemp -d)"
trap 'rm -rf -- "$PACKAGE"' EXIT
mkdir -p "$ROOT/dist"

docker build --tag "$IMAGE" --file "$ROOT/Dockerfile" "$ROOT"
docker tag "$IMAGE" homestock:release
docker save --output "$PACKAGE/homestock-image.tar" "$IMAGE" homestock:release
cp "$ROOT/deploy/docker-compose.yml" "$PACKAGE/docker-compose.yml"
cp "$ROOT/deploy/config.example.toml" "$PACKAGE/config.example.toml"
cp "$ROOT/deploy/.env.example" "$PACKAGE/.env.example"
cp "$ROOT/deploy/README.md" "$PACKAGE/README.md"
cp "$ROOT/deploy/install.sh" "$ROOT/deploy/upgrade.sh" "$PACKAGE/"

ARCHIVE="$ROOT/dist/homestock-$VERSION.tar.gz"
tar -C "$PACKAGE" -czf "$ARCHIVE" .
echo "Created $ARCHIVE"
