#!/usr/bin/env bash
# Homestock installer, shipped inside each release's deployment bundle and run by install.sh.
# Safe to re-run: it creates what is missing and never overwrites config.toml, certificates
# or data. Settings are saved in <install-dir>/install.conf; .env is regenerated from it.
set -euo pipefail

BUNDLE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
APP_UID=10001
APP_GID=10001

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Installs Homestock with Docker Compose, or brings an existing install to this release: it
stops Homestock, backs up and checks the database, starts the new version, and goes back to
the old one if the new one doesn't come up. Without --yes it asks for anything not given as
an option, and before upgrading.

Options:
  --hostname NAME       Name clients use for Homestock (for the certificate and messages)
  --trusted-proxy IP    Address of a TLS-offloading reverse proxy, e.g. HAProxy on OPNsense;
                        repeat for several. Its X-Forwarded-For is trusted for client addresses.
  --port PORT           Host port for Homestock's HTTPS (default 8443)
  --install-dir DIR     Deployment files (default /opt/homestock)
  --data-dir DIR        Database and photos (default /srv/homestock/data)
  --certs-dir DIR       cert.pem and key.pem (default /srv/homestock/certs)
  --nas-dir DIR          Copy backups to this folder, already mounted from a NAS
  --nas-rclone          Copy backups to an SMB share mounted with rclone (asks for the login)
  --image REF           Use this image instead of the release's (for testing)
  -y, --yes             Don't ask: accept defaults and install Docker if it is missing
  -h, --help            Show this help

install.sh and homestock-upgrade also take:
  --version X.Y.Z       Install this release instead of the latest
  --check               Show the installed and latest versions, and change nothing
EOF
}

say() { printf '\033[1mhomestock:\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mhomestock: warning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mhomestock: error:\033[0m %s\n' "$*" >&2; exit 1; }

# --- options -------------------------------------------------------------------------------
opt_hostname="" opt_port="" opt_image="" assume_yes=false proxies_given=false opt_nas=""
install_dir=/opt/homestock opt_data_dir="" opt_certs_dir=""
opt_proxies=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --hostname) opt_hostname="${2:?--hostname needs a value}"; shift 2 ;;
    --trusted-proxy) opt_proxies+=("${2:?--trusted-proxy needs a value}"); proxies_given=true; shift 2 ;;
    --port) opt_port="${2:?--port needs a value}"; shift 2 ;;
    --install-dir) install_dir="${2:?--install-dir needs a value}"; shift 2 ;;
    --data-dir) opt_data_dir="${2:?--data-dir needs a value}"; shift 2 ;;
    --certs-dir) opt_certs_dir="${2:?--certs-dir needs a value}"; shift 2 ;;
    --nas-dir) opt_nas="mount ${2:?--nas-dir needs a value}"; shift 2 ;;
    --nas-rclone) opt_nas=rclone; shift ;;
    --image) opt_image="${2:?--image needs a value}"; shift 2 ;;
    -y|--yes) assume_yes=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown option $1" ;;
  esac
done
[[ "$(id -u)" -eq 0 ]] || die "run as root"

# --- validation: every value written to .env, install.conf or config.toml passes one of these
valid_path() { [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ && "$1" != *..* ]]; }
valid_hostname() { [[ "$1" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]]; }
valid_ip() { [[ "$1" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ || ( "$1" =~ ^[0-9A-Fa-f:.]+$ && "$1" == *:* ) ]]; }
valid_port() { [[ "$1" =~ ^[0-9]+$ ]] && (( $1 >= 1 && $1 <= 65535 )); }
valid_id() { [[ "$1" =~ ^[0-9]+$ ]]; }

# KEY=VALUE files are parsed, never sourced: only the given key, value taken literally.
kv_get() {
  if [[ -f "$1" ]]; then sed -n "s/^$2=//p" "$1" | tail -n 1; fi
}

