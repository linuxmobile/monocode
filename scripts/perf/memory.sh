#!/bin/sh
# Read a prepared store and log the app's resident memory while it idles.
#
#   scripts/perf/memory.sh <db> [seconds] [binary]
#
# <db> is a store copied from a real profile, so a large transcript, many
# sessions or a big image cache is what the run measures. The app opens it on
# throwaway data and the script prints RSS and thread count once a second.
set -eu
db=${1:?usage: memory.sh <db> [seconds] [binary]}
seconds=${2:-20}
app=${3:-crates/monocode-gpui/target/release/monocode-gpui}
work=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$work"' EXIT INT TERM
mkdir -p "$work/data" "$work/project"
cp "$db" "$work/data/monocode.db"
MONOCODE_NATIVE_DATA_DIR="$work/data" "$app" --dev-data --cwd "$work/project" \
  >"$work/app.log" 2>&1 &
pid=$!
# The window is up and the store is open by now.
sleep 6
i=0
while [ "$i" -lt "$seconds" ]; do
  rss=$(awk '/VmRSS/{print $2}' "/proc/$pid/status")
  threads=$(ls "/proc/$pid/task" | wc -l)
  echo "$i rss=$((rss / 1024))MiB threads=$threads"
  sleep 1
  i=$((i + 1))
done
