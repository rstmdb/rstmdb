#!/bin/bash

# rstmdb CLI Test Script
# Tests all commands using the rstmdb-cli tool
#
# Usage: ./test_commands.sh [server:port] [token] [tls-options]
# Default: 127.0.0.1:7401, my-secret-token
#
# TLS options:
#   --tls                       Enable TLS with insecure mode (skip cert verification)
#   --tls-ca PATH               Enable TLS with CA certificate
#   --mtls CA CERT KEY          Enable mutual TLS with CA cert, client cert, and client key
#
# Examples:
#   ./test_commands.sh                                    # Plain TCP
#   ./test_commands.sh 127.0.0.1:7401 my-token --tls      # TLS insecure
#   ./test_commands.sh 127.0.0.1:7401 my-token --tls-ca ./dev-certs/ca-cert.pem
#   ./test_commands.sh 127.0.0.1:7401 my-token --mtls ./dev-certs/ca-cert.pem ./dev-certs/client-cert.pem ./dev-certs/client-key.pem
#
# Or set via environment:
#   RSTMDB_TOKEN=my-secret-token ./test_commands.sh

SERVER="${1:-127.0.0.1:7401}"
TOKEN="${2:-${RSTMDB_TOKEN:-my-secret-token}}"
TLS_OPTS=""

# Parse TLS options from remaining arguments
shift 2 2>/dev/null || true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --tls)
            TLS_OPTS="--tls --insecure"
            shift
            ;;
        --tls-ca)
            TLS_OPTS="--tls --ca-cert $2"
            shift 2
            ;;
        --mtls)
            TLS_OPTS="--tls --ca-cert $2 --client-cert $3 --client-key $4"
            shift 4
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

CLI="cargo run -p rstmdb-cli --"
PASSED=0
FAILED=0

# Generate unique suffix for this test run
RUN_ID=$(date +%s)

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Run test with authentication
run_test() {
    local name="$1"
    shift

    echo -n "Testing: $name... "
    if OUTPUT=$($CLI -s "$SERVER" $TLS_OPTS -t "$TOKEN" "$@" 2>&1); then
        echo -e "${GREEN}PASS${NC}"
        PASSED=$((PASSED + 1))
        return 0
    else
        echo -e "${RED}FAIL${NC}"
        echo "  Command: $@"
        echo "  Output: $OUTPUT"
        FAILED=$((FAILED + 1))
        return 1
    fi
}

# Run test expecting failure (with authentication)
run_test_expect_fail() {
    local name="$1"
    shift

    echo -n "Testing: $name (expect error)... "
    if OUTPUT=$($CLI -s "$SERVER" $TLS_OPTS -t "$TOKEN" "$@" 2>&1); then
        echo -e "${RED}FAIL${NC} (expected error but succeeded)"
        echo "  Output: $OUTPUT"
        FAILED=$((FAILED + 1))
        return 1
    else
        echo -e "${GREEN}PASS${NC}"
        PASSED=$((PASSED + 1))
        return 0
    fi
}

# Run test WITHOUT authentication (for testing public commands)
run_test_no_auth() {
    local name="$1"
    shift

    echo -n "Testing: $name (no auth)... "
    if OUTPUT=$($CLI -s "$SERVER" $TLS_OPTS "$@" 2>&1); then
        echo -e "${GREEN}PASS${NC}"
        PASSED=$((PASSED + 1))
        return 0
    else
        echo -e "${RED}FAIL${NC}"
        echo "  Command: $@"
        echo "  Output: $OUTPUT"
        FAILED=$((FAILED + 1))
        return 1
    fi
}

