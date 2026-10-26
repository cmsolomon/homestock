# Administration

Everything here is done on the server, as root or with `sudo`. The installer adds four commands:

| Command | What it's for |
|---|---|
| `homestock-doctor` | Check that Homestock, its backups and the NAS copy are working. |
| `homestock-upgrade` | Check for and install new releases. |
| `homestock-admin` | Recover accounts: reset a password, create an admin. |
| `homestock-nas` | Set up, confirm or turn off the copy of backups to a NAS. |

- [Checking everything is working](#checking-everything-is-working)
- [Upgrading](#upgrading)
- [Account recovery](#account-recovery)
- [Backups](#backups)
- [Copying backups to a NAS](#copying-backups-to-a-nas)
- [Restoring a backup](#restoring-a-backup)
- [Files and settings](#files-and-settings)
- [Uninstalling](#uninstalling)

## Checking everything is working

```sh
sudo homestock-doctor
```

It checks:
- Docker and the container;
- that Homestock answers its health check;
- the database version and the certificate's expiry date;
- free disk space and backups waiting to be copied;
- if set up, every part of the NAS copy.

Each problem is marked `FAIL` with a `→` line saying what to do. The command exits with status 0
when nothing failed, so it can also be used from monitoring scripts.

## Upgrading

```sh
sudo homestock-upgrade --check    # installed and latest versions, and a link to the release notes
sudo homestock-upgrade            # upgrade to the latest release
```

`~/homestock_upgrade.sh`, in the home directory of whoever ran the installer, is a shortcut to the
same command. Use `--version 1.2.3` to install a particular release, and `--yes` to skip the
confirmation, for example from cron.

An upgrade is verified the same way as an install: the release and its image must carry this
project's signatures. Homestock is only stopped once the new release has been downloaded and has
checked that it can read your database. Then the upgrade:

1. **stops Homestock** and writes a checked copy of the database to
   `<data>/pre-upgrade/<date>-v<old version>/`, along with the settings files;
2. **starts the new version**, which updates the database format if the release needs it;
3. **waits for the new version to report healthy**. If it doesn't, the upgrade puts the previous
   version back automatically, with the database as it was before the upgrade, and says where
   the new version's log was saved.

The three most recent pre-upgrade copies are kept. Each folder has a `manifest` file describing
what it holds and how to go back by hand.

## Account recovery

If someone has forgotten their password, or the only admin is locked out:

```sh
sudo homestock-admin list-users              # everyone, their role, and whether a password change is due
sudo homestock-admin reset-password Alex     # prints a temporary password
sudo homestock-admin create-admin Morgan     # a new admin account, with a temporary password
sudo homestock-admin make-admin Sam          # give an existing member admin rights
```

A temporary password must be changed at the next sign-in. Resetting a password also signs that
person out everywhere.

These commands work whether Homestock is running or stopped. Each use is recorded in the system
log (`journalctl -t homestock-admin`), without the passwords.

## Backups

Homestock backs up its own database while it's running. It doesn't interrupt anyone, because it
waits for the house to be quiet:

- a backup is made after **5 minutes with no activity**, if anything changed since the last one;
- at most once every **30 minutes**, and at least **once a day** while things are changing.

Each backup is a complete, checked copy of the database. It goes to `<data>/staging/`, where the
three newest are kept until they are copied somewhere safer. Photos are separate files in
`<data>/photos/`.

When an upgrade changes the database format, Homestock also saves a copy next to the database
first (`homestock.sqlite3.pre-migration-…`).

These timings can be changed in the `[backup]` section of `config.toml`.

Backups on the same disk as the database don't protect you from that disk failing, so copy them
to a NAS, or back up the whole machine. For example, Proxmox's own backups of the LXC capture
everything.

## Copying backups to a NAS

Homestock can copy its backups, and your photos, to a folder on a NAS every five minutes. There
are two ways to connect the NAS:

```sh
sudo homestock-nas setup mount /mnt/nas/homestock   # a folder you have already mounted from the NAS
sudo homestock-nas setup rclone                     # let Homestock mount an SMB (Windows/Samba) share
```

**An existing mount** can be anything that appears as a folder on this machine: an SMB/CIFS or
NFS mount in `/etc/fstab`, your own rclone mount, or a folder bind-mounted into an LXC by
Proxmox. Homestock's user (UID 10001) must be able to write to it. If it can't, setup says how to
fix that for each kind of mount.

**rclone** sets everything up for you:
- it installs `rclone` and `fuse3`;
- it asks for the NAS address, share, folder and login, then checks the login over the network;
- it mounts the share at boot with a systemd service.

It needs FUSE. In a Proxmox LXC, FUSE is a container feature that is off by default. If it's
missing, setup still saves your settings, and both setup and `homestock-doctor` explain how to
turn FUSE on.

### Confirming the share

Backups are only copied once you've confirmed the share. Confirming writes a small marker file
(`.homestock-nas`) on it. If the NAS isn't mounted, the mount point is just an empty local
folder. Without the marker, Homestock can't tell the two apart, and could quietly fill up the
local disk thinking it was the NAS.

Setup asks you to confirm straight away when the share is mounted. Otherwise, for example
after you enable FUSE and restart, run:

```sh
sudo homestock-nas confirm
```

It shows you what's mounted there and what's in it, and writes the marker only when you say yes.

### Day to day

```sh
sudo homestock-nas sync      # copy anything waiting now, and show what was copied
sudo homestock-nas disable   # stop copying; nothing on the NAS is deleted
```

On the NAS, the 48 newest database backups are kept. Photos are copied once and never deleted
there. Both settings can be changed in `install.conf`: `HOMESTOCK_BACKUP_KEEP` and
`HOMESTOCK_SYNC_PHOTOS`.

## Restoring a backup

Any backup file, whether from `staging/`, the NAS or a `pre-upgrade/` folder, is a complete
database. To restore one:

```sh
cd /opt/homestock
sudo docker compose stop
sudo install -m 0600 -o 10001 -g 10001 /path/to/homestock-<time>.sqlite3 /srv/homestock/data/homestock.sqlite3
sudo rm -f /srv/homestock/data/homestock.sqlite3-wal /srv/homestock/data/homestock.sqlite3-shm
sudo docker compose start
```

Use your own data directory if you changed it. Removing the `-wal` and `-shm` files matters:
they belong to the old database. A backup from an older release is upgraded automatically when
Homestock starts. One from a newer release than the one installed is refused, so upgrade first.

## Files and settings

| Path | What it is |
|---|---|
| `/opt/homestock/install.conf` | The installer's settings: folders, port, hostname and NAS copy. |
| `/opt/homestock/config.toml` | Homestock's own settings: sessions, backups, `trusted_proxies`. Yours to edit; installs and upgrades never overwrite it. |
| `/opt/homestock/.env`, `docker-compose.yml`, `release.env` | Generated by the installer. Edits are replaced. |
| `/opt/homestock/rclone.conf` | The NAS login, when rclone is used (root-only). |
| `/srv/homestock/data/` | The database, `photos/`, `staging/` (backups waiting) and `pre-upgrade/`. |
| `/srv/homestock/certs/` | `cert.pem` and `key.pem`. |

After editing `config.toml`, restart Homestock:

```sh
sudo docker restart homestock
```

After editing `install.conf`, apply it by re-running the current release:

```sh
sudo homestock-upgrade --version "$(sed -n 's/^HOMESTOCK_VERSION=//p' /opt/homestock/release.env)"
```

The logs are in Docker (`sudo docker logs homestock`) and, for the NAS copy, in the journal
(`journalctl -u homestock-backup-sync -u homestock-nas-mount`).

## Uninstalling

There is no uninstall command yet. To remove Homestock:

```sh
sudo homestock-nas disable                      # if the NAS copy was set up
sudo docker compose -f /opt/homestock/docker-compose.yml down
sudo rm -f /usr/local/bin/homestock-{upgrade,admin,nas,doctor} ~/homestock_upgrade.sh
sudo rm -rf /opt/homestock
```

Then remove `/srv/homestock` once you're sure you have the data you need. It holds your database
and photos.
