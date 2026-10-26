#!/bin/sh
set -eu

data_dir=${HOMESTOCK_DATA_DIR:-./data}
backup_dir=${HOMESTOCK_BACKUP_DIR:-./backups}
sentinel=${HOMESTOCK_BACKUP_SENTINEL:-.homestock-nas}
keep=${HOMESTOCK_BACKUP_KEEP:-48}
sync_photos=${HOMESTOCK_SYNC_PHOTOS:-true}
require_mount=${HOMESTOCK_BACKUP_REQUIRE_MOUNT:-false}
staging_dir="${data_dir%/}/staging"

case "$keep" in
    ''|*[!0-9]*) echo "HOMESTOCK_BACKUP_KEEP must be a positive integer" >&2; exit 2 ;;
esac
if [ "$keep" -lt 1 ]; then
    echo "HOMESTOCK_BACKUP_KEEP must be a positive integer" >&2
    exit 2
fi
case "$sync_photos" in
    true|false) ;;
    *) echo "HOMESTOCK_SYNC_PHOTOS must be true or false" >&2; exit 2 ;;
esac
case "$require_mount" in
    true|false) ;;
    *) echo "HOMESTOCK_BACKUP_REQUIRE_MOUNT must be true or false" >&2; exit 2 ;;
esac

if [ "$require_mount" = true ] && ! mountpoint -q "$backup_dir"; then
    echo "Homestock backup sync: backup mount is unavailable; leaving pending files in place"
    exit 0
fi
if [ ! -f "${backup_dir%/}/$sentinel" ]; then
    echo "Homestock backup sync: sentinel is unavailable; leaving pending files in place"
    exit 0
fi

for source in "$staging_dir"/homestock-*.sqlite3; do
    [ -f "$source" ] || continue
    name=${source##*/}
    destination="${backup_dir%/}/$name"
    if [ -f "$destination" ]; then
        rm -- "$source"
        continue
    fi

    partial="${destination}.part"
    rm -f -- "$partial"
    cp -- "$source" "$partial"
    sync -f "$partial"
    mv -- "$partial" "$destination"
    rm -- "$source"
    echo "Homestock backup sync: uploaded $name"
done

if [ "$sync_photos" = true ] && [ -d "${data_dir%/}/photos" ]; then
    mkdir -p -- "${backup_dir%/}/photos"
    for source in "${data_dir%/}"/photos/*; do
        [ -f "$source" ] || continue
        name=${source##*/}
        destination="${backup_dir%/}/photos/$name"
        [ -e "$destination" ] && continue

        partial="${destination}.part"
        rm -f -- "$partial"
        cp -- "$source" "$partial"
        sync -f "$partial"
        mv -- "$partial" "$destination"
        echo "Homestock backup sync: uploaded photo $name"
    done
fi

files=$(find "$backup_dir" -maxdepth 1 -type f -name 'homestock-*.sqlite3' -printf '%f\n' | LC_ALL=C sort -r)
printf '%s\n' "$files" | tail -n "+$((keep + 1))" | while IFS= read -r name; do
    [ -n "$name" ] || continue
    rm -- "${backup_dir%/}/$name"
done
