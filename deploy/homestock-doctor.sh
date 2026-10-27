#!/usr/bin/env bash
# homestock-doctor: check that Homestock, its backups and (if set up) the copy to a NAS are
# working, and say what to do about anything that isn't. The installer puts it on PATH as
# /usr/local/bin/homestock-doctor and runs it at the end of every install and upgrade.
#
#   sudo homestock-doctor
#
# Exits 0 when nothing failed (warnings included), 1 otherwise.
set -uo pipefail

INSTALL_DIR=/opt/homestock

CONF="$INSTALL_DIR/install.conf"
failures=0
ok() { printf '  \033[32mok\033[0m    %s\n' "$*"; }
info() { printf '  \033[2m--\033[0m    %s\n' "$*"; }
warn() { printf '  \033[33mwarn\033[0m  %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; failures=$((failures + 1)); }
fix() { printf '        → %s\n' "$*"; }
section() { printf '\n\033[1m%s\033[0m\n' "$*"; }
kv_get() {
  if [[ -f "$1" ]]; then sed -n "s/^$2=//p" "$1" | tail -n 1; fi
}
conf_get() { kv_get "$CONF" "$1"; }
age() { # age SECONDS → "3 h", "2 d"
  local s="$1"
  if (( s < 120 )); then echo "${s} s"; elif (( s < 7200 )); then echo "$((s / 60)) min"
  elif (( s < 172800 )); then echo "$((s / 3600)) h"; else echo "$((s / 86400)) d"; fi
}
fuse_ok() { [[ -c /dev/fuse ]] && ( exec 3<>/dev/fuse ) 2>/dev/null; }

if [[ "$(id -u)" -ne 0 ]]; then
  echo "homestock-doctor: run as root: sudo homestock-doctor" >&2
  exit 1
fi
if [[ ! -f "$CONF" ]]; then
  echo "homestock-doctor: no Homestock install in $INSTALL_DIR" >&2
  exit 1
fi

data_dir="$(conf_get HOMESTOCK_DATA_DIR)"
certs_dir="$(conf_get HOMESTOCK_CERT_DIR)"
port="$(conf_get HOMESTOCK_PORT)"; port="${port:-8443}"
uid="$(conf_get HOMESTOCK_UID)"; uid="${uid:-10001}"
gid="$(conf_get HOMESTOCK_GID)"; gid="${gid:-10001}"
config="$INSTALL_DIR/config.toml"
scheme=https
grep -Eq '^[[:space:]]*tls_cert[[:space:]]*=' "$config" 2>/dev/null || scheme=http
in_container() { docker exec homestock /usr/local/bin/homestock --config /etc/homestock/config.toml "$@" 2>&1; }

# --- Homestock -----------------------------------------------------------------------------
section "Homestock"
version="$(kv_get "$INSTALL_DIR/release.env" HOMESTOCK_VERSION)"
info "installed release: ${version:-unknown} ($INSTALL_DIR)"
virt="$(systemd-detect-virt --container 2>/dev/null || true)"
[[ -z "$virt" || "$virt" == none ]] || info "running in a container ($virt)"

if ! command -v docker >/dev/null; then
  fail "Docker is not installed"; fix "re-run the installer"
elif ! docker info >/dev/null 2>&1; then
  fail "Docker is not running"; fix "systemctl start docker"
else
  ok "Docker $(docker version -f '{{.Server.Version}}' 2>/dev/null), Compose $(docker compose version --short 2>/dev/null)"
  state="$(docker inspect -f '{{.State.Status}} {{.RestartCount}}' homestock 2>/dev/null)"
  if [[ -z "$state" ]]; then
    fail "the homestock container does not exist"; fix "re-run the installer, or: docker compose -f $INSTALL_DIR/docker-compose.yml up -d"
  elif [[ "${state%% *}" != running ]]; then
    fail "the homestock container is ${state%% *}"; fix "docker logs homestock; then docker start homestock"
  else
    ok "container running"
    (( ${state##* } == 0 )) || warn "the container has restarted ${state##* } times; see: docker logs homestock"
    expected="$(kv_get "$INSTALL_DIR/.env" HOMESTOCK_IMAGE)"
    running="$(docker inspect -f '{{.Config.Image}}' homestock 2>/dev/null)"
    [[ -z "$expected" || "$running" == "$expected" ]] \
      || warn "running $running, but .env names $expected; re-run the installer to apply it"
  fi
  code="$(curl -sk -o /dev/null -w '%{http_code}' --max-time 5 "$scheme://127.0.0.1:$port/healthz" 2>/dev/null || true)"
  if [[ "$code" == 200 ]]; then
    ok "answering on $scheme://127.0.0.1:$port"
  else
    fail "no healthy answer on $scheme://127.0.0.1:$port/healthz (HTTP ${code:-none})"; fix "docker logs --tail 40 homestock"
  fi
  if [[ "${state%% *}" == running ]]; then
    db_schema="$(in_container database-version)"
    app_schema="$(in_container schema-version)"
    if [[ "$db_schema" =~ ^[0-9]+$ && "$db_schema" == "$app_schema" ]]; then
      ok "database at schema $db_schema"
    else
      fail "database schema check: ${db_schema:-no answer} (this release expects ${app_schema:-?})"
    fi
  fi
fi

cert="$certs_dir/cert.pem"
if [[ -f "$cert" ]]; then
  end="$(openssl x509 -enddate -noout -in "$cert" 2>/dev/null | cut -d= -f2)"
  if ! openssl x509 -checkend 0 -noout -in "$cert" >/dev/null 2>&1; then
    fail "the certificate expired ($end)"; fix "replace $cert and key.pem, or delete both and re-run the installer for a new self-signed pair"
  elif ! openssl x509 -checkend $((30 * 86400)) -noout -in "$cert" >/dev/null 2>&1; then
    warn "the certificate expires soon ($end)"
  else
    ok "certificate valid until $end"
  fi
elif [[ "$scheme" == https ]]; then
  fail "no certificate at $cert"
fi

if [[ -d "$data_dir" ]]; then
  free="$(df -B1 --output=avail "$data_dir" | tail -n 1)"
  if (( free < 1024 * 1024 * 1024 )); then
    warn "only $(df -h --output=avail "$data_dir" | tail -n 1 | tr -d ' ') free for $data_dir"
  else
    ok "$(df -h --output=avail "$data_dir" | tail -n 1 | tr -d ' ') free for $data_dir"
  fi
else
  fail "the data folder $data_dir is missing"
fi

# --- backups -------------------------------------------------------------------------------
section "Backups"
now="$(date +%s)"
mapfile -t pending < <(find "$data_dir/staging" -maxdepth 1 -type f -name 'homestock-*.sqlite3' -printf '%T@ %f\n' 2>/dev/null | sort -n)
if [[ ${#pending[@]} -eq 0 ]]; then
  info "no backups waiting in $data_dir/staging"
else
  oldest="${pending[0]%%.*}"
  info "${#pending[@]} backup(s) waiting in $data_dir/staging, oldest $(age $((now - oldest))) old"
fi
snapshots="$(find "$data_dir/pre-upgrade" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l)"
(( snapshots == 0 )) || info "$snapshots pre-upgrade backup(s) in $data_dir/pre-upgrade"

mode="$(conf_get HOMESTOCK_NAS_MODE)"
backup_dir="$(conf_get HOMESTOCK_BACKUP_DIR)"
sentinel="$(conf_get HOMESTOCK_BACKUP_SENTINEL)"; sentinel="${sentinel:-.homestock-nas}"
if [[ -z "$mode" ]]; then
  info "backups are not copied to a NAS (set up with: sudo homestock-nas setup mount DIR | rclone)"
else
  section "NAS copy ($([[ "$mode" == rclone ]] && echo "rclone mount of $(conf_get HOMESTOCK_NAS_REMOTE)" || echo "mounted folder"), $backup_dir)"
  mounted=true
  if [[ "$mode" == rclone ]]; then
    if command -v rclone >/dev/null && command -v fusermount3 >/dev/null; then
      ok "rclone and fuse3 installed"
    else
      fail "rclone or fuse3 is missing"; fix "sudo homestock-nas setup rclone"
    fi
    if fuse_ok; then
      ok "FUSE available"
    else
      fail "FUSE is not available, so the share cannot be mounted"
      fix "on the Proxmox host: pct set <CTID> --features nesting=1,fuse=1 (keep keyctl=1 if set)"
      fix "then restart the container: pct reboot <CTID>, and run sudo homestock-doctor again"
    fi
    if mountpoint -q "$backup_dir"; then
      ok "share mounted at $backup_dir"
    else
      mounted=false
      fail "the share is not mounted at $backup_dir"
      fuse_ok && fix "systemctl status homestock-nas-mount; journalctl -u homestock-nas-mount -n 20"
    fi
  elif [[ ! -d "$backup_dir" ]]; then
    mounted=false
    fail "$backup_dir does not exist"; fix "mount the NAS share there"
  elif [[ "$(findmnt -n -o TARGET -T "$backup_dir")" == / ]]; then
    warn "$backup_dir is on this machine's own disk, not a mounted share"
  else
    ok "$backup_dir is on $(findmnt -n -o SOURCE,FSTYPE -T "$backup_dir" | tr -s ' ')"
  fi

  if $mounted; then
    if [[ -f "$backup_dir/$sentinel" ]]; then
      ok "share confirmed ($sentinel present)"
    else
      fail "the share is not confirmed, so nothing is copied to it"; fix "check it is the NAS, then: sudo homestock-nas confirm"
    fi
    # shellcheck disable=SC2016 # expanded by the inner shell
    if setpriv --reuid="$uid" --regid="$gid" --clear-groups -- \
      sh -c 'f="$1/.homestock-write-test.$$"; : > "$f" && rm -f -- "$f"' sh "$backup_dir" 2>/dev/null; then
      ok "writable by Homestock's user (uid $uid)"
    else
      fail "Homestock's user (uid $uid) cannot write to $backup_dir"
    fi
    newest="$(find "$backup_dir" -maxdepth 1 -type f -name 'homestock-*.sqlite3' -printf '%T@\n' 2>/dev/null | sort -n | tail -n 1)"
    if [[ -n "$newest" ]]; then
      info "newest backup on the NAS is $(age $((now - ${newest%%.*}))) old"
    else
      info "no backups on the NAS yet"
    fi
  fi

  if systemctl is-enabled --quiet homestock-backup-sync.timer 2>/dev/null && systemctl is-active --quiet homestock-backup-sync.timer; then
    ok "sync timer active (every five minutes)"
  else
    fail "the sync timer is not running"; fix "sudo homestock-nas refresh, or set it up again"
  fi
  result="$(systemctl show homestock-backup-sync.service -p Result --value 2>/dev/null)"
  last="$(systemctl show homestock-backup-sync.service -p ExecMainExitTimestamp --value 2>/dev/null)"
  if [[ -z "$last" ]]; then
    info "the sync has not run since boot"
  elif [[ "$result" == success ]]; then
    ok "last sync succeeded ($last)"
  else
    fail "last sync failed ($last)"; fix "journalctl -u homestock-backup-sync -n 20"
  fi
  if [[ ${#pending[@]} -gt 0 ]] && (( now - oldest > 3600 )); then
    fail "a backup has waited over an hour to be copied"
  fi
fi

echo
if (( failures == 0 )); then
  printf '\033[32mAll checks passed.\033[0m\n'
  exit 0
fi
printf '\033[31m%d check(s) need attention (see → above).\033[0m\n' "$failures"
exit 1