ask() { # ask VAR "Question" default
  local answer
  if $assume_yes || [[ ! -r /dev/tty ]]; then printf -v "$1" '%s' "$3"; return; fi
  read -r -p "$2 [$3]: " answer </dev/tty || true
  printf -v "$1" '%s' "${answer:-$3}"
}

confirm() { # confirm "Question" → 0 for yes
  local answer
  $assume_yes && return 0
  [[ -r /dev/tty ]] || return 1
  read -r -p "$1 [Y/n]: " answer </dev/tty || true
  [[ -z "$answer" || "$answer" =~ ^[Yy] ]]
}

# --- the release being installed -----------------------------------------------------------
release_version="$(kv_get "$BUNDLE_DIR/release.env" HOMESTOCK_VERSION)"
image="${opt_image:-$(kv_get "$BUNDLE_DIR/release.env" HOMESTOCK_IMAGE)}"
[[ -n "$image" ]] || die "no image to install: this bundle has no release.env; pass --image to test"
[[ "$image" =~ ^[A-Za-z0-9._/:@-]+$ ]] || die "invalid image reference: $image"
release_version="${release_version:-unreleased}"

# --- prerequisites -------------------------------------------------------------------------
apt_install() {
  command -v apt-get >/dev/null || die "please install: $*"
  say "installing $*"
  DEBIAN_FRONTEND=noninteractive apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >/dev/null
}

