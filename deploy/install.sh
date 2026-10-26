#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required to install Homestock." >&2
  exit 1
fi
docker compose version >/dev/null
if command -v systemctl >/dev/null 2>&1; then
  systemctl enable --now docker.service
fi

for file in config.example.toml docker-compose.yml; do
  if [[ ! -f "$file" ]]; then
    echo "Release file is missing: $file" >&2
    exit 1
  fi
done

if [[ ! -e config.toml ]]; then
  cp config.example.toml config.toml
  echo "Created config.toml from config.example.toml; review it before exposing the app."
fi
if [[ ! -e .env ]]; then
  cp .env.example .env
fi

mkdir -p data backups certs
if [[ -f homestock-image.tar ]]; then
  docker load --input homestock-image.tar
else
  docker compose pull
fi
docker compose up -d --no-build

if [[ -f install-backup-sync.sh ]]; then
  ./install-backup-sync.sh
fi

echo "Homestock is running at https://127.0.0.1:8443. Persistent data is in the configured data directory."
