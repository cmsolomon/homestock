#!/usr/bin/env bash
# Regenerate the documentation screenshots in docs/images/.
#
#   scripts/screenshots/run.sh [IMAGE]
#
# Builds the image from this checkout (or uses IMAGE), runs it with a throwaway database,
# creates the admin through the setup screen, loads the demo household (demo-data.py), and
# captures each screen with headless Firefox (shoot.mjs). Needs docker, node with npm,
# python3, curl and firefox. PORT (default 18080) is the local port used while it runs.
set -euo pipefail

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$HERE/../.." && pwd)"
PORT="${PORT:-18080}"
NAME=homestock-screenshots
BASE="http://127.0.0.1:$PORT"

for tool in docker node npm python3 curl firefox; do
  command -v "$tool" >/dev/null || { echo "screenshots: $tool is required" >&2; exit 1; }
done

image="${1:-}"
if [[ -z "$image" ]]; then
  image=homestock:screenshots
  echo "Building $image from this checkout"
  docker build -q -t "$image" "$ROOT" >/dev/null
fi

if [[ ! -d "$HERE/node_modules" ]]; then
  (cd "$HERE" && npm ci --silent)
fi

work="$(mktemp -d)"
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf -- "$work"
}
trap cleanup EXIT

mkdir "$work/data"
cat > "$work/config.toml" <<'EOF'
bind = "0.0.0.0:8443"
data_dir = "/var/lib/homestock"
cookie_secure = false
[backup]
EOF

docker run -d --name "$NAME" --user "$(id -u):$(id -g)" -p "127.0.0.1:$PORT:8443" \
  -v "$work/data:/var/lib/homestock" -v "$work/config.toml:/etc/homestock/config.toml:ro" \
  "$image" >/dev/null
for _ in $(seq 1 30); do
  curl -fs -o /dev/null "$BASE/healthz" && break
  sleep 1
done

curl -fs -o /dev/null -H "Origin: $BASE" \
  -d 'name=Alex&password=demo-password-1&confirm=demo-password-1' "$BASE/setup"
python3 -I "$HERE/demo-data.py" "$work/data/homestock.sqlite3"

echo "Capturing into docs/images/"
node "$HERE/shoot.mjs" "$BASE" "$ROOT/docs/images"
