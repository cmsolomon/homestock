# Homestock on Proxmox VE 9: Docker Compose in an LXC

This guide installs Homestock in an unprivileged Debian LXC on Proxmox VE 9, runs it with Docker Compose, and mounts an SMB share inside the LXC using rclone for backups.

## Recommended layout

- The live SQLite database, SQLite WAL files, photos, and temporary backup staging files live on the LXC's local storage.
- rclone mounts the SMB share inside the LXC at `/mnt/homestock-backups`. Homestock writes completed database backups and copies photos there.
- Docker Compose runs the published Homestock image as UID/GID `10001`.
- Homestock serves HTTPS directly on port `8443` using a certificate mounted from the LXC.

**Do not put the live SQLite database or its `-wal`/`-shm` files on the SMB mount.** SQLite WAL relies on local filesystem locking. Homestock stages an online SQLite backup locally, validates it, then copies the completed backup to the SMB destination.

Example paths used below:

| Purpose | Path |
|---|---|
| rclone SMB mount inside the LXC | `/mnt/homestock-backups` |
| Homestock deployment files inside the LXC | `/opt/homestock` |
| Local database and photos inside the LXC | `/srv/homestock/data` |
| GHCR image | `ghcr.io/ntyndall1/homestock:<release-version>` |

Change the share address, share name, LXC ID, release version, and paths to suit your environment.

## 1. Create the LXC in the Proxmox GUI

In the Proxmox VE 9 web interface:

1. Download a current **Debian 13 (Trixie)** LXC template.
2. Click **Create CT** and give it a unique CT ID and hostname such as `homestock`. Use an **unprivileged** container.
3. Give it a static DHCP lease or another stable LAN address.
4. Allocate at least 2 CPU cores, 1 GB RAM, and enough local disk for the OS, Docker image, database, and photos. Increase disk space as the photo library grows.
5. On the **Template** page select Debian 13; on **Disks**, use local storage (not the NAS CIFS share).
6. Finish creating the container, then select it and open **Options → Features → Edit**. Enable **Nesting**, **Keyctl**, and **FUSE**.
7. In **Options → Start at boot**, enable automatic startup. Homestock starts independently of the NAS; its backup sync timer waits for the SMB mount and sentinel.
8. Start the container from the GUI, then open its **Console** tab.

Record the numeric CT ID. If you prefer the CLI, `pct start <CTID>` and `pct enter <CTID>` on the Proxmox host perform the equivalent start and console access. Confirm the CT is unprivileged in **Options** or with `pct config <CTID>`; the latter should include `unprivileged: 1`.

Docker-in-LXC adds a layer of kernel and permission constraints. Keep the LXC unprivileged. If Docker reports overlay filesystem or permission errors that cannot be resolved with the documented nesting features, use a small Debian VM for Docker rather than making the LXC privileged.

## 2. Configure rclone to access the SMB share

Create a dedicated SMB user on TrueNAS with write access only to the Homestock backup share. In the earlier examples, the SMB server is `nas.intra.tynsol.com` and the account is `homestock`. Use the exact SMB share name shown in TrueNAS; the share name is distinct from the SMB username.

The rclone mount runs **inside CT 107**, not on the Proxmox host. rclone uses FUSE, so the LXC needs the **FUSE** feature in addition to **Nesting** and **Keyctl**. If you did not enable FUSE while creating CT 107:

1. Stop CT 107 in the Proxmox GUI.
2. Open **CT 107 → Options → Features → Edit** and enable **FUSE**. Leave the container unprivileged; do not enable CIFS kernel mounting or make the container privileged.
3. Start CT 107 and open its Console.
Confirm `/dev/fuse` exists inside the LXC:

```sh
ls -l /dev/fuse
```

If the FUSE option is unavailable in the GUI, use the Proxmox node Shell only after checking the existing feature flags with `pct config 107`. Add `fuse=1` while preserving existing flags such as `nesting=1,keyctl=1`, then restart the CT:

```sh
pct set 107 --features 'nesting=1,keyctl=1,fuse=1'
```

Install rclone and FUSE utilities in the LXC:

```sh
apt update
apt install -y rclone fuse3
install -d -m 0755 /mnt/homestock-backups /var/cache/rclone
```

Run the interactive configuration as root:

```sh
rclone config
```

Create a new remote with these values (the prompts may vary slightly by rclone version):

- **Name:** `homestock`
- **Storage type:** `smb`
- **Host:** `nas.intra.tynsol.com`
- **User:** `homestock`
- **Port:** press Enter for the default `445`
- **Password:** enter the SMB account password at the prompt; do not put it in a shell command
- **Domain:** leave the default unless TrueNAS requires a specific domain/workgroup
- Accept/save the configuration and quit.

