#!/usr/bin/env bash
#
# Load test + auto-compaction test for the replication cluster.
#
# Pumps configurable write volume at the primary, watches replication keep up
# across replicas, then verifies auto-compaction reduces WAL size.
#
# Usage:
#   ./scripts/load-replication.sh                          # default workload
#   ./scripts/load-replication.sh --instances 500 --events 20 --workers 16
#   ./scripts/load-replication.sh --reset --force-compact  # clean + manual compact
#   ./scripts/load-replication.sh --compact-threshold 500  # trigger auto-compaction sooner
#
# Targets the docker-compose cluster on 127.0.0.1:7401 (primary), :7402/:7403 (replicas).
# Override with RSTMDB_PRIMARY / RSTMDB_REPLICAS env vars.

set -euo pipefail

# ---------- defaults ----------
CLI="${RSTMDB_CLI:-$(dirname "$0")/../target/release/rstmdb-cli}"
PRIMARY="${RSTMDB_PRIMARY:-127.0.0.1:7401}"
REPLICAS="${RSTMDB_REPLICAS:-127.0.0.1:7402,127.0.0.1:7403}"
PRIMARY_METRICS="${RSTMDB_PRIMARY_METRICS:-127.0.0.1:9090}"
REPLICAS_METRICS="${RSTMDB_REPLICAS_METRICS:-127.0.0.1:9091,127.0.0.1:9092}"
TOKEN="${RSTMDB_TOKEN:-load-test-token}"

INSTANCES=200
EVENTS_PER_INSTANCE=10
WORKERS=16
MACHINE="load_test"
RESET=0
FORCE_COMPACT=0
CONVERGE_TIMEOUT=60

# ---------- colors ----------
if [[ -t 1 ]]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
    BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
else
    RED=; GREEN=; YELLOW=; BLUE=; BOLD=; NC=
fi

log()    { echo -e "${BLUE}[$(date +%H:%M:%S)]${NC} $*"; }
ok()     { echo -e "${GREEN}✓${NC} $*"; }
warn()   { echo -e "${YELLOW}⚠${NC} $*"; }
fail()   { echo -e "${RED}✗${NC} $*"; }
section(){ echo -e "\n${BOLD}=== $* ===${NC}"; }

# ---------- arg parsing ----------
usage() {
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage ;;
        --instances) INSTANCES="$2"; shift 2 ;;
        --events) EVENTS_PER_INSTANCE="$2"; shift 2 ;;
        --workers) WORKERS="$2"; shift 2 ;;
        --reset) RESET=1; shift ;;
        --force-compact) FORCE_COMPACT=1; shift ;;
        --machine) MACHINE="$2"; shift 2 ;;
        --converge-timeout) CONVERGE_TIMEOUT="$2"; shift 2 ;;
        *) fail "unknown arg: $1"; usage ;;
    esac
done

# ---------- helpers ----------
cli()    { "$CLI" -s "$PRIMARY" "$@"; }
cli_at() { "$CLI" -s "$1" "${@:2}"; }

get_metric() {
    # get_metric <host:port> <metric_name>
    curl -sS --max-time 2 "http://$1/metrics" 2>/dev/null | awk -v m="$2" '$1==m{print $2; exit}'
}

array_from_csv() {
    IFS=',' read -r -a _arr <<< "$1"
    echo "${_arr[@]}"
}

# Status line printer for lag watch
print_state_row() {
    # print_state_row <label> <host:port> <metrics host:port>
    local label="$1" host="$2" mhost="$3"
    local entries instances machines size
    entries=$(get_metric "$mhost" "rstmdb_wal_entries")
    instances=$(get_metric "$mhost" "rstmdb_instances_total")
    machines=$(get_metric "$mhost" "rstmdb_machines_total")
    size=$(get_metric "$mhost" "rstmdb_wal_size_bytes")
    printf "  %-10s entries=%-8s instances=%-8s machines=%-3s wal_bytes=%s\n" \
        "$label" "${entries:-?}" "${instances:-?}" "${machines:-?}" "${size:-?}"
}