# Run test expecting auth failure (no token provided)
run_test_auth_required() {
    local name="$1"
    shift

    echo -n "Testing: $name (expect auth required)... "
    if OUTPUT=$($CLI -s "$SERVER" $TLS_OPTS "$@" 2>&1); then
        echo -e "${RED}FAIL${NC} (expected auth error but succeeded)"
        echo "  Output: $OUTPUT"
        FAILED=$((FAILED + 1))
        return 1
    else
        if echo "$OUTPUT" | grep -qi "unauthorized\|authentication"; then
            echo -e "${GREEN}PASS${NC}"
            PASSED=$((PASSED + 1))
            return 0
        else
            echo -e "${RED}FAIL${NC} (wrong error type)"
            echo "  Output: $OUTPUT"
            FAILED=$((FAILED + 1))
            return 1
        fi
    fi
}

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}rstmdb CLI Test Suite${NC}"
echo -e "${BLUE}Server: $SERVER${NC}"
echo -e "${BLUE}Token: ${TOKEN:0:10}...${NC}"
if [ -n "$TLS_OPTS" ]; then
    echo -e "${BLUE}TLS: $TLS_OPTS${NC}"
else
    echo -e "${BLUE}TLS: disabled${NC}"
fi
echo -e "${BLUE}Run ID: $RUN_ID${NC}"
echo -e "${BLUE}========================================${NC}"
echo ""

# Build first
echo -e "${YELLOW}Building CLI...${NC}"
cargo build -p rstmdb-cli --quiet

# ============================================================================
echo -e "\n${YELLOW}=== Authentication ===${NC}"
# ============================================================================

# PING should work without auth (exempt command)
run_test_no_auth "PING without auth" ping

# INFO requires auth when auth is enabled
run_test_auth_required "INFO without auth" info

# With valid token
run_test "PING with auth" ping
run_test "INFO with auth" info

# ============================================================================
echo -e "\n${YELLOW}=== Machine Definitions ===${NC}"
# ============================================================================

# Order state machine (save to temp file to avoid shell quoting issues)
ORDER_DEF_FILE=$(mktemp)
cat > "$ORDER_DEF_FILE" << 'EOF'
{"states":["created","paid","shipped","delivered","cancelled"],"initial":"created","transitions":[{"from":"created","event":"PAY","to":"paid"},{"from":"created","event":"CANCEL","to":"cancelled"},{"from":"paid","event":"SHIP","to":"shipped"},{"from":"paid","event":"REFUND","to":"cancelled"},{"from":"shipped","event":"DELIVER","to":"delivered"}]}
EOF

run_test "PUT_MACHINE order v1" put-machine -n order -v 1 "@$ORDER_DEF_FILE"
run_test "PUT_MACHINE order v1 (idempotent)" put-machine -n order -v 1 "@$ORDER_DEF_FILE"

# Payment with guard (single transition per event)
PAYMENT_DEF_FILE=$(mktemp)
cat > "$PAYMENT_DEF_FILE" << 'EOF'
{"states":["pending","approved","rejected"],"initial":"pending","transitions":[{"from":"pending","event":"APPROVE","to":"approved","guard":"ctx.approved"},{"from":"pending","event":"REJECT","to":"rejected"}]}
EOF

run_test "PUT_MACHINE payment v1 (with guards)" put-machine -n payment -v 1 "@$PAYMENT_DEF_FILE"

run_test "GET_MACHINE order v1" get-machine -n order -v 1
run_test_expect_fail "GET_MACHINE nonexistent" get-machine -n nonexistent -v 1
run_test "LIST_MACHINES" list-machines

# Cleanup temp files
rm -f "$ORDER_DEF_FILE" "$PAYMENT_DEF_FILE"

# ============================================================================
echo -e "\n${YELLOW}=== Instance Lifecycle ===${NC}"
# ============================================================================

run_test "CREATE_INSTANCE order-001" create-instance -m order -V 1 -i "order-001-$RUN_ID" -c '{"customer":"alice","total":99.99}'
run_test "CREATE_INSTANCE order-002 (auto ctx)" create-instance -m order -V 1 -i "order-002-$RUN_ID"
run_test "CREATE_INSTANCE auto-id" create-instance -m order -V 1

