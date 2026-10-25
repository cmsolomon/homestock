# Homestock Docker release

Homestock runs as an offline Docker image. Build a release on a machine with Docker for the same CPU architecture as the Proxmox LXC:

For a full Proxmox VE 9 setup guide covering an unprivileged LXC, Docker Compose, an SMB share mounted inside the LXC with rclone, permissions, HTTPS, backups, and upgrades, see [PROXMOX-LXC.md](./PROXMOX-LXC.md).

```sh
./release.sh 0.2.0
```

## Publish to GitHub Container Registry

The `Publish container` GitHub Actions workflow builds the Docker image and pushes it to `ghcr.io/<owner>/<repository>` when a GitHub release is published. It publishes version tags (for example `1.2.3`, `1.2`, and `1`) and a commit-SHA tag. To publish, create and publish a GitHub release with a semantic version tag such as `v1.2.3`; the workflow uses the repository's `GITHUB_TOKEN`, so no registry secret is needed. A manual workflow run is also available and publishes the commit-SHA tag.

To use the published image from the Proxmox LXC, set `HOMESTOCK_IMAGE=ghcr.io/<owner>/<repository>:<version>` in `.env`; install and upgrade will pull it when `homestock-image.tar` is absent. Set the package visibility to public in GitHub's package settings for unauthenticated pulls; for a private package, log Docker in to `ghcr.io` in the LXC with a token that has `read:packages`. Leave the default `homestock:release` value to use the offline image tarball in a release bundle.

Prepare a Debian-based LXC with Docker Engine and the Docker Compose plugin installed. If it is unprivileged, enable Proxmox `nesting` and `keyctl` for Docker; enable `fuse` as well if mounting an SMB share inside the LXC with rclone. Transfer `dist/homestock-0.2.0.tar.gz` into the LXC and extract it into a dedicated deployment directory. On the LXC:

```sh
tar -xzf homestock-0.2.0.tar.gz
```

## First install

1. Review `config.toml` (created by the installer from the example) and `.env`.
2. Ensure the directories in `.env` exist and are writable by the configured `HOMESTOCK_UID` and `HOMESTOCK_GID` (default `10001`). If running as root with the default local paths: `chown 10001:10001 data backups`.
3. Run `./install.sh`.
4. Configure the HTTPS reverse proxy to reach the app backend. For HAProxy on another host/LXC, follow [PROXMOX-LXC.md](./PROXMOX-LXC.md): bind the backend to the Homestock LXC's fixed LAN IP and allow only HAProxy to connect. Keep `cookie_secure = true`; the browser-facing connection remains HTTPS.
5. Create the initial household admin through the setup page.

`config.toml` sets the in-container SQLite `database_path` and backup `[backup].directory`. Keep the database path at `/var/lib/homestock/homestock.sqlite3` unless you also change the Compose data mount. The LXC source paths are set separately in `.env`; `HOMESTOCK_DATA_DIR` contains the database, photos, and staged backups.

The backup task skips its work unless `.homestock-nas` exists under the configured backup directory. For a local backup folder, create the marker after installation:

```sh
touch backups/.homestock-nas
```

For an SMB/NAS backup destination, mount the share inside the LXC with rclone at `/mnt/homestock-backups`. Set `HOMESTOCK_BACKUP_DIR` in `.env` to that path and ensure the mount is writable by the configured UID/GID. Create `.homestock-nas` on the mounted share itself, never in the empty local mount-point directory. With Docker's automatic startup disabled as described in the Proxmox guide, verify the rclone mount and marker before starting Docker after an LXC reboot.

## Upgrade

Extract the new release over the existing deployment directory, then run `./upgrade.sh`. The archive does not contain `config.toml`, `.env`, or application data, so those remain untouched. Before replacing the container, the script verifies that the configured data directory and database file exist and that the database path is within its persistent mount. It then loads the new image and recreates the service using the existing bind mounts. It does not remove Docker volumes or data. Keep `HOMESTOCK_DATA_DIR` and `database_path` unchanged to continue using the existing database.

The schema version is stored in SQLite's `PRAGMA user_version` (currently version 2). If startup detects pending migrations, it makes an online SQLite backup next to the database and verifies it before changing the schema. Backup failure prevents the migration and app startup. New databases are initialized without this extra backup. Keep an independent backup before upgrades as well; the migration backup is a rollback safeguard, not a substitute for a verified recovery copy.