# ---------- preflight ----------
section "Preflight"
log "CLI      : $CLI"
log "Primary  : $PRIMARY (metrics $PRIMARY_METRICS)"
log "Replicas : $REPLICAS (metrics $REPLICAS_METRICS)"
log "Workload : $INSTANCES instances × $EVENTS_PER_INSTANCE events = $((INSTANCES * (EVENTS_PER_INSTANCE + 1))) writes"
log "Workers  : $WORKERS concurrent"
log "Reset    : $([ $RESET -eq 1 ] && echo yes || echo no)"

if [[ ! -x "$CLI" ]]; then
    fail "CLI binary not found/executable: $CLI"
    fail "Build with: cargo build --release -p rstmdb-cli"
    exit 1
fi

if ! cli ping >/dev/null 2>&1; then
    fail "Cannot ping primary at $PRIMARY"
    exit 1
fi
ok "primary reachable"

# Check replicas
IFS=',' read -r -a REPLICA_ARR <<< "$REPLICAS"
IFS=',' read -r -a REPLICA_METRICS_ARR <<< "$REPLICAS_METRICS"
for r in "${REPLICA_ARR[@]}"; do
    if cli_at "$r" ping >/dev/null 2>&1; then
        ok "replica reachable: $r"
    else
        warn "replica NOT reachable: $r (test will continue without checking it)"
    fi
done

# ---------- reset ----------
if [[ $RESET -eq 1 ]]; then
    section "Reset"
    if cli flush-all >/dev/null 2>&1; then
        ok "primary flushed"
    else
        warn "flush-all failed (likely disabled in config)"
    fi
fi

# ---------- setup machine ----------
section "Setup"
DEF='{
  "states": ["created","active","done"],
  "initial": "created",
  "transitions": [
    {"from":"created","event":"START","to":"active"},
    {"from":"active","event":"STEP","to":"active"},
    {"from":"active","event":"FINISH","to":"done"}
  ]
}'
if cli put-machine -n "$MACHINE" -v 1 "$DEF" >/dev/null 2>&1; then
    ok "machine '$MACHINE v1' registered"
else
    ok "machine '$MACHINE v1' already exists (idempotent)"
fi

# ---------- record starting metrics ----------
START_ENTRIES=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_entries")
START_SEGMENTS=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_segments")
START_SIZE=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_size_bytes")
log "baseline: entries=$START_ENTRIES segments=$START_SEGMENTS size_bytes=$START_SIZE"

# ---------- workload ----------
section "Workload"
log "Running $INSTANCES create + $((INSTANCES * EVENTS_PER_INSTANCE)) apply_event, $WORKERS workers..."

WORK_DIR=$(mktemp -d)
trap "rm -rf '$WORK_DIR'" EXIT

# Two-phase workload to avoid xargs parallelism reordering per-instance ops:
#   Phase 1: all create-instance calls (no inter-dependency)
#   Phase 2: all apply-event calls (depend on create, but safe once phase 1 is done)
CREATES_FILE="$WORK_DIR/creates.txt"
# Events are split into two order-independent phases so they can run
# concurrently without racing per-instance transition order: every instance
# must be STARTed (created → active) before any STEP (active → active). Running
# all STARTs first, then all STEPs, means each phase is safe to parallelize —
# interleaving them (the old single events file) let a STEP fire before its
# START and fail with an invalid-transition error.
STARTS_FILE="$WORK_DIR/starts.txt"
STEPS_FILE="$WORK_DIR/steps.txt"
INSTANCE_IDS=()
RUN_STAMP=$(date +%s)-$$
for i in $(seq 1 "$INSTANCES"); do
    ID="load-$RUN_STAMP-$i"
    INSTANCE_IDS+=("$ID")
    echo "$ID" >> "$CREATES_FILE"
    echo "$ID|START" >> "$STARTS_FILE"
    for _ in $(seq 2 "$EVENTS_PER_INSTANCE"); do
        echo "$ID|STEP" >> "$STEPS_FILE"
    done
