# Deployment files and releases

This folder is what a release ships to servers. To install Homestock, follow the
[Quickstart](../docs/QUICKSTART.md); this page is for people working on the project.

| File | Installed as | Purpose |
|---|---|---|
| `install.sh` | `/usr/local/bin/homestock-upgrade` | The bootstrap: verifies a release with cosign and runs its installer. The installed copy has the install directory and repository filled in, and adds `--check`. |
| `homestock-installer.sh` | (run from the bundle) | Installs, re-applies or upgrades, with automatic rollback. |
| `homestock-admin.sh` | `/usr/local/bin/homestock-admin` | Account recovery inside the container. |
| `homestock-nas.sh` | `/usr/local/bin/homestock-nas` | Sets up and confirms the NAS copy, and writes its systemd units. |
| `homestock-doctor.sh` | `/usr/local/bin/homestock-doctor` | Health checks. |
| `sync-homestock-backups.sh` | `<install-dir>/` | Run by the sync timer as UID 10001. |
| `docker-compose.yml` | `<install-dir>/` | The container: read-only, no capabilities, UID 10001. |
| `config.example.toml` | `<install-dir>/` | Every `config.toml` setting, with comments. |

## Releases

Publishing a GitHub release with a semantic version tag such as `v1.2.3` runs the `Release`
workflow (`.github/workflows/release.yml`). It:

- builds the image natively for amd64 and arm64 and pushes one multi-arch image to
  `ghcr.io/<owner>/<repository>`, tagged `1.2.3`, `1.2`, `1` and with the commit SHA;
- signs the image with keyless cosign (Sigstore, using the workflow's GitHub identity, so there
  are no keys to manage);
- attaches the signed install files to the release:
  - `homestock-deploy-1.2.3.tar.gz`: this folder, plus `release.env`, which pins the image by
    digest;
  - the `install.sh` bootstrap;
  - `SHA256SUMS`;
  - a `.sigstore.json` signature bundle for each of these.

The installer verifies against the workflow's identity at the release tag, so keep the
workflow's path stable.

The workflow uses the repository's `GITHUB_TOKEN`, so no secrets are needed. Make the GHCR
package public in GitHub's package settings so installs can pull without logging in. A manual
workflow run publishes and signs a commit-SHA image tag only. To build the install files
locally, run `scripts/package-release.sh VERSION IMAGE@sha256:DIGEST OUTDIR`.

To verify an image by hand:

```sh
cosign verify ghcr.io/<owner>/<repository>:1.2.3 \
  --certificate-identity https://github.com/<owner>/<repository>/.github/workflows/release.yml@refs/tags/v1.2.3 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Testing the installer without a release

Run the installer straight from a checkout, with a locally built image:

```sh
docker build -t homestock:dev .
sudo bash deploy/homestock-installer.sh --image homestock:dev --install-dir /tmp/hs/opt \
  --data-dir /tmp/hs/data --certs-dir /tmp/hs/certs
```

Signature checks are skipped, with a warning, when `--image` is given.
