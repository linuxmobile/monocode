#!/usr/bin/env bash
# Measures the host's real-world performance on this machine.
#
# These numbers are baselines, not targets: they exist so a regression is
# visible. Everything here drives the actual binary over its real HTTP
# protocol, because a benchmark that bypasses the protocol measures the wrong
# thing. No agent is installed or needed.
#
# Usage: scripts/perf/baseline.sh [binary]
set -euo pipefail

BINARY="${1:-crates/monocode-gpui/target/native-dev/monocode-host}"
if [ ! -x "$BINARY" ]; then
    echo "No executable at $BINARY. Build one first: cargo xtask build" >&2
    exit 2
fi

WORK="$(mktemp -d)"
cleanup() {
    [ -n "${HOST_PID:-}" ] && kill "$HOST_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

port_of() {
    # `serve --port 0` binds an OS-chosen port and records it. Waiting for this
    # file is the only reliable readiness signal.
    local dir="$1"
    for _ in $(seq 1 600); do
        [ -f "$dir/running.json" ] && return 0
        sleep 0.005
    done
    echo "the host did not start" >&2
    return 1
}

echo "binary: $BINARY"
echo

# --- startup ---------------------------------------------------------------
total=0
runs=5
for index in $(seq 1 $runs); do
    dir="$WORK/start-$index"
    mkdir -p "$dir"
    start=$(date +%s%N)
    "$BINARY" serve --data-dir "$dir" --port 0 >/dev/null 2>&1 &
    pid=$!
    port_of "$dir"
    end=$(date +%s%N)
    ms=$(( (end - start) / 1000000 ))
    total=$((total + ms))
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
done
echo "startup to readiness: $((total / runs)) ms (mean of $runs)"

# --- idle footprint --------------------------------------------------------
IDLE="$WORK/idle"
mkdir -p "$IDLE"
"$BINARY" serve --data-dir "$IDLE" --port 0 >/dev/null 2>&1 &
HOST_PID=$!
port_of "$IDLE"
# Let background threads settle so the CPU figure is not startup work.
sleep 3
ps -o rss=,pcpu= -p "$HOST_PID" | awk '{printf "idle RSS: %.1f MiB\nidle CPU: %s%%\n", $1/1024, $2}'
echo "threads: $(ls "/proc/$HOST_PID/task" | wc -l)"

PORT=$(python3 -c "import json;print(json.load(open('$IDLE/running.json'))['port'])")
TOKEN=$("$BINARY" pair --data-dir "$IDLE" --label benchmark | python3 -c "import json,sys;print(json.load(sys.stdin)['token'])")
ENVIRONMENT=$("$BINARY" pair --data-dir "$IDLE" --label benchmark2 | python3 -c "import json,sys;print(json.load(sys.stdin)['environment_id'])")

# --- protocol throughput and file indexing ---------------------------------
python3 - "$PORT" "$TOKEN" "$ENVIRONMENT" "$WORK" <<'PY'
import http.client, json, os, sys, time

port, token, environment, work = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]

def rpc(method, params):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
    connection.request(
        "POST", "/rpc",
        json.dumps({"version": 1, "environmentId": environment, "method": method, "params": params}),
        {"Authorization": "Bearer " + token, "Content-Type": "application/json"},
    )
    response = connection.getresponse()
    body = json.loads(response.read())
    connection.close()
    return response.status, body

project = os.path.join(work, "project")
os.makedirs(project, exist_ok=True)
status, body = rpc("projects.open", {"cwd": project})
project_id = body["result"]["id"]

# An empty round trip: the floor for any UI action.
count = 500
started = time.time()
for _ in range(count):
    rpc("sessions.list", {"projectId": project_id})
elapsed = time.time() - started
print(f"rpc round trip: {elapsed/count*1000:.2f} ms/call (sessions.list, {count} calls)")

# Indexing a tree of a realistic size.
for index in range(500):
    directory = os.path.join(project, f"dir{index % 20}")
    os.makedirs(directory, exist_ok=True)
    open(os.path.join(directory, f"file{index}.txt"), "w").write("x" * 200)
started = time.time()
status, body = rpc("files.index", {"projectId": project_id})
elapsed = time.time() - started
print(f"files.index: {elapsed*1000:.0f} ms for {len(body.get('result') or [])} files")

# Reading one file repeatedly: the most common workspace operation.
target = os.path.join(project, "dir0", "file0.txt")
count = 200
started = time.time()
for _ in range(count):
    rpc("files.read", {"projectId": project_id, "path": "dir0/file0.txt"})
elapsed = time.time() - started
print(f"files.read: {elapsed/count*1000:.2f} ms/call ({count} calls)")
PY

echo
echo "Note: these are absolute baselines for this machine. Compare runs on the"
echo "same hardware; absolute values differ across machines."
