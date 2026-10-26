#!/usr/bin/env bash
# Homestock bootstrap installer: installs Homestock, or brings an existing install up to date.
#
#   curl -fsSLO https://github.com/cmsolomon/homestock/releases/latest/download/install.sh
#   sudo bash install.sh [--version X.Y.Z] [installer options…]
#
# It downloads a pinned, checksum-verified cosign, uses it to check that the release's
# deployment bundle was signed by this repository's release workflow at that release's tag,
# then runs the installer inside the bundle. The installer also checks the image signature
# before anything runs. Pass --help to see the installer's options.
#
# The installer keeps a copy of this script as /usr/local/bin/homestock-upgrade, set up for
# that install, so upgrading is: sudo homestock-upgrade [--check | --version X.Y.Z] [--yes]
set -euo pipefail

REPO="${HOMESTOCK_REPO:-cmsolomon/homestock}"   # a fork can point this at itself
INSTALL_DIR=/opt/homestock
COSIGN_VERSION=v3.1.3
OIDC_ISSUER=https://token.actions.githubusercontent.com

say() { printf 'homestock: %s\n' "$*"; }
die() { printf 'homestock: error: %s\n' "$*" >&2; exit 1; }

version="" check=false
passthrough=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) [[ $# -ge 2 ]] || die "--version needs a value"; version="${2#v}"; shift 2 ;;
    --version=*) version="${1#--version=}"; version="${version#v}"; shift ;;
    --install-dir) [[ $# -ge 2 ]] || die "--install-dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
    --check) check=true; shift ;;
    *) passthrough+=("$1"); shift ;;
  esac
done
requested="$version"
! $check || [[ -z "$requested" ]] || die "--check compares with the latest release; leave out --version"

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null || die "$tool is required (on Debian/Ubuntu: apt install curl tar coreutils)"
done

case "$(uname -m)" in
  x86_64)        arch=amd64; cosign_sha256=4629c757b7618056f8ddd7e2625ae9fdd94c0372a65049520bc7d9df9efc7f71 ;;
  aarch64|arm64) arch=arm64; cosign_sha256=c5d324e091826b0d7a78eb16fef316450b4eb9aaec045611c08ba06f5e73220a ;;
  *) die "unsupported CPU architecture $(uname -m); Homestock images are built for amd64 and arm64" ;;
esac

if [[ -z "$version" ]]; then
  latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" \
    || die "could not reach GitHub to find the latest release"
  version="${latest##*/tag/v}"
fi
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die "could not determine a release version (got '$version')"

installed=""
if [[ -f "$INSTALL_DIR/release.env" ]]; then
  installed="$(sed -n 's/^HOMESTOCK_VERSION=//p' "$INSTALL_DIR/release.env" | tail -n 1)"
fi
if $check; then
  say "installed:      ${installed:-nothing in $INSTALL_DIR}"
  say "latest release: $version, notes at https://github.com/$REPO/releases/tag/v$version"
  if [[ "$installed" == "$version" ]]; then say "up to date"; else say "to upgrade: sudo homestock-upgrade"; fi
  exit 0
fi
if [[ -z "$requested" && "$installed" == "$version" ]]; then
  say "Homestock $version, the latest release, is already installed (--version $version re-applies it)"
  exit 0
fi
[[ "$(id -u)" -eq 0 ]] || die "run as root, e.g. sudo bash $0"

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
download() { curl -fsSL --retry 3 -o "$work/$2" "$1" || die "download failed: $1"; }

say "fetching cosign $COSIGN_VERSION to verify the release"
download "https://github.com/sigstore/cosign/releases/download/$COSIGN_VERSION/cosign-linux-$arch" cosign
echo "$cosign_sha256  $work/cosign" | sha256sum -c --quiet - || die "cosign checksum mismatch; not continuing"
chmod +x "$work/cosign"

bundle="homestock-deploy-$version.tar.gz"
base="https://github.com/$REPO/releases/download/v$version"
say "downloading Homestock $version"
download "$base/$bundle" "$bundle"
download "$base/$bundle.sigstore.json" "$bundle.sigstore.json"

identity="https://github.com/$REPO/.github/workflows/release.yml@refs/tags/v$version"
"$work/cosign" verify-blob --bundle "$work/$bundle.sigstore.json" \
  --certificate-identity "$identity" --certificate-oidc-issuer "$OIDC_ISSUER" \
  "$work/$bundle" >/dev/null 2>&1 \
  || die "the release bundle's signature does not match $REPO's release workflow; not installing"
say "verified: signed by $REPO's release workflow for v$version"

tar -xzf "$work/$bundle" -C "$work"
HOMESTOCK_COSIGN="$work/cosign" HOMESTOCK_SIGNER_IDENTITY="$identity" HOMESTOCK_REPO="$REPO" \
  bash "$work/homestock-deploy-$version/homestock-installer.sh" --install-dir "$INSTALL_DIR" "${passthrough[@]}"