rclone stores the password in obscured form in its configuration; this is not strong encryption. Protect the root-owned config file:

```sh
chmod 0700 /root/.config/rclone
chmod 0600 /root/.config/rclone/rclone.conf
```

Check the connection and list available shares:

```sh
rclone lsd homestock:
```

Use the exact intended share name returned by that command in place of `<SHARE>`. Check the share contents:

```sh
rclone lsf homestock:<SHARE>
```

Do not continue until this lists the intended backup share. In rclone paths, the first component after `homestock:` is the SMB share name.

## 3. Mount the SMB share inside the LXC

Create a systemd service so the rclone mount is available after the LXC starts. The mount presents files as owned by UID/GID `10001`, which is the identity used by the Homestock Docker container. `--vfs-cache-mode writes` lets rclone safely handle normal file writes; the live SQLite database remains on local LXC storage.

Create `/etc/systemd/system/rclone-homestock.service` inside the LXC:

```ini
[Unit]
Description=Homestock SMB backup mount (rclone)
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart=/usr/bin/rclone mount homestock:<SHARE> /mnt/homestock-backups --config=/root/.config/rclone/rclone.conf --allow-other --default-permissions --uid=10001 --gid=10001 --file-perms=0600 --dir-perms=0700 --vfs-cache-mode=writes --cache-dir=/var/cache/rclone --vfs-cache-max-size=2G --vfs-cache-max-age=24h
Restart=on-failure
RestartSec=10
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
```

Replace `<SHARE>` with the actual TrueNAS SMB share name. If the 2 GiB cache limit is too small for your database backups or photo collection, increase it; cached writes temporarily use local LXC disk space.

Enable and start the mount:

```sh
systemctl daemon-reload
systemctl enable --now rclone-homestock.service
systemctl status rclone-homestock.service
mountpoint -q /mnt/homestock-backups && findmnt -rn -M /mnt/homestock-backups -o FSTYPE
```

The mount should appear as FUSE/rclone. Create the safety sentinel directly on the SMB share through rclone, then verify it appears in the mount:

```sh
rclone touch homestock:<SHARE>/.homestock-nas
mountpoint -q /mnt/homestock-backups \
  && test -e /mnt/homestock-backups/.homestock-nas \
  && echo "SMB mount and sentinel are ready"
```

Do not create `.homestock-nas` in the plain local mount-point directory if rclone is not mounted. The backup sync timer requires this marker before copying pending backups to the NAS.

If the NAS is another guest on this Proxmox host, configure its startup order to start before CT 107 when possible. Homestock starts without waiting for it; the backup sync timer retries after the SMB mount becomes available.

Verify that files are accessible as the app's UID:

```sh
stat -c 'owner=%u:%g mode=%a path=%n' /mnt/homestock-backups
setpriv --reuid=10001 --regid=10001 --clear-groups -- touch /mnt/homestock-backups/.write-test
setpriv --reuid=10001 --regid=10001 --clear-groups -- rm /mnt/homestock-backups/.write-test
```

Both write-test commands must succeed before configuring the backup sync timer. If they fail, check rclone's mount service, the UID/GID and FUSE permissions, and the SMB account's TrueNAS share/dataset ACL.

## 4. Install Docker Engine and Compose in the LXC

Run inside the Debian LXC as root. This uses Docker's Debian repository so the Docker Compose plugin is installed with the engine:

```sh
apt update
apt install -y ca-certificates curl
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian $(. /etc/os-release && echo "$VERSION_CODENAME") stable" \
  > /etc/apt/sources.list.d/docker.list
apt update
apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl start docker
docker run --rm hello-world
docker compose version
```

The `hello-world` command verifies that Docker can create and run a container in this LXC. If it fails with an overlay filesystem, AppArmor, or `operation not permitted` error, recheck **Nesting** and **Keyctl**. Do not switch the LXC to privileged as a shortcut; use a VM if the host/container combination cannot run Docker safely.

Leave Docker's normal automatic startup enabled. Compose uses `restart: unless-stopped`, and Homestock writes backups locally without requiring the NAS to be mounted:

```sh
systemctl enable docker.service
```

## 5. Create the Homestock deployment

Create a stable deployment directory and local data directory inside the LXC:

```sh
install -d -m 0755 /opt/homestock
install -d -o 10001 -g 10001 -m 0700 /srv/homestock/data
install -d -o 10001 -g 10001 -m 0700 /srv/homestock/certs
cd /opt/homestock
```

Create `/opt/homestock/.env` and set the release image tag to one that has been published by the GitHub Actions workflow. For example:

