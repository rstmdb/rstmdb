# rstmdb

A state machine database with WAL durability and snapshot compaction.

## Features

- **State Machine Engine** - Define state machines with states, transitions, and guards
- **WAL Durability** - Write-ahead log ensures no data loss on crash
- **Snapshot Compaction** - Automatic snapshots and WAL cleanup
- **TCP Protocol (RCP)** - Binary JSON protocol for high performance
- **TLS Support** - Optional TLS encryption with mutual TLS (mTLS) support
- **Guard Expressions** - Conditional transitions based on context
- **Idempotency** - Safe retries with idempotency keys
- **Batch Operations** - Execute multiple operations atomically
- **Event Streaming** - Watch instance changes in real-time with WATCH_INSTANCE and WATCH_ALL

## Quick Start

```bash
# Build
cargo build --release

# Run server
./target/release/rstmdb

# Or with config file
RSTMDB_CONFIG=config.yaml ./target/release/rstmdb
```

## CLI Usage

```bash
# Ping server
rstmdb-cli ping

# Define a state machine
rstmdb-cli put-machine -n order -v 1 '{
  "states": ["created", "paid", "shipped"],
  "initial": "created",
  "transitions": [
    {"from": "created", "event": "PAY", "to": "paid"},
    {"from": "paid", "event": "SHIP", "to": "shipped", "guard": "ctx.ready"}
  ]
}'

# Create instance
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"ready": false}'

# Apply event
rstmdb-cli apply-event -i order-001 -e PAY -p '{"amount": 99.99}'

# Get instance
rstmdb-cli get-instance order-001

# Compact WAL
rstmdb-cli compact --force
```

## Event Streaming

rstmdb supports real-time event streaming for building event-driven architectures.

### Watch a Specific Instance

Subscribe to state changes on a single instance:

```bash
# Watch instance and receive events
rstmdb-cli watch-instance order-001

# Output when state changes:
# {"type":"event","subscription_id":"sub-xxx","instance_id":"order-001",
#  "machine":"order","version":1,"from_state":"created","to_state":"paid",
#  "event":"PAY","wal_offset":42,"payload":{...},"ctx":{...}}
```

### Watch All Events

Subscribe to all state changes across all instances:

```bash
# Watch all events
rstmdb-cli watch-all

# Filter by machine type
rstmdb-cli watch-all --machines order

# Filter by target state (e.g., only "shipped" transitions)
rstmdb-cli watch-all --to-states shipped

# Filter by event type
rstmdb-cli watch-all --events PAY,SHIP

# Combine filters
rstmdb-cli watch-all --machines order --to-states shipped

# Exclude context from events (smaller payloads)
rstmdb-cli watch-all --no-ctx
```

### Use Cases

**Notification Service:**
```bash
# Watch for orders that ship, send customer notifications
rstmdb-cli watch-all --machines order --to-states shipped
```

**Audit Log:**
```bash
# Consume all events and write to external log
rstmdb-cli watch-all | jq -c >> audit.jsonl
```

**CQRS Projection:**
```bash
# Build a read model from state machine events
# Use a consumer that tracks the last processed wal_offset
rstmdb-cli watch-all --machines order | ./projection-builder
```

### Unsubscribe

```bash
# Unsubscribe using subscription ID (when not using Ctrl+C)
rstmdb-cli unwatch sub-xxxxxxxx
```

## Configuration

### Config File (YAML)

```yaml
# config.yaml
network:
  bind_addr: "127.0.0.1:7401"
  max_connections: 1000

auth:
  required: false
  # token_hashes:
  #   - "<sha256-hash>"  # Generate with: rstmdb-cli hash-token <token>

storage:
  data_dir: "./data"
  wal_segment_size_mb: 64
  fsync_policy: every_write

compaction:
  enabled: true
  events_threshold: 10000
  size_threshold_mb: 100
```

### Authentication

When `auth.required: true`, clients must authenticate before executing commands.

```bash
# Generate a token hash for config
rstmdb-cli hash-token "my-secret-token"

# Use token with CLI
rstmdb-cli --token "my-secret-token" list-machines

# Or via environment variable
export RSTMDB_TOKEN="my-secret-token"
rstmdb-cli list-machines
```

Token hashes can be stored in config or loaded from an external secrets file:

```yaml
auth:
  required: true
  secrets_file: "/etc/rstmdb/tokens.secret"  # One hash per line
```

