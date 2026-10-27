#!/usr/bin/env bash
# homestock-nas: copy Homestock's backups to a NAS. The installer puts it on PATH as
# /usr/local/bin/homestock-nas.
#
#   sudo homestock-nas setup mount DIR   # use a folder already mounted from the NAS
#   sudo homestock-nas setup rclone      # mount an SMB share with rclone (asks for the details)
#   sudo homestock-nas confirm           # check the share is mounted, then mark it for backups
#   sudo homestock-nas sync              # copy pending backups now
#   sudo homestock-nas disable           # stop copying (nothing is deleted from the NAS)
#
# Homestock writes backups to <data>/staging, and a systemd timer copies them to the NAS every
# five minutes. It only copies once the share holds the sentinel file written by `confirm`,
# so the empty mount point of a share that failed to mount is never mistaken for the NAS.
# Add --yes to answer yes to every question.
set -euo pipefail

INSTALL_DIR=/opt/homestock

CONF="$INSTALL_DIR/install.conf"
RCLONE_CONF="$INSTALL_DIR/rclone.conf"
SYNC_UNIT=homestock-backup-sync
MOUNT_UNIT=homestock-nas-mount
UNIT_DIR=/etc/systemd/system

say() { printf '\033[1mhomestock-nas:\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mhomestock-nas: warning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31mhomestock-nas: error:\033[0m %s\n' "$*" >&2; exit 1; }

assume_yes=false
args=()
for arg in "$@"; do
  case "$arg" in
    -y|--yes) assume_yes=true ;;
    *) args+=("$arg") ;;
  esac
done
set -- "${args[@]}"

usage() { echo "Usage:"; sed -n 's/^#   /  /p' "$0"; }