done

CREATE_COUNT=$(wc -l < "$CREATES_FILE")
EVENT_COUNT=$(( $(wc -l < "$STARTS_FILE" 2>/dev/null || echo 0) + $(wc -l < "$STEPS_FILE" 2>/dev/null || echo 0) ))
TOTAL_OPS=$((CREATE_COUNT + EVENT_COUNT))
log "Generated $CREATE_COUNT creates + $EVENT_COUNT events = $TOTAL_OPS operations"

FAIL_FILE="$WORK_DIR/failures"
: > "$FAIL_FILE"

# Self-contained worker scripts invoked DIRECTLY by xargs. We deliberately do
# NOT use `export -f fn` + `xargs bash -c 'fn'`: that pattern silently fails to
# run the worker under macOS's stock bash 3.2 (exported functions aren't
# imported into the xargs child), so the whole workload no-ops while reporting
# "no failures". Baking config into a plain script and calling it directly is
# portable across bash/dash/zsh. Config values are expanded at write time; the
# per-item argument (`$1`) stays literal.
CREATE_HELPER="$WORK_DIR/do_create.sh"
cat > "$CREATE_HELPER" <<HELPER
#!/usr/bin/env bash
"$CLI" -s "$PRIMARY" create-instance -m "$MACHINE" -V 1 -i "\$1" >/dev/null 2>&1 \\
    || echo "C" >> "$FAIL_FILE"
HELPER
EVENT_HELPER="$WORK_DIR/do_event.sh"
cat > "$EVENT_HELPER" <<HELPER
#!/usr/bin/env bash
arg="\$1"; id="\${arg%%|*}"; ev="\${arg#*|}"
"$CLI" -s "$PRIMARY" apply-event -i "\$id" -e "\$ev" >/dev/null 2>&1 \\
    || echo "E" >> "$FAIL_FILE"
HELPER
chmod +x "$CREATE_HELPER" "$EVENT_HELPER"

START_TIME=$(date +%s)

# Phase 1: creates.
# NOTE: read the work list from STDIN (`< file`), NOT `xargs -a file` — the
# `-a` flag is a GNU extension that BSD/macOS xargs rejects with "invalid
# option -- a", which (under `2>/dev/null || true`) silently ran zero ops.
log "  phase 1: creating $CREATE_COUNT instances..."
xargs -P "$WORKERS" -I{} "$CREATE_HELPER" {} < "$CREATES_FILE" 2>/dev/null || true

# Phase 2a: START every instance (created -> active). One per instance, so safe
# to run concurrently.
log "  phase 2a: starting instances (START)..."
[[ -s "$STARTS_FILE" ]] && \
    xargs -P "$WORKERS" -I{} "$EVENT_HELPER" {} < "$STARTS_FILE" 2>/dev/null || true

# Phase 2b: STEP (active -> active). Safe now that every instance is active;
# concurrent STEPs on the same instance are all valid.
log "  phase 2b: applying STEP events..."
[[ -s "$STEPS_FILE" ]] && \
    xargs -P "$WORKERS" -I{} "$EVENT_HELPER" {} < "$STEPS_FILE" 2>/dev/null || true

END_TIME=$(date +%s)
ELAPSED=$((END_TIME - START_TIME))
[[ $ELAPSED -lt 1 ]] && ELAPSED=1
RATE=$((TOTAL_OPS / ELAPSED))
FAILURES=$(wc -l < "$FAIL_FILE" 2>/dev/null || echo 0)
CREATE_FAILS=$(grep -c "^C$" "$FAIL_FILE" 2>/dev/null || echo 0)
EVENT_FAILS=$(grep -c "^E$" "$FAIL_FILE" 2>/dev/null || echo 0)