### TLS

Enable TLS encryption for secure communication:

```yaml
# config.yaml
tls:
  enabled: true
  cert_path: "/path/to/server-cert.pem"
  key_path: "/path/to/server-key.pem"
  # For mutual TLS (mTLS):
  require_client_cert: true
  client_ca_path: "/path/to/client-ca.pem"
```

**Generate development certificates:**

```bash
./scripts/generate-dev-certs.sh ./dev-certs
```

**CLI usage with TLS:**

```bash
# Standard TLS (verify server certificate)
rstmdb-cli --tls --ca-cert ./dev-certs/ca-cert.pem -t my-token ping

# Insecure mode (skip verification - dev only)
rstmdb-cli --tls --insecure -t my-token ping

# Mutual TLS (client certificate required)
rstmdb-cli --tls \
    --ca-cert ./dev-certs/ca-cert.pem \
    --client-cert ./dev-certs/client-cert.pem \
    --client-key ./dev-certs/client-key.pem \
    -t my-token ping
```

**Environment variables for TLS:**

```bash
export RSTMDB_TLS=true
export RSTMDB_CA_CERT=/path/to/ca-cert.pem
export RSTMDB_CLIENT_CERT=/path/to/client-cert.pem
export RSTMDB_CLIENT_KEY=/path/to/client-key.pem
```

### Environment Variables

| Variable                   | Default          | Description                    |
| -------------------------- | ---------------- | ------------------------------ |
| `RSTMDB_CONFIG`            | -                | Path to YAML config file       |
| `RSTMDB_BIND`              | `127.0.0.1:7401` | Server bind address            |
| `RSTMDB_DATA`              | `./data`         | Data directory                 |
| `RSTMDB_AUTH_REQUIRED`     | `false`          | Require authentication         |
| `RSTMDB_AUTH_TOKEN_HASH`   | -                | Single token hash              |
| `RSTMDB_AUTH_SECRETS_FILE` | -                | Path to secrets file           |
| `RSTMDB_COMPACT_EVENTS`    | `10000`          | Auto-compact after N events    |
| `RSTMDB_COMPACT_SIZE_MB`   | `100`            | Auto-compact when WAL > N MB   |
| `RSTMDB_TOKEN`             | -                | CLI: Auth token for commands   |

Environment variables override config file values.

## State Machine Definition

```json
{
  "states": ["pending", "approved", "rejected"],
  "initial": "pending",
  "transitions": [
    {
      "from": "pending",
      "event": "APPROVE",
      "to": "approved",
      "guard": "ctx.amount <= 1000"
    },
    {
      "from": "pending",
      "event": "REJECT",
      "to": "rejected"
    }
  ]
}
```

### Guard Expressions

Guards are boolean expressions evaluated against instance context:

```
ctx.amount <= 1000
ctx.approved && ctx.manager_id
ctx.items > 0
!ctx.cancelled
```

## Architecture

```
┌─────────────────────────────────────────────┐
│                  Client                      │
└─────────────────┬───────────────────────────┘
                  │ RCP Protocol (TCP)
┌─────────────────▼───────────────────────────┐
│               Server                         │
│  ┌─────────────────────────────────────┐    │
│  │         Command Handler              │    │
│  └─────────────────┬───────────────────┘    │
│                    │                         │
│  ┌─────────────────▼───────────────────┐    │
│  │       State Machine Engine           │    │
│  │  • Definitions  • Instances          │    │
│  │  • Guards       • Transitions        │    │
│  └─────────────────┬───────────────────┘    │
│                    │                         │
│  ┌────────────┬────┴────┬─────────────┐    │
│  │    WAL     │ Snapshot │  Compaction │    │
│  │            │  Store   │   Manager   │    │
│  └────────────┴──────────┴─────────────┘    │
└─────────────────────────────────────────────┘
```

## Data Directory Structure

```
data/
├── wal/
│   ├── 0000000000000001.wal
│   ├── 0000000000000002.wal
│   └── ...
└── snapshots/
    ├── index.json
    ├── snap-xxx.snap
    └── ...
```

## Development

```bash
# Run tests
cargo test --workspace

# Run with logging
RUST_LOG=debug cargo run

# Format code
cargo fmt

# Lint
cargo clippy
```

### Git Hooks

```bash
# Setup pre-commit hooks (fmt, clippy, tests)
./.githooks/setup.sh
```