valid_path() { [[ "$1" =~ ^/[A-Za-z0-9._/-]+$ && "$1" != *..* ]]; }
kv_get() {
  if [[ -f "$1" ]]; then sed -n "s/^$2=//p" "$1" | tail -n 1; fi
}
conf_get() { kv_get "$CONF" "$1"; }
conf_set() { # conf_set KEY [VALUE]: set KEY, or remove it when no value is given
  { grep -v "^$1=" "$CONF" || true; [[ $# -lt 2 ]] || echo "$1=$2"; } > "$CONF.new"
  chmod 0600 "$CONF.new"
  mv -f -- "$CONF.new" "$CONF"
}

ask() { # ask VAR "Question" default
  local answer
  if $assume_yes || [[ ! -r /dev/tty ]]; then printf -v "$1" '%s' "$3"; return; fi
  read -r -p "$2${3:+ [$3]}: " answer </dev/tty || true
  printf -v "$1" '%s' "${answer:-$3}"
}
confirm_no() { # confirm_no "Question" → 0 only for an explicit yes
  local answer
  $assume_yes && return 0
  [[ -r /dev/tty ]] || return 1
  read -r -p "$1 [y/N]: " answer </dev/tty || true
  [[ "$answer" =~ ^[Yy] ]]
}

# FUSE works only if /dev/fuse exists and this container may open it.
fuse_ok() { [[ -c /dev/fuse ]] && ( exec 3<>/dev/fuse ) 2>/dev/null; }
fuse_help() {
  cat >&2 <<'EOF'

  FUSE is not available here, so rclone cannot mount the share. In a Proxmox LXC, enable it
  on the Proxmox host, keeping the features the container already has (nesting is needed
  for Docker; add keyctl=1 if it was set):
      pct set <CTID> --features nesting=1,fuse=1
      pct reboot <CTID>
  (or Options → Features → FUSE in the web UI, then restart the container). The share then
  mounts at boot. Check it with: sudo homestock-doctor, then mark it with: sudo homestock-nas confirm

EOF
}

[[ "$(id -u)" -eq 0 ]] || die "run as root: sudo homestock-nas ${*:-help}"
[[ -f "$CONF" ]] || die "no Homestock install in $INSTALL_DIR"
if ! command -v systemctl >/dev/null || [[ ! -d /run/systemd/system ]]; then
  die "copying backups to a NAS needs systemd"
fi

uid="$(conf_get HOMESTOCK_UID)"; uid="${uid:-10001}"
gid="$(conf_get HOMESTOCK_GID)"; gid="${gid:-10001}"
data_dir="$(conf_get HOMESTOCK_DATA_DIR)"
if ! [[ "$uid" =~ ^[0-9]+$ && "$gid" =~ ^[0-9]+$ ]] || ! valid_path "$data_dir"; then
  die "$CONF has an invalid value"
fi

# Run a command as Homestock's user, which owns the backups and does the copying.
as_app() { setpriv --reuid="$uid" --regid="$gid" --clear-groups -- "$@"; }
writable_by_app() {
  # shellcheck disable=SC2016 # expanded by the inner shell
  as_app sh -c 'f="$1/.homestock-write-test.$$"; : > "$f" && rm -f -- "$f"' sh "$1" 2>/dev/null
}
not_writable() {
  die "Homestock's user (uid $uid) cannot write to $1. Give it to that user: for a CIFS mount add uid=$uid,gid=$gid to the mount options, for rclone --uid $uid --gid $gid, for NFS chown the folder on the NAS"
}

apt_install() {
  command -v apt-get >/dev/null || die "please install: $*"
  say "installing $*"
  DEBIAN_FRONTEND=noninteractive apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" >/dev/null
}

# --- systemd units -------------------------------------------------------------------------
write_units() { # write_units [restart]: (re)write the units for the configured mode
  local mode backup_dir
  mode="$(conf_get HOMESTOCK_NAS_MODE)"
  backup_dir="$(conf_get HOMESTOCK_BACKUP_DIR)"
  cat > "$UNIT_DIR/$SYNC_UNIT.service" <<EOF
# Written by homestock-nas, and again on each upgrade; settings are in $CONF.
[Unit]
Description=Copy Homestock backups to the NAS
After=$MOUNT_UNIT.service

[Service]
Type=oneshot
EnvironmentFile=$CONF
User=$uid
Group=$gid
NoNewPrivileges=yes
ExecStart=$INSTALL_DIR/sync-homestock-backups.sh
EOF
  cat > "$UNIT_DIR/$SYNC_UNIT.timer" <<EOF
# Written by homestock-nas, and again on each upgrade.
[Unit]
Description=Copy Homestock backups to the NAS every five minutes

[Timer]
OnBootSec=2min
OnUnitActiveSec=5min
Persistent=true
Unit=$SYNC_UNIT.service

[Install]
WantedBy=timers.target
EOF
  if [[ "$mode" == rclone ]]; then
    cat > "$UNIT_DIR/$MOUNT_UNIT.service" <<EOF
# Written by homestock-nas, and again on each upgrade; the share's login is in $RCLONE_CONF.
[Unit]
Description=Homestock NAS share $(conf_get HOMESTOCK_NAS_REMOTE) (rclone mount at $backup_dir)
Wants=network-online.target
After=network-online.target
ConditionPathExists=/dev/fuse

[Service]
Type=notify
ExecStart=$(command -v rclone) mount homestock-nas:$(conf_get HOMESTOCK_NAS_PATH) $backup_dir --config $RCLONE_CONF --uid $uid --gid $gid --umask 077 --allow-other --dir-cache-time 30s
ExecStop=$(command -v fusermount3) -uz $backup_dir
Restart=on-failure
RestartSec=30

[Install]
WantedBy=multi-user.target
EOF
  elif [[ -f "$UNIT_DIR/$MOUNT_UNIT.service" ]]; then
    systemctl disable --now "$MOUNT_UNIT.service" >/dev/null 2>&1 || true
    rm -f -- "$UNIT_DIR/$MOUNT_UNIT.service"
  fi
  systemctl daemon-reload
  if [[ "$mode" == rclone ]]; then
    systemctl enable "$MOUNT_UNIT.service" >/dev/null 2>&1
    if fuse_ok; then
      if [[ "${1:-}" == restart ]]; then
        systemctl restart "$MOUNT_UNIT.service" || warn "the share did not mount: systemctl status $MOUNT_UNIT"
      else
        systemctl start "$MOUNT_UNIT.service" || warn "the share did not mount: systemctl status $MOUNT_UNIT"
      fi
    fi
  fi
  systemctl enable --now "$SYNC_UNIT.timer" >/dev/null 2>&1
}

# --- confirm: the human check that the share is the NAS, then the sentinel -----------------
confirm_share() {
  local mode backup_dir sentinel
  mode="$(conf_get HOMESTOCK_NAS_MODE)"
  backup_dir="$(conf_get HOMESTOCK_BACKUP_DIR)"
  sentinel="$(conf_get HOMESTOCK_BACKUP_SENTINEL)"; sentinel="${sentinel:-.homestock-nas}"
  [[ -n "$mode" ]] || die "copying to a NAS is not set up: sudo homestock-nas setup mount DIR | rclone"
  if [[ "$mode" == rclone ]]; then
    if ! fuse_ok; then fuse_help; die "the share cannot be mounted until FUSE is enabled"; fi
    systemctl is-active --quiet "$MOUNT_UNIT.service" || systemctl start "$MOUNT_UNIT.service" || true
    mountpoint -q "$backup_dir" || die "the share is not mounted at $backup_dir; see: systemctl status $MOUNT_UNIT"
  fi
  [[ -d "$backup_dir" ]] || die "$backup_dir does not exist; is the share mounted?"
  if [[ -f "$backup_dir/$sentinel" ]]; then
    say "$backup_dir is already confirmed as the NAS share ($sentinel is there)"
    return 0
  fi
  echo "This is what is at $backup_dir:"
  findmnt -T "$backup_dir" -o TARGET,SOURCE,FSTYPE | sed 's/^/  /'
  df -h --output=size,used,avail "$backup_dir" | sed 's/^/  /'
  echo "  contents:"
  # shellcheck disable=SC2012 # a listing for a person to look at
  ls -A "$backup_dir" | head -n 10 | sed 's/^/    /'
  if [[ "$(findmnt -n -o TARGET -T "$backup_dir")" == / ]]; then
    warn "$backup_dir is on this machine's own disk, not a mounted share"
  fi
  writable_by_app "$backup_dir" || not_writable "$backup_dir"
  confirm_no "Is this the NAS share for Homestock's backups?" || die "not confirmed; nothing was written"
  # shellcheck disable=SC2016 # expanded by the inner shell
  as_app sh -c 'printf "Homestock backups go here (confirmed %s on %s).\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$2" > "$1"' \
    sh "$backup_dir/$sentinel" "$(hostname)"
  say "confirmed: wrote $backup_dir/$sentinel; pending backups are copied within five minutes"
  systemctl start --no-block "$SYNC_UNIT.service" || true
}

# --- setup ---------------------------------------------------------------------------------
setup_common() { # setup_common MODE DIR REQUIRE_MOUNT
  [[ -x "$INSTALL_DIR/sync-homestock-backups.sh" ]] || die "$INSTALL_DIR/sync-homestock-backups.sh is missing; re-run the installer"
  conf_set HOMESTOCK_NAS_MODE "$1"
  conf_set HOMESTOCK_BACKUP_DIR "$2"
  conf_set HOMESTOCK_BACKUP_REQUIRE_MOUNT "$3"
  [[ -n "$(conf_get HOMESTOCK_BACKUP_SENTINEL)" ]] || conf_set HOMESTOCK_BACKUP_SENTINEL .homestock-nas
}

setup_mount() {
  local dir="${1:-}"
  [[ -n "$dir" ]] || die "usage: homestock-nas setup mount DIR"
  valid_path "$dir" || die "the folder must be an absolute path without spaces: $dir"
  [[ -d "$dir" ]] || die "$dir does not exist; mount the NAS share there first"
  writable_by_app "$dir" || not_writable "$dir"
  setup_common mount "$dir" false
  conf_set HOMESTOCK_NAS_REMOTE
  conf_set HOMESTOCK_NAS_PATH
  rm -f -- "$RCLONE_CONF"
  write_units
  say "backups will be copied to $dir"
  confirm_share
}

setup_rclone() {
  local host share folder user pass domain mnt obscured
  $assume_yes && die "rclone setup asks for the NAS login; run it in a terminal without --yes"
  [[ -r /dev/tty ]] || die "rclone setup asks for the NAS login; run it in a terminal"
  ask host "NAS server name or IP address" ""
  ask share "Share name on the NAS" ""
  ask folder "Folder in the share for Homestock's backups" "homestock"
  ask user "User name on the NAS" ""
  read -r -s -p "Password for $user: " pass </dev/tty || true
  echo
  ask domain "Domain or workgroup" "WORKGROUP"
  ask mnt "Mount point on this machine" "/mnt/homestock-nas"
  [[ "$host" =~ ^[A-Za-z0-9]([A-Za-z0-9.:-]*[A-Za-z0-9])?$ ]] || die "invalid server: $host"
  [[ "$share" =~ ^[A-Za-z0-9._$-]+$ ]] || die "invalid share name (letters, digits, . _ - \$ only): $share"
  [[ "$folder" =~ ^[A-Za-z0-9._/-]+$ && "$folder" != *..* ]] || die "invalid folder: $folder"
  [[ "$user" =~ ^[A-Za-z0-9._@-]+$ ]] || die "invalid user name: $user"
  [[ "$domain" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid domain: $domain"
  valid_path "$mnt" || die "the mount point must be an absolute path without spaces: $mnt"
  [[ -n "$pass" ]] || die "a password is required"
  folder="${folder#/}"; folder="${folder%/}"

  local missing=()
  command -v rclone >/dev/null || missing+=(rclone)
  command -v fusermount3 >/dev/null || missing+=(fuse3)
  command -v setpriv >/dev/null || missing+=(util-linux)
  [[ ${#missing[@]} -eq 0 ]] || apt_install "${missing[@]}"
  if [[ "$(systemctl show "$MOUNT_UNIT.service" -p ActiveState --value 2>/dev/null)" == active ]]; then
    systemctl stop "$MOUNT_UNIT.service"
  fi

  obscured="$(printf '%s' "$pass" | rclone obscure -)" || die "rclone could not encode the password"
  umask 077
  cat > "$RCLONE_CONF.new" <<EOF
# Written by homestock-nas setup rclone. The password is obscured, not encrypted: keep this private.
[homestock-nas]
type = smb
host = $host
user = $user
pass = $obscured
domain = $domain
EOF
  mv -f -- "$RCLONE_CONF.new" "$RCLONE_CONF"
  umask 022
  say "checking the login for //$host/$share"
  rclone lsf --config "$RCLONE_CONF" --max-depth 1 "homestock-nas:$share" >/dev/null \
    || die "could not open //$host/$share as $user (see the rclone error above); settings were not changed"
  rclone mkdir --config "$RCLONE_CONF" "homestock-nas:$share/$folder" || die "could not create $folder on the share"

  # The mount point itself stays root-only, so nothing is written there while unmounted.
  if mountpoint -q "$mnt"; then die "something else is already mounted at $mnt"; fi
  install -d -m 0700 "$mnt"
  setup_common rclone "$mnt" true
  conf_set HOMESTOCK_NAS_REMOTE "//$host/$share/$folder"
  conf_set HOMESTOCK_NAS_PATH "$share/$folder"
  write_units restart
  say "backups will be copied to //$host/$share/$folder, mounted at $mnt"
  if ! fuse_ok; then
    fuse_help
    warn "saved, but the share cannot be mounted until FUSE is enabled"
    return 0
  fi
  for _ in $(seq 1 15); do mountpoint -q "$mnt" && break; sleep 1; done
  confirm_share
}

disable() {
  systemctl disable --now "$SYNC_UNIT.timer" "$MOUNT_UNIT.service" >/dev/null 2>&1 || true
  rm -f -- "$UNIT_DIR/$SYNC_UNIT.service" "$UNIT_DIR/$SYNC_UNIT.timer" "$UNIT_DIR/$MOUNT_UNIT.service" "$RCLONE_CONF"
  systemctl daemon-reload
  for key in HOMESTOCK_NAS_MODE HOMESTOCK_NAS_REMOTE HOMESTOCK_NAS_PATH HOMESTOCK_BACKUP_DIR HOMESTOCK_BACKUP_REQUIRE_MOUNT; do
    conf_set "$key"
  done
  say "stopped copying backups to the NAS; what is already there was left alone"
}

case "${1:-help}" in
  setup)
    case "${2:-}" in
      mount) setup_mount "${3:-}" ;;
      rclone) setup_rclone ;;
      *) die "usage: homestock-nas setup mount DIR | homestock-nas setup rclone" ;;
    esac
    ;;
  confirm) confirm_share ;;
  sync)
    [[ -n "$(conf_get HOMESTOCK_NAS_MODE)" ]] || die "copying to a NAS is not set up"
    since="$(date '+%Y-%m-%d %H:%M:%S')"
    status=0
    systemctl start "$SYNC_UNIT.service" || status=$?
    journalctl -u "$SYNC_UNIT.service" --since "$since" -o cat --no-pager 2>/dev/null | grep -v '^\(Starting\|Finished\|Started\|homestock-backup-sync.service: Deactivated\)' || true
    exit "$status"
    ;;
  disable) disable ;;
  refresh) # used by the installer: rewrite the units after an upgrade
    [[ -z "$(conf_get HOMESTOCK_NAS_MODE)" ]] || write_units
    ;;
  help|-h|--help) usage ;;
  *) usage >&2; exit 2 ;;
esac
