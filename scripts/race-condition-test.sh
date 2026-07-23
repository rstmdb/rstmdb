#!/bin/bash
# Race Condition Test Script for rstmdb
# Tests concurrent operations to detect potential race conditions

set -e

# Configuration
RSTMDB_CLI="${RSTMDB_CLI:-./target/release/rstmdb-cli}"
RSTMDB_SERVER="${RSTMDB_SERVER:-127.0.0.1:7401}"
RSTMDB_TOKEN="${RSTMDB_TOKEN:-my-secret-token}"
MACHINE_NAME="race_test_machine"
MACHINE_VERSION=1
NUM_INSTANCES="${NUM_INSTANCES:-10}"
NUM_EVENTS_PER_INSTANCE="${NUM_EVENTS_PER_INSTANCE:-20}"
PARALLEL_JOBS="${PARALLEL_JOBS:-5}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

cli() {
    $RSTMDB_CLI -s "$RSTMDB_SERVER" -t "$RSTMDB_TOKEN" "$@"
}

# Cleanup function
cleanup() {
    log_info "Test completed."
}

trap cleanup EXIT

# Check if CLI exists
if [[ ! -x "$RSTMDB_CLI" ]]; then
    log_error "CLI not found at $RSTMDB_CLI"
    log_info "Build with: cargo build --release -p rstmdb-cli"
    exit 1
fi

# Check connection
log_info "Checking connection to rstmdb at $RSTMDB_SERVER..."
if ! cli info > /dev/null 2>&1; then
    log_error "Cannot connect to rstmdb server"
    exit 1
fi
log_info "Connected successfully"

# Create test machine
log_info "Setting up test machine: $MACHINE_NAME"
cli put-machine -n "$MACHINE_NAME" -v $MACHINE_VERSION '{
    "states": ["pending", "processing", "completed", "failed"],
    "initial": "pending",
    "transitions": [
        {"from": "pending", "event": "START", "to": "processing"},
        {"from": "processing", "event": "COMPLETE", "to": "completed"},
        {"from": "processing", "event": "FAIL", "to": "failed"},
        {"from": "failed", "event": "RETRY", "to": "pending"}
    ]
}' 2>/dev/null || log_info "(machine already exists)"

echo ""
echo "======================================"
echo "  Race Condition Test Suite"
echo "======================================"
echo "  Instances: $NUM_INSTANCES"
echo "  Events per instance: $NUM_EVENTS_PER_INSTANCE"
echo "  Parallel jobs: $PARALLEL_JOBS"
echo "======================================"
echo ""

# Test 1: Concurrent Instance Creation
log_info "Test 1: Concurrent Instance Creation"
log_info "Creating $NUM_INSTANCES instances in parallel..."

TEMP_DIR=$(mktemp -d)
INSTANCE_IDS=()

for i in $(seq 1 $NUM_INSTANCES); do
    (
        instance_id="race_test_$(date +%s%N)_$i"
        if $RSTMDB_CLI -s "$RSTMDB_SERVER" -t "$RSTMDB_TOKEN" \
            create-instance -m "$MACHINE_NAME" -V $MACHINE_VERSION -i "$instance_id" -c "{\"test_id\": $i}" > /dev/null 2>&1; then
            echo "$instance_id" >> "$TEMP_DIR/instances.txt"
            echo "OK: $instance_id"
        else
            echo "FAIL: $i"
        fi
    ) &

    # Limit parallel jobs
    if (( i % PARALLEL_JOBS == 0 )); then
        wait
    fi
done
wait

if [[ -f "$TEMP_DIR/instances.txt" ]]; then
    while IFS= read -r line; do
        INSTANCE_IDS+=("$line")
    done < "$TEMP_DIR/instances.txt"
fi

