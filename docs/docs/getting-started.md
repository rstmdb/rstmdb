---
sidebar_position: 2
---

# Getting Started

This guide will help you install, configure, and run rstmdb.

## Prerequisites

- **Rust 1.75+** (for building from source)
- **Docker** (optional, for containerized deployment)

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/rstmdb/rstmdb.git
cd rstmdb

# Build in release mode
cargo build --release

# The binaries will be in ./target/release/
# - rstmdb (server)
# - rstmdb-cli (command-line client)
```

### Using Docker

```bash
# Pull the image
docker pull rstmdb/rstmdb:latest

# Run with default settings
docker run -p 7401:7401 -v rstmdb-data:/data rstmdb/rstmdb

# Or with custom configuration
docker run -p 7401:7401 \
  -v rstmdb-data:/data \
  -e RSTMDB_AUTH_REQUIRED=true \
  -e RSTMDB_AUTH_TOKEN_HASH=<sha256-hash> \
  rstmdb/rstmdb
```

## Running the Server

### Basic Usage

```bash
# Start with default settings (binds to 127.0.0.1:7401)
./target/release/rstmdb

# Or with environment variables
RSTMDB_BIND=0.0.0.0:7401 ./target/release/rstmdb
```

### With Configuration File

Create a `config.yaml`:

```yaml
network:
  bind_addr: "0.0.0.0:7401"
  idle_timeout_secs: 300

storage:
  data_dir: "./data"
  wal_segment_size_mb: 64
  fsync_policy: every_write

auth:
  required: false
```

Run with the config:

```bash
RSTMDB_CONFIG=config.yaml ./target/release/rstmdb
```

## Verifying the Installation

Use the CLI to check the connection:

```bash
# Check server health
./target/release/rstmdb-cli -s 127.0.0.1:7401 ping
# Output: PONG

# Get server info
./target/release/rstmdb-cli -s 127.0.0.1:7401 info
```

## Your First State Machine

### 1. Define a State Machine

```bash
rstmdb-cli -s 127.0.0.1:7401 put-machine -n order -v 1 '{
  "states": ["created", "paid", "shipped", "delivered", "cancelled"],
  "initial": "created",
  "transitions": [
    {"from": "created", "event": "PAY", "to": "paid"},
    {"from": "created", "event": "CANCEL", "to": "cancelled"},
    {"from": "paid", "event": "SHIP", "to": "shipped"},
    {"from": "paid", "event": "REFUND", "to": "cancelled"},
    {"from": "shipped", "event": "DELIVER", "to": "delivered"}
  ]
}'
```

### 2. Create an Instance

```bash
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"customer": "alice", "total": 99.99}'
```

### 3. Apply Events

```bash
# Pay for the order
rstmdb-cli apply-event -i order-001 -e PAY -p '{"method": "card", "transaction_id": "txn-123"}'

# Ship the order
rstmdb-cli apply-event -i order-001 -e SHIP -p '{"carrier": "fedex", "tracking": "FX123456"}'
```

### 4. Check Instance State

```bash
rstmdb-cli get-instance order-001
```

Output:
```json
{
  "id": "order-001",
  "machine": "order",
  "version": 1,
  "state": "shipped",
  "context": {
    "customer": "alice",
    "total": 99.99,
    "method": "card",
    "transaction_id": "txn-123",
    "carrier": "fedex",
    "tracking": "FX123456"
  }
}
```

### 5. Watch for Events

In a separate terminal:

```bash
# Watch all events on the order machine
rstmdb-cli watch-all --machines order
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `RSTMDB_CONFIG` | Path to config file | None |
| `RSTMDB_BIND` | Server bind address | `127.0.0.1:7401` |
| `RSTMDB_DATA` | Data directory | `./data` |
| `RSTMDB_AUTH_REQUIRED` | Require authentication | `false` |
| `RSTMDB_AUTH_TOKEN_HASH` | SHA-256 token hash | None |
| `RSTMDB_TOKEN` | Auth token (CLI) | None |

## CLI Quick Reference

```bash
# Connection options
rstmdb-cli -s <server> -t <token> <command>

# Common commands
rstmdb-cli ping                              # Health check
rstmdb-cli info                              # Server info
rstmdb-cli put-machine -n <name> -v <ver> '<json>'  # Create machine
rstmdb-cli create-instance -m <machine> -V <ver> -i <id> -c '<ctx>'
rstmdb-cli apply-event -i <id> -e <event> -p '<payload>'
rstmdb-cli get-instance <id>
rstmdb-cli watch-all --machines <name>       # Stream events
```

## Next Steps

- [Architecture](./architecture) - Learn how rstmdb works internally
- [State Machines](./concepts/state-machines) - Understand state machine definitions
- [Configuration](./configuration) - Full configuration reference
- [CLI Reference](./cli) - Complete CLI documentation
