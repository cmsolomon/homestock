# Quickstart

This guide takes you from a fresh Linux machine to signing in to Homestock. It takes about ten
minutes, and the steps are the same on x86-64 and ARM64. The installer picks the right image for
your machine.

Installing into a Proxmox LXC? Create the container first with
[Running Homestock in a Proxmox LXC](PROXMOX-LXC.md), then come back here.

## What you need

| | |
|---|---|
| **Machine** | Any 64-bit **x86-64 (amd64)** or **ARM64 (aarch64)** Linux system: a Proxmox LXC or VM, a mini PC, or a Raspberry Pi 4 or 5 running a 64-bit OS. 32-bit systems are not supported. |
| **Operating system** | **Debian 12 or 13, or Ubuntu 22.04 or later**, where the installer can install Docker for you. On other distributions, install [Docker Engine](https://docs.docker.com/engine/install/) with the Compose plugin first. The optional NAS copy of backups also needs systemd. |
| **Resources** | 1 CPU core and 1 GB of RAM are plenty. Allow a few GB of disk for the system and image, plus room for your photos. |
| **Access** | `root`, or a user who can use `sudo`. |
| **Network** | A fixed address on your LAN (a static IP or a DHCP reservation). During installation and upgrades the machine needs to reach `github.com`, `ghcr.io` and `download.docker.com`. Homestock itself makes no outside connections. |

Check your architecture with `uname -m`. It should say `x86_64` or `aarch64`.

## 1. Download and run the installer

```sh
sudo apt-get update && sudo apt-get install -y curl    # if curl is missing
curl -fsSLO https://github.com/cmsolomon/homestock/releases/latest/download/install.sh
sudo bash install.sh
```

`install.sh` is a small bootstrap. Before running anything, it:

1. finds the latest release;
2. downloads [cosign](https://docs.sigstore.dev/), a signature-checking tool, and checks it
   against a checksum built into the script;
3. checks that the release was signed by this project's release workflow;
4. runs the installer that came with the release, which also checks the container image's
   signature before starting it.

If any check fails, it stops without changing anything.

## 2. Answer the questions

The installer asks a few questions. Press Enter to accept the default shown in brackets.

| Question | What to enter |
|---|---|
| Hostname clients will use | The name you'll type in the browser, e.g. `homestock.home.arpa`. It goes into the certificate. |
| HTTPS port on this host | `8443` unless something else uses it. |
| Directory for the database and photos | Default `/srv/homestock/data`. Use fast local disk, not a network share. |
| Directory for cert.pem and key.pem | Default `/srv/homestock/certs`. |
| Reverse proxy addresses | Leave blank, unless a TLS-offloading proxy will sit in front of Homestock (see [HAProxy](HAPROXY.md)). |
| Copy backups to a NAS? | `none` for now is fine. You can set it up any time; see [Administration](ADMINISTRATION.md#copying-backups-to-a-nas). |

If Docker isn't installed, the installer offers to install it from Docker's own repository.

To install without questions, for example from a script, pass the answers as options and add
`--yes`:

```sh
sudo bash install.sh --yes --hostname homestock.home.arpa
```

`sudo bash install.sh --help` lists every option.

## 3. Check it's running

When it finishes, the installer prints:
- the address to open;
- where its files are;
- a health check of the whole install, which should end with **All checks passed**.

If a check needs attention, the line marked `→` beneath it says what to do. You can run the same
check any time:

```sh
sudo homestock-doctor
```

## 4. Sign in for the first time

Open `https://<hostname>:8443/` in your browser.

Homestock creates its own **self-signed certificate**, so the first visit shows a browser warning.
The connection is still encrypted, but the browser has no way to know the certificate is yours.
You can:

- **accept the warning** on each device: fine for a quick start;
- **put Homestock behind a reverse proxy** with a trusted certificate: see [HAProxy](HAPROXY.md),
  which is the best option if you already run one;
- **use your own certificate:** replace `cert.pem` and `key.pem` in the certificates directory,
  then run `sudo docker restart homestock`.

The first page you see is **setup**. Create the household admin account there. Then open
**Household** to add everyone else. Each person gets a temporary password, which they change at
first sign-in.

## 5. Start using it

- **Add rooms, then the furniture and boxes in them.** On **Locations**, use **Add room**. Inside
  any location, use **Add box inside**.
- **Fill a box with Quick add.** Open the box and choose **Add item**. The box stays locked while
  you add one thing after another. On a phone, the round **+** button opens Quick add for the
  last box you used.
- **Set a low-stock alert** on things you run out of, such as batteries or coffee, and they'll
  appear on the shopping list when they run low.

## Next steps

- **Upgrades:** `sudo homestock-upgrade --check` tells you when there's a new release, and
  `sudo homestock-upgrade` installs it safely. `~/homestock_upgrade.sh` is a shortcut to the same
  command.
- **Forgotten passwords:** anyone with access to the server can reset a password or create a new
  admin with `sudo homestock-admin`.
- **Backups:** Homestock backs up its database automatically. Copying the backups to a NAS keeps
  them safe if the machine dies.

All of these are covered in [Administration](ADMINISTRATION.md).
