#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if [[ "$(id -u)" -ne 0 ]]; then
  echo "Run as root to install the Homestock backup sync timer." >&2
  exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemd is required to install the Homestock backup sync timer." >&2
  exit 1
fi

chmod 0755 /opt/homestock/sync-homestock-backups.sh
install -m 0644 homestock-backup-sync.service /etc/systemd/system/homestock-backup-sync.service
install -m 0644 homestock-backup-sync.timer /etc/systemd/system/homestock-backup-sync.timer
systemctl daemon-reload
systemctl enable --now homestock-backup-sync.timer
