#!/bin/bash
# Stress Test Script for rstmdb
# Simple concurrent load test

set -e

CLI="${RSTMDB_CLI:-./target/release/rstmdb-cli}"
SERVER="${RSTMDB_SERVER:-127.0.0.1:7401}"
TOKEN="${RSTMDB_TOKEN:-my-secret-token}"

cli() { $CLI -s "$SERVER" -t "$TOKEN" "$@"; }

echo "=== rstmdb Stress Test ==="
echo ""

# Check connection
if ! cli info > /dev/null 2>&1; then
    echo "ERROR: Cannot connect to rstmdb at $SERVER"
    exit 1
fi
echo "Connected to $SERVER"

# Create test machine
MACHINE="stress_test"
VERSION=1

echo ""
echo "1. Setting up test machine..."
cli put-machine -n "$MACHINE" -v $VERSION '{
    "states": ["a", "b", "c"],
    "initial": "a",
    "transitions": [
        {"from": "a", "event": "GO", "to": "b"},
        {"from": "b", "event": "GO", "to": "c"},
        {"from": "c", "event": "GO", "to": "a"}
    ]
}' 2>/dev/null || echo "   (machine already exists)"

echo ""
echo "2. Testing concurrent instance creation..."
PIDS=()
INSTANCE_IDS=()
for i in $(seq 1 20); do
    ID="stress_$RANDOM$i"
    INSTANCE_IDS+=("$ID")
    cli create-instance -m "$MACHINE" -V $VERSION -i "$ID" -c '{}' > /dev/null 2>&1 &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait $pid 2>/dev/null || true; done
echo "   Done - attempted 20 instances"

echo ""
echo "3. Testing concurrent events on single instance..."
INSTANCE="stress_single_$(date +%s)"
cli create-instance -m "$MACHINE" -V $VERSION -i "$INSTANCE" -c '{}' > /dev/null 2>&1

PIDS=()
for i in $(seq 1 50); do
    cli apply-event -i "$INSTANCE" -e "GO" -p "{\"i\":$i}" > /dev/null 2>&1 &
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait $pid 2>/dev/null || true; done

INSTANCE_DATA=$(cli get-instance "$INSTANCE" 2>&1)
echo "   Done - instance state: $(echo "$INSTANCE_DATA" | grep -o '"state":"[^"]*"' | head -1 || echo 'unknown')"

echo ""
echo "4. Testing mixed operations..."
PIDS=()
for i in $(seq 1 30); do
    case $((i % 3)) in
        0) cli create-instance -m "$MACHINE" -V $VERSION -i "mix_$RANDOM$i" -c '{}' > /dev/null 2>&1 & ;;
        1) cli apply-event -i "$INSTANCE" -e "GO" -p '{}' > /dev/null 2>&1 & ;;
        2) cli get-instance "$INSTANCE" > /dev/null 2>&1 & ;;
    esac
    PIDS+=($!)
done
for pid in "${PIDS[@]}"; do wait $pid 2>/dev/null || true; done
echo "   Done - 30 mixed operations"

echo ""
echo "5. Checking WAL..."
WAL=$(cli wal-read -l 50 2>&1)
WAL_COUNT=$(echo "$WAL" | grep -c '"offset"' || echo 0)
echo "   WAL has $WAL_COUNT recent entries"

echo ""
echo "=== Stress Test Complete ==="
echo "Check server logs for errors"