CREATED_COUNT=${#INSTANCE_IDS[@]}
log_info "Created $CREATED_COUNT / $NUM_INSTANCES instances"

if [[ $CREATED_COUNT -lt $NUM_INSTANCES ]]; then
    log_warn "Some instances failed to create"
fi

# Test 2: Concurrent Events on Same Instance
log_info ""
log_info "Test 2: Concurrent Events on Same Instance"

if [[ ${#INSTANCE_IDS[@]} -gt 0 ]]; then
    TARGET_INSTANCE="${INSTANCE_IDS[0]}"
    log_info "Sending $NUM_EVENTS_PER_INSTANCE events to instance: $TARGET_INSTANCE"

    for i in $(seq 1 $NUM_EVENTS_PER_INSTANCE); do
        (
            event="START"
            case $((i % 4)) in
                0) event="START" ;;
                1) event="COMPLETE" ;;
                2) event="FAIL" ;;
                3) event="RETRY" ;;
            esac
            $RSTMDB_CLI -s "$RSTMDB_SERVER" -t "$RSTMDB_TOKEN" \
                apply-event -i "$TARGET_INSTANCE" -e "$event" -p "{\"seq\": $i}" > /dev/null 2>&1
        ) &

        if (( i % PARALLEL_JOBS == 0 )); then
            wait
        fi
    done
    wait

    # Check final state
    log_info "Checking instance state..."
    FINAL_STATE=$(cli get-instance "$TARGET_INSTANCE" 2>&1)
    log_info "Final state: $(echo "$FINAL_STATE" | grep -o '"state":"[^"]*"' | head -1 || echo "unknown")"
else
    log_warn "No instances available for this test"
fi

# Test 3: Concurrent Events on Multiple Instances
log_info ""
log_info "Test 3: Concurrent Events on Multiple Instances"

if [[ ${#INSTANCE_IDS[@]} -gt 1 ]]; then
    log_info "Applying events to ${#INSTANCE_IDS[@]} instances concurrently..."

    TOTAL_EVENTS=$((NUM_INSTANCES * 5))
    log_info "Sending $TOTAL_EVENTS events across all instances..."

    for i in $(seq 1 $TOTAL_EVENTS); do
        (
            random_idx=$((RANDOM % ${#INSTANCE_IDS[@]}))
            random_instance="${INSTANCE_IDS[$random_idx]}"
            event="START"
            case $((RANDOM % 4)) in
                0) event="START" ;;
                1) event="COMPLETE" ;;
                2) event="FAIL" ;;
                3) event="RETRY" ;;
            esac
            $RSTMDB_CLI -s "$RSTMDB_SERVER" -t "$RSTMDB_TOKEN" \
                apply-event -i "$random_instance" -e "$event" -p "{\"batch\": $i}" > /dev/null 2>&1
        ) &

        if (( i % PARALLEL_JOBS == 0 )); then
            wait
        fi
        echo -n "."
    done
    wait
    echo ""
    log_info "Completed sending events"
else
    log_warn "Need more than 1 instance for this test"
fi

# Test 4: WAL Consistency Check
log_info ""
log_info "Test 4: WAL Consistency Check"

WAL_ENTRIES=$(cli wal-read -l 100 2>&1)
WAL_COUNT=$(echo "$WAL_ENTRIES" | grep -c '"offset"' || echo "0")
log_info "WAL entries (last 100): $WAL_COUNT"

# Test 5: Rapid Fire Single Instance
log_info ""
log_info "Test 5: Rapid Fire - 50 events as fast as possible"

RAPID_INSTANCE="rapid_test_$(date +%s)"
cli create-instance -m "$MACHINE_NAME" -V $MACHINE_VERSION -i "$RAPID_INSTANCE" -c '{"rapid": true}' > /dev/null 2>&1

START_TIME=$(date +%s%N)
for i in $(seq 1 50); do
    cli apply-event -i "$RAPID_INSTANCE" -e "START" -p "{\"rapid\": $i}" > /dev/null 2>&1 &
done
wait
END_TIME=$(date +%s%N)

ELAPSED=$(( (END_TIME - START_TIME) / 1000000 ))
log_info "50 concurrent events completed in ${ELAPSED}ms"

# Verify rapid fire instance state
RAPID_STATE=$(cli get-instance "$RAPID_INSTANCE" 2>&1)
log_info "Rapid fire instance state: $(echo "$RAPID_STATE" | grep -o '"state":"[^"]*"' | head -1 || echo "unknown")"

# Summary
echo ""
echo "======================================"
echo "  Test Summary"
echo "======================================"
echo "  Instances created: $CREATED_COUNT"
echo "  All tests completed"
echo ""
log_info "Check server logs for any errors or warnings"

# Cleanup temp dir
rm -rf "$TEMP_DIR"