```dotenv
HOMESTOCK_IMAGE=ghcr.io/ntyndall1/homestock:1.2.3
HOMESTOCK_DATA_DIR=/srv/homestock/data
HOMESTOCK_CERT_DIR=/srv/homestock/certs
HOMESTOCK_BACKUP_DIR=/mnt/homestock-backups
HOMESTOCK_UID=10001
HOMESTOCK_GID=10001
HOMESTOCK_BACKUP_REQUIRE_MOUNT=true
HOMESTOCK_BACKUP_SENTINEL=.homestock-nas
HOMESTOCK_BACKUP_KEEP=48
HOMESTOCK_SYNC_PHOTOS=true
```

Place a PEM certificate chain in `/srv/homestock/certs/cert.pem` and its matching private key in `/srv/homestock/certs/key.pem`. Make them readable by UID `10001` inside the container. For example, as root:

```sh
install -o 10001 -g 10001 -m 0644 /path/to/your-cert.pem /srv/homestock/certs/cert.pem
install -o 10001 -g 10001 -m 0600 /path/to/your-private-key.pem /srv/homestock/certs/key.pem
```

Use those filenames for either a snakeoil or a trusted certificate. A self-signed snakeoil certificate encrypts traffic, but browsers warn that they cannot verify its identity unless you explicitly trust it. For a trusted certificate, ensure its SAN covers the hostname clients use.

Use the actual published release version instead of `1.2.3`. The Actions workflow publishes images after a GitHub Release is published with a semantic version tag such as `v1.2.3`; it publishes tags `1.2.3`, `1.2`, `1`, and a commit-SHA tag. Prefer the full version tag for a predictable deployment.

### Compose file

Create `/opt/homestock/docker-compose.yml` with this content. It is the same configuration as [`deploy/docker-compose.yml`](./docker-compose.yml), shown here so the deployment can be set up directly in the LXC:

```yaml
services:
  homestock:
    image: "${HOMESTOCK_IMAGE:-homestock:release}"
    container_name: homestock
    restart: unless-stopped
    init: true
    user: "${HOMESTOCK_UID:-10001}:${HOMESTOCK_GID:-10001}"
    ports:
      - "8443:8443"
    volumes:
      - "${HOMESTOCK_DATA_DIR:-./data}:/var/lib/homestock"
      - "${HOMESTOCK_CERT_DIR:-./certs}:/etc/homestock/certs:ro"
      - "./config.toml:/etc/homestock/config.toml:ro"
    read_only: true
    tmpfs:
      - "/tmp:rw,noexec,nosuid,size=16m"
    cap_drop:
      - ALL
    security_opt:
      - no-new-privileges:true
    stop_grace_period: 45s
```

The example Compose file publishes HTTPS port `8443` on the LXC's network interfaces. Restrict access with the Proxmox or network firewall if needed. You can instead bind only to the LXC's fixed LAN IP:

```yaml
ports:
  - "10.0.0.107:8443:8443"
```

Replace `10.0.0.107` with CT 107's actual static/reserved IP. Restrict access to this port with the Proxmox firewall or network firewall as appropriate.

### Homestock config file

Create `/opt/homestock/config.toml`:

```toml
bind = "0.0.0.0:8443"
tls_cert = "/etc/homestock/certs/cert.pem"
tls_key = "/etc/homestock/certs/key.pem"
data_dir = "/var/lib/homestock"
database_path = "/var/lib/homestock/homestock.sqlite3"
session_days = 30
cookie_secure = true

[backup]
idle_secs = 300
min_interval_secs = 1800
max_interval_secs = 86400
pending_keep = 3
```

These are paths **inside the app container**. Compose maps `/var/lib/homestock` to the LXC's local `/srv/homestock/data`, which contains the database, photos, and up to three complete pending backups. The cert directory is mounted read-only from `/srv/homestock/certs` on the LXC. Homestock serves HTTPS directly on port `8443`; the app container does not mount the NAS.

## 6. Pull and start Homestock

If the GHCR package is public, no registry login is needed. If it is private, log in inside the LXC as the account that runs Docker. Use a GitHub Personal Access Token (classic) with `read:packages`; do not put the token in `.env` or the Compose file:

```sh
printf '%s' 'YOUR_GITHUB_TOKEN' | docker login ghcr.io --username YOUR_GITHUB_USERNAME --password-stdin
```

Start the app and install its independent backup-sync timer:

```sh
cd /opt/homestock
docker compose config
systemctl enable --now docker.service
docker compose pull
docker compose up -d
./install-backup-sync.sh
docker compose ps
docker compose logs --tail=100 homestock
```

