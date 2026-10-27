#!/usr/bin/env bash
# homestock-admin: account recovery for whoever has root on the Homestock server. The installer
# puts it on PATH as /usr/local/bin/homestock-admin.
#
#   sudo homestock-admin list-users
#   sudo homestock-admin reset-password NAME   # prints a temporary password, changed at sign-in
#   sudo homestock-admin create-admin NAME     # a new admin, with a temporary password
#   sudo homestock-admin make-admin NAME       # promote an existing member
#   sudo homestock-admin database-version | schema-version
#
# The command runs inside the Homestock container (a one-off one if Homestock is stopped), and
# who ran what is recorded in the system log (journalctl -t homestock-admin). Passwords it
# prints are not logged.
set -euo pipefail

INSTALL_DIR=/opt/homestock

die() { printf 'homestock-admin: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -eq 0 ]] || die "run as root: sudo homestock-admin ${*:-help}"
[[ -f "$INSTALL_DIR/docker-compose.yml" ]] || die "no Homestock install in $INSTALL_DIR"

case "${1:-help}" in
  help|-h|--help)
    echo "Usage:"
    sed -n 's/^#   /  /p' "$0"
    exit 0
    ;;
  serve|backup|--config)
    die "'$1' is not available here; see: homestock-admin help"
    ;;
esac

logger -t homestock-admin -- "${SUDO_USER:-root} ran: $*" 2>/dev/null || true

compose=(docker compose --progress quiet --project-directory "$INSTALL_DIR" -f "$INSTALL_DIR/docker-compose.yml")
if [[ -n "$("${compose[@]}" ps -q --status running homestock 2>/dev/null)" ]]; then
  exec "${compose[@]}" exec -T homestock /usr/local/bin/homestock --config /etc/homestock/config.toml "$@"
fi
exec "${compose[@]}" run --rm --no-deps -T homestock --config /etc/homestock/config.toml "$@"
