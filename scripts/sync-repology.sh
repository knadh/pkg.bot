#!/usr/bin/env bash
# Run from the app directory: ./scripts/sync-repology.sh /path/to/data [FORCE_IMPORT]
# Set FORCE_IMPORT to 1 to import even when the dump hasn't changed and download is skipped.
# Cron requires a sudoers rule (configured with visudo):
# pkgbot ALL=(root) NOPASSWD: /usr/bin/systemctl stop pkgbot.service, /usr/bin/systemctl start pkgbot.service
set -euo pipefail

log() {
    printf '[%s] %s\n' "$(date --iso-8601=seconds)" "$*"
}

log "starting repology sync"

dir=$(realpath "${1:?usage: $0 DATA_DIR [FORCE_IMPORT]}")
force_import=${2:-0}
dump="$dir/repology-database-dump-latest.sql.zst"
pending="$dump.pending"
sync_dir="$dir/.sync"
tmp="$sync_dir/data.db"
stopped=false
backed_up=false

[[ -f "$dir/data.db" ]] || { log "missing $dir/data.db" >&2; exit 1; }
mkdir -p "$sync_dir"
export TMPDIR="$sync_dir"
export SQLITE_TMPDIR="$sync_dir"

# Lock syncs for this data directory.
exec 9>"$sync_dir/sync-repology.lock"
flock -n 9 || { log "repology sync is already running; skipping"; exit 0; }

cleanup() {
    status=$?
    trap - EXIT
    if (( status != 0 )); then
        log "repology sync failed (exit status $status); cleaning up" >&2
    fi
    if $stopped; then
        if $backed_up; then
            log "restoring the previous database from backup"
            mv -f "$dir/data.db.bak" "$dir/data.db" || true
        fi
        log "restarting pkgbot during cleanup"
        sudo -n /usr/bin/systemctl start pkgbot.service || true
    fi
    rm -f "$dump.part" "$dir/data.db.new"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Download the file and retain timestamps for cron freshness checks.
# Skip an unchanged dump unless an import is forced or needs retrying.
condition=()
[[ ! -f "$dump" ]] || condition=(--time-cond "$dump")
log "starting download (checking for a newer dump if one already exists)"
code=$(curl --fail --location --show-error --no-silent --remote-time \
    "${condition[@]}" --output "$dump.part" --write-out '%{http_code}' \
    https://dumps.repology.org/repology-database-dump-latest.sql.zst)

case "$code" in
    200)
        touch "$pending"
        mv -f "$dump.part" "$dump"
        log "download complete: $dump"
        ;;
    304)
        if [[ "$force_import" == 1 ]]; then
            log "dump unchanged; forcing import"
            touch "$pending"
        elif [[ -f "$pending" ]]; then
            log "retrying the previous unfinished import"
        else
            log "no new repology dump; exiting"
            exit 0
        fi
        ;;
    *) log "unexpected HTTP status: $code" >&2; exit 1 ;;
esac

# Remove the old PG volume completely.
log "removing the old PostgreSQL container and volume"
docker compose down db -v

# Wait for PG to be up and run the import.
log "starting PostgreSQL and waiting for it to be ready"
docker compose up -d --wait --wait-timeout 120 db
# Stream only the tables the importer needs, without writing an extracted SQL file.
log "restoring the repology dump into PostgreSQL"
zstd --decompress --stdout "$dump" | ./pkgbot restore-repology
log "repology dump restore complete"

# Run the import.
log "removing temporary SQLite database files and initializing a new database"
rm -f "$tmp" "$tmp-wal" "$tmp-shm"
./pkgbot --db="$tmp" install --yes
log "starting package import into SQLite"
./pkgbot --db="$tmp" import
log "package import complete"

# Stop docker once the import is done.
log "stopping PostgreSQL"
docker compose down db

# Step the new DB's attribs.
log "staging the new database and matching permissions and ownership"
mv -f "$tmp" "$dir/data.db.new"
chmod --reference="$dir/data.db" "$dir/data.db.new"
chown --reference="$dir/data.db" "$dir/data.db.new"

# Create a backup, stop the app, and replace the old DB with the new one.
log "stopping pkgbot"
stopped=true
sudo -n /usr/bin/systemctl stop pkgbot.service
log "backing up the current database and replacing it with the new database"
mv -f "$dir/data.db" "$dir/data.db.bak"
backed_up=true
mv -f "$dir/data.db.new" "$dir/data.db"

# Start!
log "starting pkgbot"
sudo -n /usr/bin/systemctl start pkgbot.service
stopped=false
rm -f "$pending"
log "repology sync complete"
