#!/usr/bin/env bash
# Build static/app.css from styles/input.css with a pinned, checksum-verified Tailwind
# standalone CLI (no Node). The CLI is downloaded once into .cache/; the app itself never
# fetches anything at runtime. Extra arguments go to Tailwind, e.g. `--watch` while editing.
set -euo pipefail

VERSION=4.3.3
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

# From the release's sha256sums.txt.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)              PLATFORM=linux-x64;   SHA256=dc61b3ac6b8c9ca874c0cc4c57b2409791a64c5540404ca5f5367360babc313a ;;
  Linux-aarch64|Linux-arm64) PLATFORM=linux-arm64; SHA256=55fd0b241214eff3de1e8ee4f22796662f2d2e7a49bcfca7477cfd0bac398195 ;;
  Darwin-arm64)              PLATFORM=macos-arm64; SHA256=cdf646702987a743464dff4d9c60fd4480d1c1e73dd819a9a67f1078815dce9d ;;
  Darwin-x86_64)             PLATFORM=macos-x64;   SHA256=7922e0953f2110c05976e3bf58f14e643d90427575e766b7d433f5f80cbee7e1 ;;
  *) echo "build-css: no pinned Tailwind CLI for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

BIN="$ROOT/.cache/tailwindcss-$VERSION-$PLATFORM"
if [[ ! -x "$BIN" ]]; then
  mkdir -p "$ROOT/.cache"
  curl -fsSL --retry 3 -o "$BIN.part" \
    "https://github.com/tailwindlabs/tailwindcss/releases/download/v$VERSION/tailwindcss-$PLATFORM"
  if command -v sha256sum >/dev/null; then actual="$(sha256sum "$BIN.part")"; else actual="$(shasum -a 256 "$BIN.part")"; fi
  if [[ "${actual%% *}" != "$SHA256" ]]; then
    rm -f -- "$BIN.part"
    echo "build-css: checksum mismatch for tailwindcss-$PLATFORM v$VERSION" >&2
    exit 1
  fi
  chmod +x "$BIN.part"
  mv -- "$BIN.part" "$BIN"
fi

cd -- "$ROOT"
exec "$BIN" -i styles/input.css -o static/app.css --minify "$@"
