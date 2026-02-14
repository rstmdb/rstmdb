---
sidebar_position: 8
---

# CLI Reference

Complete reference for the `rstmdb-cli` command-line interface.

## Installation

The CLI is included when building from source:

```bash
cargo build --release -p rstmdb-cli
# Binary at ./target/release/rstmdb-cli
```

## Usage

```bash
rstmdb-cli [OPTIONS] [COMMAND]
```

### Modes

**Single Command Mode:**
```bash
rstmdb-cli ping
rstmdb-cli get-instance order-001
```

**Interactive REPL:**
```bash
rstmdb-cli
> ping
PONG
> get-instance order-001
{...}
> exit
```

## Global Options

| Option | Env Variable | Default | Description |
|--------|--------------|---------|-------------|
| `-s, --server <ADDR>` | `RSTMDB_SERVER` | `127.0.0.1:7401` | Server address |
| `-t, --token <TOKEN>` | `RSTMDB_TOKEN` | None | Authentication token |
| `--tls` | `RSTMDB_TLS` | false | Enable TLS |
| `--ca-cert <PATH>` | `RSTMDB_CA_CERT` | None | CA certificate path |
| `--client-cert <PATH>` | `RSTMDB_CLIENT_CERT` | None | Client certificate (mTLS) |
| `--client-key <PATH>` | `RSTMDB_CLIENT_KEY` | None | Client key (mTLS) |
| `-k, --insecure` | - | false | Skip TLS verification |
| `--server-name <NAME>` | - | None | TLS SNI hostname |
| `--wire-mode <MODE>` | - | `binary` | Wire mode: binary, jsonl |
| `-v, --verbose` | - | false | Verbose output |
| `--json` | - | false | JSON output format |

## Commands

### Session Commands

#### ping

Health check.

```bash
rstmdb-cli ping
# Output: PONG
```

#### info

Server information.

```bash
rstmdb-cli info
```

```json
{
  "version": "0.1.0",
  "uptime_secs": 3600,
  "connections": 5,
  "instances": 1000
}
```

#### hash-token

Generate SHA-256 hash for a token.

```bash
rstmdb-cli hash-token my-secret-token
# Output: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

Use the hash in server configuration:
```yaml
auth:
  token_hashes:
    - "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
```

---

### Machine Commands

#### put-machine

Register a machine definition.

```bash
rstmdb-cli put-machine -n <NAME> -v <VERSION> '<DEFINITION_JSON>'
```

| Option | Required | Description |
|--------|----------|-------------|
| `-n, --name <NAME>` | Yes | Machine name |
| `-v, --version <VERSION>` | Yes | Version number |

**Example:**
```bash
rstmdb-cli put-machine -n order -v 1 '{
  "states": ["pending", "paid", "shipped"],
  "initial": "pending",
  "transitions": [
    {"from": "pending", "event": "PAY", "to": "paid"},
    {"from": "paid", "event": "SHIP", "to": "shipped"}
  ]
}'
```

#### get-machine

Get a machine definition.

```bash
rstmdb-cli get-machine <NAME> [VERSION]
```

**Examples:**
```bash
# Get latest version
rstmdb-cli get-machine order