`docker compose config` should show the expected absolute LXC paths and GHCR image. Fix any missing environment variables or paths before starting. The sync timer runs as UID/GID `10001`; adjust its service unit if you use a different app UID. Check it with `systemctl status homestock-backup-sync.timer` and inspect runs with `journalctl -u homestock-backup-sync.service`.

Test the NAS mount is writable by UID `10001` using the `setpriv` write test in §3. The app can already be running; pending backups upload once the timer sees both the mount and sentinel.

The first visit displays the setup page. Create the household admin there. Do not add initial passwords to `config.toml`; the normal setup flow is preferred.

## 7. Verify HTTPS

Homestock serves HTTPS directly on port `8443`, using the certificate and key configured in `config.toml`. Visit `https://<LXC-IP>:8443/` from another device. Keep `cookie_secure = true`.

## 8. Verify backup behavior

The sync timer only writes to the NAS when the mount and sentinel are present. Check from inside the LXC:

```sh
test -e /mnt/homestock-backups/.homestock-nas && echo "NAS sentinel present"
```

Backups are dirty-and-idle driven: the backup worker captures a database-version baseline at app startup, so startup alone does not trigger a backup. By default Homestock waits until a subsequent database change, the app has been idle for five minutes, and at least 30 minutes have passed since the last backup. Changed databases are also backed up at least daily. A graceful shutdown creates a local backup immediately only if the database has changed since the last backup. The app keeps up to three local pending database backups. The separate timer uploads pending backups and photos when the NAS is available, then prunes NAS database backups to the newest 48.

The first setup may not produce a backup until a database change occurs and the idle/interval conditions are met (or the app shuts down gracefully while dirty). Check app logs for backup messages:

```sh
docker compose logs --since=24h homestock | grep 'backup:'
```

If the share is unavailable, Homestock continues running and stores pending backups locally; at most three are kept. The sync timer leaves them in place until both the mount and sentinel are present. Do not create `.homestock-nas` in the underlying empty mount-point directory. No container recreation is needed after rclone reconnects.

## 9. Upgrade without losing the database

Before an upgrade:

1. Confirm the latest backup exists on the NAS and is readable.
2. Keep the same `/srv/homestock/data` directory and `database_path` in `config.toml`.
3. The rclone mount and sentinel are not required for app startup; the backup sync timer will resume when they are available.
4. Change `HOMESTOCK_IMAGE` in `.env` to the new published full version tag.
5. Pull and recreate the service:

```sh
cd /opt/homestock
systemctl start docker
docker compose pull
docker compose up -d
docker compose ps
docker compose logs --tail=100 homestock
```

Do **not** run `docker compose down -v`, remove `/srv/homestock/data`, or change the data bind mount during a normal upgrade. The Compose bind mount persists the SQLite database independently of the replaceable container. When the app detects pending schema migrations, it creates and integrity-checks a local online backup beside the database before applying them. If that safety backup fails, startup stops before migrating.

## Troubleshooting checklist

| Symptom | Check |
|---|---|
| Docker fails to start in the LXC | Confirm the LXC is unprivileged and **Nesting**/**Keyctl** are enabled. If the host kernel still blocks Docker, use a VM rather than a privileged LXC. |
| Backup directory is not writable | Check `systemctl status rclone-homestock.service`, the mount's UID/GID (`10001:10001`), FUSE permissions, and the TrueNAS SMB ACL. Run the `setpriv` write test in §3. |
| No NAS backups appear | Confirm `mountpoint -q /mnt/homestock-backups`, the sentinel, `homestock-backup-sync.timer`, rclone service logs (`journalctl -u rclone-homestock.service`), and sync logs (`journalctl -u homestock-backup-sync.service`). |
| NAS guest starts later than Homestock | Homestock should already be available. The timer uploads pending backups after rclone mounts the share and the sentinel appears. |
| Docker/Homestock does not start after reboot | Confirm CT 107 is set to start at boot, then check `systemctl status docker.service` and `docker compose ps`. NAS availability is not required for app startup. |
| `unauthorized` when pulling GHCR | Make the package public, or run `docker login ghcr.io` with a token that can read packages. |
| App is not reachable | Confirm `docker compose ps`, the Compose port mapping to CT 107's fixed IP on port `8443`, DNS, firewall rules, and certificate/key paths in `config.toml`. |

## References

- [Docker releases and deployment overview](./README.md)
- [Compose file](./docker-compose.yml)
- [Homestock configuration example](./config.example.toml)
- [GHCR publishing workflow](../.github/workflows/publish-container.yml)
- [Proxmox VE documentation](https://pve.proxmox.com/pve-docs/)
- [Docker Engine on Debian](https://docs.docker.com/engine/install/debian/)
- [rclone SMB backend](https://rclone.org/smb/)
- [rclone mount](https://rclone.org/commands/rclone_mount/)