if [[ $FAILURES -eq 0 ]]; then
    ok "Submitted $TOTAL_OPS ops in ${ELAPSED}s (~${RATE}/s, no failures)"
else
    warn "Submitted $TOTAL_OPS ops in ${ELAPSED}s (~${RATE}/s, failures: $CREATE_FAILS create + $EVENT_FAILS event = $FAILURES)"
fi

# Guard: verify the workload actually LANDED on the primary. Catches the class
# of failure where ops "succeed" but nothing is written (e.g. a broken worker
# invocation) — otherwise the convergence check below just waits out its timeout
# on a primary that never grew.
sleep 1
POST_ENTRIES=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_entries")
GROWTH=$(( ${POST_ENTRIES:-0} - ${START_ENTRIES:-0} ))
EXPECTED_MIN=$(( TOTAL_OPS / 2 ))  # allow for legit idempotency/no-op events
if [[ $GROWTH -lt $EXPECTED_MIN ]]; then
    fail "workload did not land: primary WAL grew by $GROWTH (expected ~$TOTAL_OPS). \
Writes were not applied — aborting before the convergence check."
    exit 1
fi
log "workload landed: primary WAL grew by $GROWTH entries"

# ---------- watch replication converge ----------
section "Replication convergence"
log "Waiting for replicas to catch up (timeout=${CONVERGE_TIMEOUT}s)..."

# Gauges are refreshed every 5s on each node (not synchronised). Wait one full
# cycle BEFORE checking so reads don't hit a stale snapshot. Then read repeatedly
# until all three agree AND the count moves past the pre-workload baseline.
# Using `rstmdb_wal_writes_total` (a Counter derived from WAL stats deltas)
# since `rstmdb_wal_entries` gauge can lag more than the counter.
sleep 6

converged=0
for elapsed in $(seq 0 2 "$CONVERGE_TIMEOUT"); do
    primary_entries=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_entries")
    all_match=1
    for mh in "${REPLICA_METRICS_ARR[@]}"; do
        r_entries=$(get_metric "$mh" "rstmdb_wal_entries")
        if [[ -z "$r_entries" ]] || [[ "$r_entries" != "$primary_entries" ]]; then
            all_match=0
            break
        fi
    done
    # Also verify count grew past baseline (stale-gauge protection)
    if [[ $all_match -eq 1 ]] && [[ -n "$primary_entries" ]] && \
       (( primary_entries > START_ENTRIES )); then
        converged=1
        ok "converged in ${elapsed}s (primary=${primary_entries} entries, was ${START_ENTRIES})"
        break
    fi
    if (( elapsed % 4 == 0 )); then
        local_replicas=""
        for mh in "${REPLICA_METRICS_ARR[@]}"; do
            r=$(get_metric "$mh" "rstmdb_wal_entries")
            local_replicas="$local_replicas $r"
        done
        log "  [+${elapsed}s] primary=${primary_entries} replicas=${local_replicas}"
    fi
    sleep 2
done

if [[ $converged -ne 1 ]]; then
    fail "did not converge within ${CONVERGE_TIMEOUT}s"
fi

# ---------- node-by-node state ----------
section "Per-node state"
print_state_row "primary" "$PRIMARY" "$PRIMARY_METRICS"
for i in "${!REPLICA_ARR[@]}"; do
    print_state_row "replica$((i+1))" "${REPLICA_ARR[$i]}" "${REPLICA_METRICS_ARR[$i]}"
done

echo ""
echo "Replication metrics (primary):"
curl -sS "http://$PRIMARY_METRICS/metrics" 2>/dev/null | awk '/^rstmdb_replication/{print "  "$0}'

