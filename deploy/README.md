# Homestock Docker release

Homestock runs as an offline Docker image published to GitHub Container Registry for `linux/amd64` and `linux/arm64`.

For a full Proxmox VE 9 setup guide covering an unprivileged LXC, Docker Compose, an SMB share mounted inside the LXC with rclone, permissions, HTTPS, backups, and upgrades, see [PROXMOX-LXC.md](./PROXMOX-LXC.md).

## Releases

Publishing a GitHub release with a semantic version tag such as `v1.2.3` runs the `Release` workflow (`.github/workflows/release.yml`). It:

- builds the image natively for amd64 and arm64 and pushes one multi-arch image to `ghcr.io/<owner>/<repository>`, tagged `1.2.3`, `1.2`, `1` and with the commit SHA;
- signs the image with keyless cosign (Sigstore, using the workflow's GitHub identity; no keys to manage);
- attaches signed install files to the release: `homestock-deploy-1.2.3.tar.gz` (this directory plus `release.env`, which pins the image by digest), the `install.sh` bootstrap, `SHA256SUMS`, and a `.sigstore.json` signature bundle for each.

The workflow uses the repository's `GITHUB_TOKEN`; no secrets are needed. Make the GHCR package public in GitHub's package settings so installs can pull without logging in. A manual workflow run publishes and signs a commit-SHA image tag only. To build the install files locally, run `scripts/package-release.sh VERSION IMAGE@sha256:DIGEST OUTDIR`.

To verify an image by hand:

```sh
cosign verify ghcr.io/<owner>/<repository>:1.2.3 \
  --certificate-identity https://github.com/<owner>/<repository>/.github/workflows/release.yml@refs/tags/v1.2.3 \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

To use the published image, set `HOMESTOCK_IMAGE=ghcr.io/<owner>/<repository>:<version>` in `.env`.

Prepare a Debian-based LXC with Docker Engine and the Docker Compose plugin installed. If it is unprivileged, enable Proxmox `nesting` and `keyctl` for Docker; enable `fuse` as well if mounting an SMB share inside the LXC with rclone. Transfer `dist/homestock-0.2.0.tar.gz` into the LXC and extract it into `/opt/homestock` (the per-app backup timer uses this path). On the LXC:

```sh
mkdir -p /opt/homestock
tar -xzf homestock-0.2.0.tar.gz -C /opt/homestock
cd /opt/homestock
```

## First install

1. Review `config.example.toml` and `.env`; `./install.sh` creates `config.toml` from the example if needed.
2. Ensure the data and certificate directories in `.env` exist; the data directory must be writable by the configured `HOMESTOCK_UID` and `HOMESTOCK_GID` (default `10001`). Put `cert.pem` and `key.pem` in the cert directory and make the private key readable by that UID.
3. Run `./install.sh` as root. It installs the app, enables Docker's normal boot-time restart behavior, and installs the per-app backup sync timer.
4. Homestock serves HTTPS directly on port `8443`; keep `cookie_secure = true`.
5. Create the initial household admin through the setup page.

`config.toml` sets the in-container SQLite `database_path` and backup timing. Keep the database path at `/var/lib/homestock/homestock.sqlite3` unless you also change the Compose data mount. The LXC source paths are set separately in `.env`; `HOMESTOCK_DATA_DIR` contains the database, photos, and pending local backups. Homestock starts normally even when the NAS is unavailable.

The sync timer only uploads pending backups when `.homestock-nas` exists under the configured backup directory. For a local backup folder, create the marker after installation:

```sh
touch backups/.homestock-nas
```

For an SMB/NAS backup destination, mount the share inside the LXC with rclone at `/mnt/homestock-backups`. Set `HOMESTOCK_BACKUP_DIR` to that path and `HOMESTOCK_BACKUP_REQUIRE_MOUNT=true` in `.env`; ensure the mount is writable by UID/GID `10001`. Create `.homestock-nas` on the mounted share itself, never in the empty local mount-point directory. A per-app systemd timer checks the mount and marker every five minutes and syncs pending backups/photos when available. Docker and Homestock start independently at boot.

## Upgrade

Extract the new release over the existing deployment directory, then run `./upgrade.sh`. The archive does not contain `config.toml`, `.env`, certificates, or application data, so those remain untouched. For an existing installation, update `config.toml` to bind `0.0.0.0:8443` and set `tls_cert`/`tls_key` to the mounted certificate paths; add `HOMESTOCK_CERT_DIR` to `.env` and place the PEM files there before restarting. Before replacing the container, the script verifies that the configured data directory and database file exist and that the database path is within its persistent mount. It then loads the new image and recreates the service using the existing bind mounts. It does not remove Docker volumes or data. Keep `HOMESTOCK_DATA_DIR` and `database_path` unchanged to continue using the existing database.

The schema version is stored in SQLite's `PRAGMA user_version` (currently version 2). If startup detects pending migrations, it makes an online SQLite backup next to the database and verifies it before changing the schema. Backup failure prevents the migration and app startup. New databases are initialized without this extra backup. Keep an independent backup before upgrades as well; the migration backup is a rollback safeguard, not a substitute for a verified recovery copy.