# Get specific version
rstmdb-cli get-machine order 1
```

#### list-machines

List all machines.

```bash
rstmdb-cli list-machines [--limit N] [--offset N]
```

---

### Instance Commands

#### create-instance

Create a new instance.

```bash
rstmdb-cli create-instance -m <MACHINE> -V <VERSION> -i <ID> [-c '<CONTEXT>']
```

| Option | Required | Description |
|--------|----------|-------------|
| `-m, --machine <NAME>` | Yes | Machine name |
| `-V, --machine-version <VER>` | Yes | Machine version |
| `-i, --id <ID>` | Yes | Instance ID |
| `-c, --ctx <JSON>` | No | Initial context |
| `--idempotency-key <KEY>` | No | Deduplication key |

**Example:**
```bash
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"customer": "alice"}'
```

#### get-instance

Get an instance.

```bash
rstmdb-cli get-instance <ID>
```

**Example:**
```bash
rstmdb-cli get-instance order-001
```

```json
{
  "id": "order-001",
  "machine": "order",
  "version": 1,
  "state": "pending",
  "context": {"customer": "alice"}
}
```

#### list-instances

List instances.

```bash
rstmdb-cli list-instances [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--machine <NAME>` | Filter by machine |
| `--state <STATE>` | Filter by state |
| `--limit <N>` | Max results |
| `--offset <N>` | Skip results |

**Examples:**
```bash
# All instances
rstmdb-cli list-instances

# Filter by machine and state
rstmdb-cli list-instances --machine order --state pending

# Pagination
rstmdb-cli list-instances --limit 10 --offset 20
```

#### delete-instance

Delete an instance.

```bash
rstmdb-cli delete-instance <ID>
```

---

### Event Commands

#### apply-event

Apply an event to an instance.

```bash
rstmdb-cli apply-event -i <INSTANCE_ID> -e <EVENT> [-p '<PAYLOAD>']
```

| Option | Required | Description |
|--------|----------|-------------|
| `-i, --instance <ID>` | Yes | Instance ID |
| `-e, --event <EVENT>` | Yes | Event name |
| `-p, --payload <JSON>` | No | Event payload |
| `--idempotency-key <KEY>` | No | Deduplication key |

**Example:**
```bash
rstmdb-cli apply-event -i order-001 -e PAY -p '{"amount": 99.99}'
```

```json
{
  "previous_state": "pending",
  "current_state": "paid"
}
```

---

### Subscription Commands

#### watch-instance

Subscribe to a single instance.

```bash
rstmdb-cli watch-instance <ID>
```

Events stream to stdout:
```json
{"instance_id": "order-001", "event": "PAY", "to_state": "paid", ...}
{"instance_id": "order-001", "event": "SHIP", "to_state": "shipped", ...}
```

Press Ctrl+C to stop.

#### watch-all

Subscribe to all events with filters.

```bash
rstmdb-cli watch-all [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--machines <NAMES>` | Filter by machines (comma-separated) |
| `--events <EVENTS>` | Filter by events |
| `--from-states <STATES>` | Filter by source states |
| `--to-states <STATES>` | Filter by target states |
| `--from-offset <N>` | Start from WAL offset |

**Examples:**
```bash
# All events
rstmdb-cli watch-all

# Only order events
rstmdb-cli watch-all --machines order

# Only shipped/delivered
rstmdb-cli watch-all --to-states shipped,delivered

# Replay from beginning
rstmdb-cli watch-all --from-offset 0
```

#### unwatch

Cancel a subscription (REPL mode only).

```bash
> unwatch <SUBSCRIPTION_ID>
```

---

### Storage Commands

#### wal-read

Read WAL entries.

```bash
rstmdb-cli wal-read [-l <LIMIT>] [--from-offset <N>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `-l, --limit <N>` | 100 | Max entries |
| `--from-offset <N>` | 0 | Starting offset |

**Example:**
```bash
rstmdb-cli wal-read -l 10
```

#### wal-stats

Get WAL statistics.

```bash
rstmdb-cli wal-stats
```

```json
{
  "current_offset": 50000,
  "segment_count": 3,
  "total_size_bytes": 157286400
}
```

#### compact

Trigger WAL compaction.

```bash
rstmdb-cli compact
```

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RSTMDB_SERVER` | Server address |
| `RSTMDB_TOKEN` | Authentication token |
| `RSTMDB_TLS` | Enable TLS (true/false) |
| `RSTMDB_CA_CERT` | CA certificate path |
| `RSTMDB_CLIENT_CERT` | Client certificate path |
| `RSTMDB_CLIENT_KEY` | Client key path |

**Example:**
```bash
export RSTMDB_SERVER=127.0.0.1:7401
export RSTMDB_TOKEN=my-secret-token
rstmdb-cli ping
```

---

## Output Formats

### Default (Human-Readable)

```bash
rstmdb-cli get-instance order-001
```

### JSON

```bash
rstmdb-cli --json get-instance order-001
```

Useful for scripting:
```bash
state=$(rstmdb-cli --json get-instance order-001 | jq -r '.state')
echo "Current state: $state"
```

---

## Examples

### Complete Workflow

```bash
# Create machine
rstmdb-cli put-machine -n order -v 1 '{
  "states": ["created", "paid", "shipped", "delivered"],
  "initial": "created",
  "transitions": [
    {"from": "created", "event": "PAY", "to": "paid"},
    {"from": "paid", "event": "SHIP", "to": "shipped"},
    {"from": "shipped", "event": "DELIVER", "to": "delivered"}
  ]
}'

# Create instance
rstmdb-cli create-instance -m order -V 1 -i ORD-001 -c '{"customer": "alice"}'

# Apply events
rstmdb-cli apply-event -i ORD-001 -e PAY -p '{"amount": 99.99}'
rstmdb-cli apply-event -i ORD-001 -e SHIP -p '{"tracking": "1Z999"}'
rstmdb-cli apply-event -i ORD-001 -e DELIVER

# Check final state
rstmdb-cli get-instance ORD-001
```

### Scripting

```bash
#!/bin/bash
# Process pending orders

for id in $(rstmdb-cli --json list-instances --machine order --state pending | jq -r '.instances[].id'); do
  echo "Processing $id"
  rstmdb-cli apply-event -i "$id" -e PROCESS
done
```

### TLS Connection

```bash
rstmdb-cli \
  --tls \
  --ca-cert /etc/rstmdb/ca.pem \
  --client-cert /etc/rstmdb/client.pem \
  --client-key /etc/rstmdb/client-key.pem \
  -s secure.example.com:7401 \
  ping
```
