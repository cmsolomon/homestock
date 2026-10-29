# Running Homestock in a Proxmox LXC

A small unprivileged LXC container is a good home for Homestock on Proxmox VE. Homestock runs in
Docker inside the container. This guide covers creating the container with the features Docker
needs; after that, the [Quickstart](QUICKSTART.md) does the rest.

## 1. Create the container

In the Proxmox web interface:

1. Download a **Debian 13 (Trixie)** container template. On your storage, open **CT Templates →
   Templates**.
2. Click **Create CT**:
   - **General:** a CT ID and hostname such as `homestock`. Leave **Unprivileged container**
     ticked.
   - **Template:** Debian 13.
   - **Disks:** 8 GB or more on local storage, plus room for your photos. Keep the root disk
     off network storage; the database needs a local disk.
   - **CPU:** 1–2 cores. **Memory:** 1024 MB.
   - **Network:** a static address, or a DHCP reservation for the container's MAC address.
3. Don't start it yet.

## 2. Turn on the features it needs

Select the container, then **Options → Features → Edit**:

| Feature | When you need it |
|---|---|
| **Nesting** | **Always.** Docker needs it to run containers. |
| **FUSE** | Only if Homestock will mount your NAS share itself with rclone (`homestock-nas setup rclone`). Not needed for the other NAS option below. |
| **keyctl** | Only if Docker fails to start containers with a keyring or permission error. Most current setups don't need it. |

Or, from the Proxmox host's shell:

```sh
pct set <CTID> --features nesting=1          # add ,fuse=1 or ,keyctl=1 as needed
```

`--features` replaces the whole list, so include every feature you want each time.

Also enable **Options → Start at boot**, so Homestock comes back after the host restarts.

## 3. Install Homestock

Start the container and open its **Console**, or run `pct enter <CTID>` on the host. You're root
in the container, so `sudo` isn't needed:

```sh
apt-get update && apt-get install -y curl
curl -fsSLO https://github.com/ntyndall1/homestock/releases/latest/download/install.sh
bash install.sh
```

Then follow the [Quickstart](QUICKSTART.md#2-answer-the-questions) from step 2. The installer
notices it's in an LXC, and points to the feature to turn on if Docker can't start.

## Getting backups onto a NAS

There are two ways to get Homestock's backups from inside the container to your NAS.

**Option A: rclone inside the container.** This is the simplest.
1. Turn on **FUSE** (step 2).
2. Run `homestock-nas setup rclone` and answer the questions.

If you choose this while FUSE is still off, the installer saves your settings and finishes with
the steps to enable it. After that:
1. restart the container (`pct reboot <CTID>`);
2. run `homestock-nas confirm` inside it to check the share and start copying.

**Option B: mount on the Proxmox host and pass it in.** This needs no FUSE in the container, and
suits you if the host already mounts the NAS.
1. Mount the share on the host. For an unprivileged container, give it to host UID/GID
   **110001**, which appears as Homestock's UID 10001 inside the container. For CIFS, that means
   adding `uid=110001,gid=110001` to the mount options.
2. Bind it into the container:
   ```sh
   pct set <CTID> -mp0 /mnt/nas/homestock,mp=/mnt/nas-homestock
   ```
3. Inside the container, run `homestock-nas setup mount /mnt/nas-homestock`.

Either way, `homestock-doctor` checks the whole chain. See
[Administration](ADMINISTRATION.md#copying-backups-to-a-nas) for how the copying works.

## Firewall

If Homestock sits behind a reverse proxy, allow only the proxy to reach it. In the container's
**Firewall** tab:
1. add a rule accepting **TCP 8443** from the proxy's address;
2. under **Firewall → Options**, set the input policy to **DROP** and enable the firewall.

The datacenter-level firewall must also be enabled for container rules to apply. Add rules for
anything else you reach the container with, such as SSH.

## Troubleshooting

| Problem | What to check |
|---|---|
| Docker won't start, or containers fail with "operation not permitted" | **Nesting** is on, and the container was restarted after turning it on. If the error mentions keyrings, also turn on **keyctl**. |
| `homestock-doctor` says FUSE is not available | Turn on **FUSE** (step 2), then `pct reboot <CTID>`. |
| The NAS share is mounted but not writable | For option B, the host mount must be owned by UID/GID 110001. Inside the container, `homestock-nas setup mount` explains what it found. |
| Homestock doesn't come back after a reboot | **Start at boot** is on, then run `homestock-doctor` inside the container. Homestock doesn't wait for the NAS. |
| Pulling the image fails | The container can reach `ghcr.io` (DNS and outbound HTTPS). |

Don't make the container privileged to work around a problem. If Docker can't be made to work in
an unprivileged LXC, a small Debian VM works the same way with the same installer.