missing=()
command -v curl >/dev/null || missing+=(curl)
command -v openssl >/dev/null || missing+=(openssl)
[[ -f /etc/ssl/certs/ca-certificates.crt ]] || missing+=(ca-certificates)
[[ ${#missing[@]} -eq 0 ]] || apt_install "${missing[@]}"

install_docker() {
  # Docker's own apt repository, as in https://docs.docker.com/engine/install/ — the distro
  # packages don't reliably include Compose v2.
  local distro codename
  # shellcheck source=/dev/null
  distro="$(. /etc/os-release && echo "${ID:-}")"
  # shellcheck source=/dev/null
  codename="$(. /etc/os-release && echo "${VERSION_CODENAME:-}")"
  if [[ "$distro" != debian && "$distro" != ubuntu ]] || [[ -z "$codename" ]]; then
    die "install Docker Engine and the Compose plugin for this system, then re-run (https://docs.docker.com/engine/install/)"
  fi
  say "installing Docker Engine from download.docker.com ($distro $codename)"
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL "https://download.docker.com/linux/$distro/gpg" -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/$distro $codename stable" \
    > /etc/apt/sources.list.d/docker.list
  apt_install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
}

if ! command -v docker >/dev/null || ! docker compose version >/dev/null 2>&1; then
  confirm "Docker Engine with the Compose plugin is required. Install it now?" \
    || die "install Docker Engine and the Compose plugin, then re-run"
  install_docker
fi
if command -v systemctl >/dev/null && [[ -d /run/systemd/system ]]; then
  systemctl enable --now docker.service >/dev/null 2>&1 || true
fi
docker info >/dev/null 2>&1 || die "Docker is installed but not running (try: systemctl start docker)"

in_lxc=false
if command -v systemd-detect-virt >/dev/null && [[ "$(systemd-detect-virt --container 2>/dev/null)" == lxc ]]; then
  in_lxc=true
fi

# --- settings: install.conf, adopted from a hand-made .env, or asked for -------------------
valid_path "$install_dir" || die "the install directory must be an absolute path without spaces: $install_dir"
install -d -m 0755 "$install_dir"
conf="$install_dir/install.conf"
legacy_env="$install_dir/.env"
installed_version="$(kv_get "$install_dir/release.env" HOMESTOCK_VERSION)"
fresh=false

if [[ -f "$conf" ]]; then
  say "existing install in $install_dir (Homestock ${installed_version:-unknown version})"
  if [[ -n "$opt_hostname$opt_port$opt_data_dir$opt_certs_dir" ]]; then
    warn "ignoring --hostname/--port/--data-dir/--certs-dir: this install's settings are in $conf"
  fi
  $proxies_given && warn "ignoring --trusted-proxy: set trusted_proxies in $install_dir/config.toml"
  [[ -z "$opt_nas" ]] || warn "ignoring --nas-dir/--nas-rclone: use sudo homestock-nas setup"
  opt_nas=""
elif [[ -f "$legacy_env" ]]; then
  say "adopting the existing install in $install_dir (its .env and config.toml are kept)"
  resolve() { local v; v="$(kv_get "$legacy_env" "$1")"; v="${v:-$2}"; [[ "$v" == /* ]] || v="$install_dir/${v#./}"; echo "$v"; }
  {
    echo "HOMESTOCK_DATA_DIR=$(resolve HOMESTOCK_DATA_DIR ./data)"
    echo "HOMESTOCK_CERT_DIR=$(resolve HOMESTOCK_CERT_DIR ./certs)"
    echo "HOMESTOCK_PORT=8443"
    echo "HOMESTOCK_HOSTNAME=$(hostname -f 2>/dev/null || hostname)"
    legacy_uid="$(kv_get "$legacy_env" HOMESTOCK_UID)"
    legacy_gid="$(kv_get "$legacy_env" HOMESTOCK_GID)"
    echo "HOMESTOCK_UID=${legacy_uid:-$APP_UID}"
    echo "HOMESTOCK_GID=${legacy_gid:-$APP_GID}"
    # A hand-made backup sync keeps going, as a NAS folder mounted by whatever mounted it.
    if [[ -n "$(kv_get "$legacy_env" HOMESTOCK_BACKUP_DIR)" ]]; then
      echo "HOMESTOCK_NAS_MODE=mount"
      echo "HOMESTOCK_BACKUP_DIR=$(resolve HOMESTOCK_BACKUP_DIR ./backups)"
    fi
    for key in HOMESTOCK_BACKUP_SENTINEL HOMESTOCK_BACKUP_KEEP HOMESTOCK_BACKUP_REQUIRE_MOUNT HOMESTOCK_SYNC_PHOTOS; do
      value="$(kv_get "$legacy_env" "$key")"
      [[ -z "$value" ]] || echo "$key=$value"
    done
  } > "$conf.new"
  cp -p "$legacy_env" "$legacy_env.before-installer"
else
  fresh=true
  say "installing Homestock $release_version"
  host_name="" port="" data_dir="" certs_dir="" proxy_answer=""
  ask host_name "Hostname clients will use" "${opt_hostname:-$(hostname -f 2>/dev/null || hostname)}"
  ask port "HTTPS port on this host" "${opt_port:-8443}"
  ask data_dir "Directory for the database and photos" "${opt_data_dir:-/srv/homestock/data}"
  ask certs_dir "Directory for cert.pem and key.pem" "${opt_certs_dir:-/srv/homestock/certs}"
  if ! $proxies_given && ! $assume_yes && [[ -r /dev/tty ]]; then
    ask proxy_answer "Reverse proxy address(es) doing TLS offloading, space-separated (blank for none)" ""
    read -r -a opt_proxies <<<"$proxy_answer"
  fi
  if [[ -z "$opt_nas" ]] && ! $assume_yes && [[ -r /dev/tty ]]; then
    nas_answer="" nas_dir=""
    echo "Homestock backs up its database to $data_dir/staging. It can copy the backups to a NAS:"
    echo "  none, mount (a folder already mounted from the NAS) or rclone (mount an SMB share for you)"
    ask nas_answer "Copy backups to a NAS?" none
    case "$nas_answer" in
      none|n|"") ;;
      mount|m) ask nas_dir "Mounted NAS folder" ""; opt_nas="mount $nas_dir" ;;
      rclone|r) opt_nas=rclone ;;
      *) die "answer none, mount or rclone" ;;
    esac
  fi
  valid_hostname "$host_name" || die "invalid hostname: $host_name"
  valid_port "$port" || die "invalid port: $port"
  valid_path "$data_dir" || die "the data directory must be an absolute path without spaces: $data_dir"
  valid_path "$certs_dir" || die "the certificate directory must be an absolute path without spaces: $certs_dir"
  for proxy in "${opt_proxies[@]}"; do valid_ip "$proxy" || die "not an IP address: $proxy"; done
  {
    echo "HOMESTOCK_DATA_DIR=$data_dir"
    echo "HOMESTOCK_CERT_DIR=$certs_dir"
    echo "HOMESTOCK_PORT=$port"
    echo "HOMESTOCK_HOSTNAME=$host_name"
    echo "HOMESTOCK_UID=$APP_UID"
    echo "HOMESTOCK_GID=$APP_GID"
  } > "$conf.new"
fi

if [[ -f "$conf.new" ]]; then
  printf '# Homestock installer settings. .env is generated from this file on every run.\n' > "$conf"
  grep -v '^$' "$conf.new" >> "$conf"
  rm -f -- "$conf.new"
  chmod 0600 "$conf"
fi

data_dir="$(kv_get "$conf" HOMESTOCK_DATA_DIR)"
certs_dir="$(kv_get "$conf" HOMESTOCK_CERT_DIR)"
port="$(kv_get "$conf" HOMESTOCK_PORT)"; port="${port:-8443}"
host_name="$(kv_get "$conf" HOMESTOCK_HOSTNAME)"
host_name="${host_name:-$(hostname -f 2>/dev/null || hostname)}"
uid="$(kv_get "$conf" HOMESTOCK_UID)"; uid="${uid:-$APP_UID}"
gid="$(kv_get "$conf" HOMESTOCK_GID)"; gid="${gid:-$APP_GID}"
if ! { valid_path "$data_dir" && valid_path "$certs_dir" && valid_port "$port" && valid_id "$uid" && valid_id "$gid"; }; then
  die "$conf has an invalid value; fix it and re-run"
fi

# --- the app's user on this host: owns the data, and runs the NAS copy (systemd needs a name) -
if ! getent group "$gid" >/dev/null; then
  groupadd --gid "$gid" homestock
fi
if ! getent passwd "$uid" >/dev/null; then
  useradd --uid "$uid" --gid "$gid" --no-create-home --home-dir /nonexistent \
    --shell /usr/sbin/nologin homestock
fi

# --- directories, certificate, config ------------------------------------------------------
install -d -m 0700 -o "$uid" -g "$gid" "$data_dir" "$certs_dir"
chown "$uid:$gid" "$data_dir" "$certs_dir"

if [[ ! -f "$certs_dir/cert.pem" && ! -f "$certs_dir/key.pem" ]]; then
  say "creating a self-signed certificate for $host_name (replace cert.pem/key.pem to use your own)"
  san="DNS:$host_name,DNS:localhost,IP:127.0.0.1"
  for ip in $(hostname -I 2>/dev/null); do valid_ip "$ip" && san+=",IP:$ip"; done
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
    -subj "/CN=$host_name" -addext "subjectAltName=$san" \
    -keyout "$certs_dir/key.pem" -out "$certs_dir/cert.pem" 2>/dev/null
elif [[ ! -f "$certs_dir/cert.pem" || ! -f "$certs_dir/key.pem" ]]; then
  die "$certs_dir must contain both cert.pem and key.pem (or neither, to create a self-signed pair)"
fi
chown "$uid:$gid" "$certs_dir/cert.pem" "$certs_dir/key.pem"
chmod 0644 "$certs_dir/cert.pem"
chmod 0600 "$certs_dir/key.pem"

config="$install_dir/config.toml"
if [[ ! -f "$config" ]]; then
  proxies_toml=""
  for proxy in "${opt_proxies[@]}"; do proxies_toml+="${proxies_toml:+, }\"$proxy\""; done
  cat > "$config" <<EOF
# Homestock configuration (created by the installer, never overwritten by it).
# Paths are inside the container. Restart after editing: docker compose -f $install_dir/docker-compose.yml restart
bind = "0.0.0.0:8443"
tls_cert = "/etc/homestock/certs/cert.pem"
tls_key = "/etc/homestock/certs/key.pem"
data_dir = "/var/lib/homestock"
database_path = "/var/lib/homestock/homestock.sqlite3"
session_days = 30
cookie_secure = true

# Reverse proxies (e.g. HAProxy on OPNsense doing TLS offloading, with X-Forwarded-For
# enabled) whose client addresses are trusted. Firewall port $port so only they can reach it.
$( [[ -n "$proxies_toml" ]] && echo "trusted_proxies = [$proxies_toml]" || echo '# trusted_proxies = ["10.0.10.1"]' )

[backup]
idle_secs = 300
min_interval_secs = 1800
max_interval_secs = 86400
pending_keep = 3
EOF
fi
chown "root:$gid" "$config"
chmod 0640 "$config"

# --- the release's image: check its signature and pull it before anything is stopped -------
if [[ -n "${HOMESTOCK_COSIGN:-}" && -z "$opt_image" ]]; then
  "$HOMESTOCK_COSIGN" verify "$image" \
    --certificate-identity "$HOMESTOCK_SIGNER_IDENTITY" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com >/dev/null 2>&1 \
    || die "the image signature does not match the release workflow; not starting it"
  say "verified: image signed by the release workflow"
else
  warn "image signature not checked (no release verification for $image)"
fi
if [[ "$image" == *@sha256:* ]] || ! docker image inspect "$image" >/dev/null 2>&1; then
  say "pulling $image"
  docker pull --quiet "$image" >/dev/null || die "could not pull $image"
fi
new_id="$(docker image inspect -f '{{.Id}}' "$image")"

compose() { docker compose --project-directory "$install_dir" -f "$install_dir/docker-compose.yml" "$@"; }

# Run a one-off homestock command from IMAGE against this install's config and data.
homestock_tool() {
  local tool_image="$1"
  shift
  docker run --rm --network none --read-only --cap-drop ALL --security-opt no-new-privileges:true \
    --user "$uid:$gid" -v "$data_dir:/var/lib/homestock" -v "$certs_dir:/etc/homestock/certs:ro" \
    -v "$config:/etc/homestock/config.toml:ro" "$tool_image" --config /etc/homestock/config.toml "$@"
}

# A top-level string setting from config.toml (the keys before the first [section]).
toml_get() {
  awk -v key="$1" '/^[[:space:]]*\[/ { exit }
    $0 ~ "^[[:space:]]*" key "[[:space:]]*=" { sub(/^[^=]*=[[:space:]]*"/, ""); sub(/".*$/, ""); value = $0 }
    END { print value }' "$config"
}

scheme=https
[[ -n "$(toml_get tls_cert)" ]] || scheme=http

# Wait for /healthz to answer OK, or with "any" for any HTTP answer: a release from before
# /healthz existed, when rolling back to it. Gives up early if the container keeps restarting.
wait_until_up() {
  local code restarts
  say "waiting for Homestock to answer on $scheme://127.0.0.1:$port/"
  for _ in $(seq 1 60); do
    code="$(curl -sk -o /dev/null -w '%{http_code}' --max-time 2 "$scheme://127.0.0.1:$port/healthz" 2>/dev/null || true)"
    [[ "$code" == 200 ]] && return 0
    [[ "${1:-}" == any && "$code" =~ ^[1-5][0-9][0-9]$ ]] && return 0
    restarts="$(docker inspect -f '{{.RestartCount}}' homestock 2>/dev/null || echo 0)"
    (( restarts < 3 )) || return 1
    sleep 2
  done
  return 1
}

# --- upgrade safety: stop the old version, back up its database, keep the way back ---------
# This runs whenever the image changes under an existing database, which includes adopting a
# hand-made install. The database is copied (and integrity-checked) with Homestock stopped,
# by the new release's tool, which reads any older schema without migrating it.
db_path="$(toml_get database_path)"
cfg_data_dir="$(toml_get data_dir)"
db_path="${db_path:-${cfg_data_dir:-/var/lib/homestock}/homestock.sqlite3}"
db_host=""
[[ "$db_path" != /var/lib/homestock/* ]] || db_host="$data_dir/${db_path#/var/lib/homestock/}"
old_id="$(docker inspect -f '{{.Image}}' homestock 2>/dev/null || true)"
old_running="$(docker inspect -f '{{.State.Running}}' homestock 2>/dev/null || true)"
old_version="${installed_version:-unknown}"
old_label="Homestock ${installed_version:-(version unknown)}"
snapshot=""

if [[ "$old_id" != "$new_id" && -n "$db_host" && -f "$db_host" ]]; then
  new_schema="$(homestock_tool "$image" schema-version)" \
    || die "Homestock $release_version's image does not run here; nothing was stopped"
  db_schema="$(homestock_tool "$image" database-version)" \
    || die "Homestock $release_version cannot read this database (see above); nothing was stopped"
  [[ "$new_schema" =~ ^[0-9]+$ && "$db_schema" =~ ^[0-9]+$ ]] || die "unexpected schema versions '$new_schema' and '$db_schema'"
  if (( db_schema > new_schema )); then
    die "the database is at schema $db_schema, newer than Homestock $release_version supports ($new_schema); install a newer release"
  fi
  db_bytes="$(cat -- "$db_host" "$db_host-wal" 2>/dev/null | wc -c)"
  free_bytes="$(df -B1 --output=avail "$data_dir" | tail -n 1)"
  if (( free_bytes < db_bytes + 100 * 1024 * 1024 )); then
    die "not enough free space in $data_dir for the pre-upgrade backup ($db_bytes bytes plus 100 MiB)"
  fi

  migration=""
  (( db_schema == new_schema )) || migration=", migrating the database from schema $db_schema to $new_schema"
  confirm "Upgrade $old_label to $release_version$migration? It is stopped for the upgrade." \
    || die "not upgraded"

  snapshot_name="$(date -u +%Y%m%dT%H%M%SZ)-v$old_version"
  snapshot="$data_dir/pre-upgrade/$snapshot_name"
  install -d -m 0700 -o "$uid" -g "$gid" "$data_dir/pre-upgrade" "$snapshot"
  if [[ -n "$old_id" ]]; then
    docker tag "$old_id" homestock-previous:latest  # keeps the old image for a rollback
    say "stopping $old_label"
    docker stop -t 45 homestock >/dev/null || die "could not stop Homestock; nothing was changed"
  fi
  say "backing up the database to $snapshot"
  if ! homestock_tool "$image" backup "/var/lib/homestock/pre-upgrade/$snapshot_name/homestock.sqlite3" >/dev/null; then
    [[ "$old_running" != true ]] || docker start homestock >/dev/null || true
    die "the pre-upgrade backup failed, so $old_label was left as it was"
  fi
  for file in .env docker-compose.yml release.env; do
    [[ ! -f "$install_dir/$file" ]] || cp -p -- "$install_dir/$file" "$snapshot/$file"
  done
  cp -p -- "$config" "$snapshot/config.toml"
  cat > "$snapshot/manifest" <<EOF
# Pre-upgrade backup, written by the Homestock installer before switching images. To go back
# by hand: stop Homestock (docker stop homestock), copy homestock.sqlite3 over the database
# below and delete its -wal and -shm files, copy .env, docker-compose.yml and release.env back
# to $install_dir, set HOMESTOCK_IMAGE in .env to the previous image, and start it.
CREATED=$(date -u +%Y-%m-%dT%H:%M:%SZ)
FROM_VERSION=$old_version
FROM_IMAGE_ID=$old_id
TO_VERSION=$release_version
TO_IMAGE=$image
SCHEMA_VERSION=$db_schema
DATABASE=$db_host
EOF
  chmod 0600 "$snapshot/manifest"
  # Compose recreates it from the new files, even if the old one came from another project.
  [[ -z "$old_id" ]] || docker rm homestock >/dev/null
fi

# Put the previous version back after a failed upgrade: its files, its image and the database
# as it was before the upgrade. Exits.
roll_back() {
  warn "Homestock $release_version did not start properly; going back to $old_label"
  compose logs --no-color homestock > "$snapshot/failed-upgrade.log" 2>&1 || true
  docker stop -t 45 homestock >/dev/null 2>&1 || true
  docker rm homestock >/dev/null 2>&1 || true
  install -m 0600 -o "$uid" -g "$gid" "$snapshot/homestock.sqlite3" "$db_host"
  rm -f -- "$db_host-wal" "$db_host-shm"
  for file in docker-compose.yml release.env; do
    if [[ -f "$snapshot/$file" ]]; then cp -p -- "$snapshot/$file" "$install_dir/$file"; else rm -f -- "$install_dir/$file"; fi
  done
  { grep -v '^HOMESTOCK_IMAGE=' "$snapshot/.env" 2>/dev/null || true; echo "HOMESTOCK_IMAGE=homestock-previous:latest"; } > "$install_dir/.env"
  chmod 0600 "$install_dir/.env"
  if compose up -d >/dev/null 2>&1 && wait_until_up any; then
    die "the upgrade failed, and $old_label is running again with the database as it was. The new version's log is in $snapshot/failed-upgrade.log"
  fi
  die "the upgrade failed and $old_label did not restart either; the pre-upgrade backup and its manifest are in $snapshot"
}

# --- deployment files and .env -------------------------------------------------------------
install -m 0644 "$BUNDLE_DIR/docker-compose.yml" "$install_dir/docker-compose.yml"
install -m 0644 "$BUNDLE_DIR/config.example.toml" "$install_dir/config.example.toml"
[[ ! -f "$BUNDLE_DIR/release.env" ]] || install -m 0644 "$BUNDLE_DIR/release.env" "$install_dir/release.env"
{
  echo "# Generated by the Homestock installer from install.conf; edits here are replaced."
  grep '^HOMESTOCK_' "$conf"
  echo "HOMESTOCK_IMAGE=$image"
} > "$install_dir/.env.new"
chmod 0600 "$install_dir/.env.new"
mv -f -- "$install_dir/.env.new" "$install_dir/.env"

# --- start ---------------------------------------------------------------------------------
if ! compose up -d --remove-orphans; then
  $in_lxc && warn "in a Proxmox LXC, Docker needs the 'nesting' feature, and 'keyctl' if containers fail to start with a keyring or permission error"
  [[ -z "$snapshot" || -z "$old_id" ]] || roll_back
  die "docker compose could not start Homestock"
fi
if ! wait_until_up; then
  compose logs --tail=40 homestock >&2 || true
  [[ -z "$snapshot" || -z "$old_id" ]] || roll_back
  die "Homestock did not become healthy; the log above usually says why"
fi

if [[ -n "$snapshot" ]]; then
  now_schema="$(docker exec homestock /usr/local/bin/homestock --config /etc/homestock/config.toml database-version 2>/dev/null || true)"
  if [[ -n "$now_schema" && "$now_schema" != "$db_schema" ]]; then
    say "the database was migrated from schema $db_schema to $now_schema"
  fi
  say "upgraded from $old_label; the pre-upgrade backup is in $snapshot"
  mapfile -t expired < <(find "$data_dir/pre-upgrade" -mindepth 1 -maxdepth 1 -type d | sort | head -n -3)
  for dir in "${expired[@]}"; do rm -rf -- "$dir"; done
fi

# --- tools: homestock-upgrade and homestock-admin, set up for this install -----------------
repo="${HOMESTOCK_REPO:-ntyndall1/homestock}"
[[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "invalid HOMESTOCK_REPO: $repo"
install_tool() { # install_tool SOURCE NAME
  sed -e "s|^INSTALL_DIR=.*|INSTALL_DIR=$install_dir  # set by the installer|" \
    -e "s|^REPO=.*|REPO=\"\${HOMESTOCK_REPO:-$repo}\"  # set by the installer|" \
    "$1" > "/usr/local/bin/$2.new"
  chmod 0755 "/usr/local/bin/$2.new"
  mv -f -- "/usr/local/bin/$2.new" "/usr/local/bin/$2"
}
install_tool "$BUNDLE_DIR/install.sh" homestock-upgrade
install_tool "$BUNDLE_DIR/homestock-admin.sh" homestock-admin
install_tool "$BUNDLE_DIR/homestock-nas.sh" homestock-nas
install_tool "$BUNDLE_DIR/homestock-doctor.sh" homestock-doctor
install -m 0755 "$BUNDLE_DIR/sync-homestock-backups.sh" "$install_dir/sync-homestock-backups.sh"

# A shortcut in the home directory of whoever ran the installer (moving or deleting it is fine).
owner="${SUDO_USER:-root}"
home="$(getent passwd "$owner" | cut -d: -f6 || true)"
if [[ -n "$home" && -d "$home" ]]; then
  if [[ -L "$home/homestock_upgrade.sh" || ! -e "$home/homestock_upgrade.sh" ]]; then
    ln -sfn /usr/local/bin/homestock-upgrade "$home/homestock_upgrade.sh"
    chown -h "$owner:" "$home/homestock_upgrade.sh" 2>/dev/null || true
  else
    warn "$home/homestock_upgrade.sh exists and is not a link; left it alone"
  fi
fi

# --- copying backups to a NAS: set up when asked, and its units refreshed on every run ------
has_systemd=false
if command -v systemctl >/dev/null && [[ -d /run/systemd/system ]]; then has_systemd=true; fi
nas_args=()
$assume_yes && nas_args+=(--yes)
if [[ -n "$(kv_get "$conf" HOMESTOCK_NAS_MODE)" ]] && $has_systemd; then
  homestock-nas refresh || warn "could not refresh the NAS copy's systemd units"
elif [[ -n "$opt_nas" ]]; then
  read -r -a nas_setup <<<"$opt_nas"
  homestock-nas setup "${nas_setup[@]}" "${nas_args[@]}" \
    || warn "copying backups to the NAS is not set up yet; fix the above, then: sudo homestock-nas setup ${opt_nas}"
fi

# --- next steps ----------------------------------------------------------------------------
address="$(hostname -I 2>/dev/null | awk '{print $1}')"
say "Homestock $release_version is running: $scheme://$host_name:$port/"
cat <<EOF

  Files:   $install_dir (settings in install.conf, app config in config.toml)
  Data:    $data_dir
  Certs:   $certs_dir$( [[ -n "$address" ]] && printf '\n  Address: %s:%s' "$address" "$port" )

  Behind a TLS-offloading proxy (e.g. HAProxy on OPNsense):
    - backend server ${address:-<this-host>}:$port with SSL enabled (verify none for the self-signed
      certificate), the X-Forwarded-For option on, and /healthz as the health check;
    - list the proxy's address in trusted_proxies in config.toml, then restart;
    - firewall port $port so only the proxy can reach it (on Proxmox: the container's firewall).

  Upgrade:   sudo homestock-upgrade   (--check to see if there is a new release;
             ~/homestock_upgrade.sh is a shortcut to it)
  Accounts:  sudo homestock-admin list-users | reset-password NAME | create-admin NAME
  NAS copy:  sudo homestock-nas setup mount DIR | setup rclone | confirm | sync | disable
  Checks:    sudo homestock-doctor
EOF
if $fresh; then
  echo
  echo "  Next: open the address above and create the household admin on the setup screen."
fi

# Last, so anything that still needs doing (such as enabling FUSE) is what is left on screen.
echo
if ! homestock-doctor; then
  echo
  warn "Homestock is installed, but the checks above need attention: follow the → steps, then run sudo homestock-doctor"
fi
