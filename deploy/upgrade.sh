#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if ! command -v docker >/dev/null 2>&1; then
  echo "Docker is required to upgrade Homestock." >&2
  exit 1
fi
docker compose version >/dev/null

for file in homestock-image.tar config.toml .env docker-compose.yml; do
  if [[ ! -f "$file" ]]; then
    echo "Refusing upgrade: required existing installation file is missing: $file" >&2
    exit 1
  fi
done

DATA_DIR="$(awk -F= '$1 == "HOMESTOCK_DATA_DIR" { print substr($0, index($0, "=") + 1); exit }' .env)"
DATA_DIR="${DATA_DIR:-./data}"
if [[ "$DATA_DIR" != /* ]]; then
  DATA_DIR="$PWD/$DATA_DIR"
fi
if [[ ! -d "$DATA_DIR" ]]; then
  echo "Refusing upgrade: configured persistent data directory does not exist: $DATA_DIR" >&2
  exit 1
fi

DATABASE_PATH="$(awk -F'"' '/^[[:space:]]*database_path[[:space:]]*=/ { print $2; exit }' config.toml)"
DATABASE_PATH="${DATABASE_PATH:-/var/lib/homestock/homestock.sqlite3}"
if [[ "$DATABASE_PATH" != /var/lib/homestock/* ]]; then
  echo "Refusing upgrade: database_path must remain under /var/lib/homestock." >&2
  exit 1
fi
DATABASE_RELATIVE_PATH="${DATABASE_PATH#/var/lib/homestock/}"
if [[ -z "$DATABASE_RELATIVE_PATH" || "$DATABASE_RELATIVE_PATH" == .. || "$DATABASE_RELATIVE_PATH" == ../* || "$DATABASE_RELATIVE_PATH" == */../* || "$DATABASE_RELATIVE_PATH" == */.. ]]; then
  echo "Refusing upgrade: database_path is not a safe path under /var/lib/homestock." >&2
  exit 1
fi
if [[ ! -f "$DATA_DIR/$DATABASE_RELATIVE_PATH" ]]; then
  echo "Refusing upgrade: the configured SQLite database was not found at $DATA_DIR/$DATABASE_RELATIVE_PATH." >&2
  echo "Check HOMESTOCK_DATA_DIR and database_path; no container was replaced." >&2
  exit 1
fi

docker load --input homestock-image.tar
docker compose up -d --no-build

echo "Homestock upgraded. The existing config and Compose bind-mounted data were left in place."