run_test_expect_fail "CREATE_INSTANCE duplicate" create-instance -m order -V 1 -i "order-001-$RUN_ID"

run_test "GET_INSTANCE order-001" get-instance "order-001-$RUN_ID"
run_test_expect_fail "GET_INSTANCE nonexistent" get-instance nonexistent-999

# ============================================================================
echo -e "\n${YELLOW}=== Event Application ===${NC}"
# ============================================================================

run_test "APPLY_EVENT PAY" apply-event -i "order-001-$RUN_ID" -e PAY -p '{"payment_id":"pay-123"}'
run_test "GET_INSTANCE (verify paid)" get-instance "order-001-$RUN_ID"

run_test "APPLY_EVENT SHIP" apply-event -i "order-001-$RUN_ID" -e SHIP
run_test "APPLY_EVENT DELIVER (with expected_state)" apply-event -i "order-001-$RUN_ID" -e DELIVER --expected-state shipped

run_test_expect_fail "APPLY_EVENT invalid transition" apply-event -i "order-001-$RUN_ID" -e PAY
run_test_expect_fail "APPLY_EVENT wrong expected_state" apply-event -i "order-002-$RUN_ID" -e PAY --expected-state paid

# ============================================================================
echo -e "\n${YELLOW}=== Guard Expressions ===${NC}"
# ============================================================================

# Test guard: ctx.approved must be true
run_test "CREATE payment-not-approved" create-instance -m payment -V 1 -i "payment-not-approved-$RUN_ID" -c '{"approved":false}'
run_test_expect_fail "APPROVE payment-not-approved (guard fails)" apply-event -i "payment-not-approved-$RUN_ID" -e APPROVE

# With approved=true, guard should pass
run_test "CREATE payment-approved" create-instance -m payment -V 1 -i "payment-approved-$RUN_ID" -c '{"approved":true}'
run_test "APPROVE payment-approved (guard passes)" apply-event -i "payment-approved-$RUN_ID" -e APPROVE

# Test REJECT (no guard)
run_test "CREATE payment-to-reject" create-instance -m payment -V 1 -i "payment-to-reject-$RUN_ID" -c '{}'
run_test "REJECT payment (no guard)" apply-event -i "payment-to-reject-$RUN_ID" -e REJECT

# ============================================================================
echo -e "\n${YELLOW}=== Delete Instance ===${NC}"
# ============================================================================

run_test "CREATE instance-to-delete" create-instance -m order -V 1 -i "instance-to-delete-$RUN_ID"
run_test "DELETE_INSTANCE" delete-instance "instance-to-delete-$RUN_ID"
run_test "DELETE_INSTANCE (idempotent)" delete-instance "instance-to-delete-$RUN_ID"

# ============================================================================
echo -e "\n${YELLOW}=== WAL Read ===${NC}"
# ============================================================================

run_test "WAL_READ from 0" wal-read -f 0 -l 5
run_test "WAL_READ with limit" wal-read -f 0 -l 2

# ============================================================================
echo -e "\n${YELLOW}=== Compaction ===${NC}"
# ============================================================================

run_test "COMPACT (snapshot all and compact)" compact --force

# ============================================================================
echo -e "\n${YELLOW}=== Summary ===${NC}"
# ============================================================================

echo ""
echo -e "${BLUE}========================================${NC}"
TOTAL=$((PASSED + FAILED))
echo -e "Passed: ${GREEN}$PASSED${NC} / $TOTAL"
echo -e "Failed: ${RED}$FAILED${NC} / $TOTAL"
echo -e "${BLUE}========================================${NC}"

if [ $FAILED -eq 0 ]; then
    echo -e "\n${GREEN}All tests passed!${NC}"
    exit 0
else
    echo -e "\n${RED}Some tests failed.${NC}"
    exit 1
fi