# ---------- WAL growth report ----------
section "WAL growth"
END_ENTRIES=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_entries")
END_SEGMENTS=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_segments")
END_SIZE=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_size_bytes")
printf "  entries:  %s → %s  (Δ %s)\n" "$START_ENTRIES" "$END_ENTRIES" \
    "$(( END_ENTRIES - START_ENTRIES ))"
printf "  segments: %s → %s\n" "$START_SEGMENTS" "$END_SEGMENTS"
printf "  size:     %s → %s bytes\n" "$START_SIZE" "$END_SIZE"

# ---------- optional manual compaction ----------
if [[ $FORCE_COMPACT -eq 1 ]]; then
    section "Manual compaction"
    BEFORE_SEGS=$END_SEGMENTS
    BEFORE_SIZE=$END_SIZE
    log "Triggering compact on primary..."
    if compact_out=$(cli compact -f 2>&1); then
        ok "compaction completed"
        echo "$compact_out" | sed 's/^/    /'
    else
        warn "compact command failed:"
        echo "$compact_out" | sed 's/^/    /'
    fi
    sleep 6
    AFTER_SEGS=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_segments")
    AFTER_SIZE=$(get_metric "$PRIMARY_METRICS" "rstmdb_wal_size_bytes")
    printf "  segments: %s → %s\n" "$BEFORE_SEGS" "$AFTER_SEGS"
    printf "  size:     %s → %s bytes\n" "$BEFORE_SIZE" "$AFTER_SIZE"
    if [[ -n "$AFTER_SEGS" ]] && [[ -n "$BEFORE_SEGS" ]] && (( AFTER_SEGS < BEFORE_SEGS )); then
        ok "compaction reduced segment count"
    elif [[ -n "$AFTER_SIZE" ]] && [[ -n "$BEFORE_SIZE" ]] && (( AFTER_SIZE < BEFORE_SIZE )); then
        ok "compaction reduced WAL size"
    else
        warn "compaction had no measurable effect (thresholds may not be met)"
    fi
fi

# ---------- parity verification ----------
section "Parity check"
# Pick a random instance, verify state on primary matches replicas
PROBE_ID="load-parity-probe-$(date +%s)"
if cli create-instance -m "$MACHINE" -V 1 -i "$PROBE_ID" >/dev/null 2>&1; then
    cli apply-event -i "$PROBE_ID" -e START >/dev/null 2>&1 || true
    sleep 3

    primary_state=$(cli get-instance "$PROBE_ID" 2>&1 | grep "State:" | awk '{print $2}')
    primary_offset=$(cli get-instance "$PROBE_ID" 2>&1 | grep "Last WAL offset:" | awk '{print $NF}')
    all_ok=1
    for r in "${REPLICA_ARR[@]}"; do
        r_state=$(cli_at "$r" get-instance "$PROBE_ID" 2>&1 | grep "State:" | awk '{print $2}' || echo "MISSING")
        r_offset=$(cli_at "$r" get-instance "$PROBE_ID" 2>&1 | grep "Last WAL offset:" | awk '{print $NF}' || echo "")
        if [[ "$r_state" == "$primary_state" ]] && [[ "$r_offset" == "$primary_offset" ]]; then
            ok "$r: state=$r_state offset=$r_offset (matches primary)"
        else
            fail "$r: state=$r_state offset=$r_offset (primary was state=$primary_state offset=$primary_offset)"
            all_ok=0
        fi
    done
    [[ $all_ok -eq 1 ]] && ok "all replicas in parity with primary"
fi

# ---------- summary ----------
section "Summary"
if [[ $converged -eq 1 ]] && [[ $FAILURES -eq 0 ]]; then
    echo -e "${GREEN}${BOLD}PASS${NC}: ${TOTAL_OPS} ops replicated to $(( ${#REPLICA_ARR[@]} )) replicas at ~${RATE}/s"
    exit 0
else
    echo -e "${RED}${BOLD}FAIL${NC}: converged=$converged failures=$FAILURES"
    exit 1
fi
