# rstmdb Architecture

## Overview

rstmdb is a **state machine database** designed for durability, consistency, and real-time event streaming. Key characteristics:

- **WAL-based durability** - All mutations are persisted to a Write-Ahead Log before acknowledgment
- **In-memory state with persistent log** - Fast reads from memory, crash recovery from WAL
- **Event sourcing ready** - Full event history preserved, supports replay and CQRS patterns
- **Real-time subscriptions** - Watch individual instances or all events with filtering

## Crate Structure

```
rstmdb/
├── rstmdb-protocol   # Wire protocol (RCP) - framing, encoding, messages
├── rstmdb-wal        # Write-Ahead Log - segments, recovery, durability
├── rstmdb-core       # State machine engine - definitions, instances, guards
├── rstmdb-storage    # Snapshots and compaction
├── rstmdb-server     # TCP server, handlers, sessions, broadcasting
├── rstmdb-client     # Rust client library
├── rstmdb-cli        # Command-line interface with REPL
└── rstmdb-bench      # Performance benchmarks
```

| Crate             | Purpose                                                             |
| ----------------- | ------------------------------------------------------------------- |
| `rstmdb-protocol` | RCP wire protocol: binary framing, JSON messages, CRC32C validation |
| `rstmdb-wal`      | Segment-based WAL with configurable fsync, recovery support         |
| `rstmdb-core`     | State machine definitions, instance management, guard evaluation    |
| `rstmdb-storage`  | Snapshot persistence, compaction coordination                       |
| `rstmdb-server`   | Async TCP server, command handling, event broadcasting              |
| `rstmdb-client`   | Connection management, async API, TLS support                       |
| `rstmdb-cli`      | Interactive shell for administration and debugging                  |
| `rstmdb-bench`    | Criterion benchmarks for WAL, protocol, engine, e2e                 |

## Architecture Diagram

```
                                    ┌─────────────────────────────────────┐
                                    │           rstmdb Server             │
┌─────────────┐                     │                                     │
│   Client    │◄───TCP/TLS─────────►│  ┌─────────────────────────────┐   │
│  (rstmdb-   │                     │  │     Session Manager         │   │
│   client)   │                     │  │  - Auth validation          │   │
└─────────────┘                     │  │  - Wire mode (binary/jsonl) │   │
                                    │  │  - Subscription tracking    │   │
┌─────────────┐                     │  └──────────────┬──────────────┘   │
│    CLI      │◄───TCP/TLS─────────►│                 │                   │
│  (rstmdb-   │                     │  ┌──────────────▼──────────────┐   │
│    cli)     │                     │  │     Command Handler         │   │
└─────────────┘                     │  │  - Request dispatch         │   │
                                    │  │  - Response formatting      │   │
                                    │  └──────────────┬──────────────┘   │
                                    │                 │                   │
                                    │  ┌──────────────▼──────────────┐   │
                                    │  │   State Machine Engine      │   │
                                    │  │  - Machine definitions      │   │
                                    │  │  - Instance state           │   │
                                    │  │  - Guard evaluation         │   │
                                    │  │  - Idempotency cache        │   │
                                    │  └──────────────┬──────────────┘   │
                                    │                 │                   │
                                    │       ┌─────────┴─────────┐         │
                                    │       │                   │         │
                                    │  ┌────▼────┐       ┌──────▼─────┐  │
                                    │  │   WAL   │       │  Snapshot  │  │
                                    │  │         │       │   Store    │  │
                                    │  └────┬────┘       └──────┬─────┘  │
                                    │       │                   │         │
                                    └───────┼───────────────────┼─────────┘
                                            │                   │
                                    ┌───────▼───────┐   ┌───────▼───────┐
                                    │  WAL Segments │   │   Snapshots   │
                                    │  (data/wal/)  │   │ (data/snap/)  │
                                    └───────────────┘   └───────────────┘
```

## Key Components

### State Machine Engine

The engine (`rstmdb-core`) manages machine definitions and instance state:

**Machine Definitions** - JSON DSL describing states and transitions:

```json
{
  "states": ["created", "paid", "shipped", "delivered"],
  "initial": "created",
  "transitions": [
    { "from": "created", "event": "PAY", "to": "paid" },
    {
      "from": "paid",
      "event": "SHIP",
      "to": "shipped",
      "guard": "ctx.address_verified"
    },
    { "from": "shipped", "event": "DELIVER", "to": "delivered" }
  ]
}
```

**Guard Expressions** - Boolean expressions evaluated against instance context:

- Field access: `ctx.field`, `ctx.nested.field`
- Comparisons: `==`, `!=`, `>`, `>=`, `<`, `<=`
- Logical: `&&`, `||`, `!`
- Grouping: parentheses for precedence

**Instance Management**:

- Creation with optional initial context
- Event application with atomic state transitions
- Soft-delete support
- Idempotency via client-provided keys

### Write-Ahead Log

The WAL (`rstmdb-wal`) provides durability through segment-based storage:

**Segment Structure**:

- Default segment size: 64 MiB
- Automatic rotation when segment fills
- Global offset encoding: `segment_id << 40 | offset_in_segment`

**Record Format** (24-byte header):

```
Offset  Size  Field       Description
0       4     magic       "WLOG" (0x574C4F47)
4       1     type        Entry type
5       1     flags       Reserved
6       2     reserved    Must be 0
8       4     length      Payload length (big-endian)
12      4     crc32c      CRC32C of payload
16      8     sequence    Sequence number
24+     var   payload     JSON-serialized entry
```

**Entry Types**:

- `PutMachine` - Machine definition registration
- `CreateInstance` - Instance creation
- `ApplyEvent` - Event application with state transition
- `DeleteInstance` - Soft deletion
- `Snapshot` - Snapshot marker for compaction
- `Checkpoint` - Recovery checkpoint

**Fsync Policies**:

- `EveryWrite` - Maximum durability, fsync after each write
- `EveryN(n)` - Fsync every N writes
- `EveryMs(ms)` - Group commit, fsync every N milliseconds
- `Never` - No fsync (fastest, data loss risk on crash)

### Server

The server (`rstmdb-server`) handles networking and request processing:

**Session Management**:

- Connection state tracking (connected → ready → authenticated)
- Wire mode negotiation (binary_json, jsonl)
- Feature negotiation (idempotency, batch, watch, wal_read)
- Subscription tracking per session

**Event Broadcasting**:

- Per-instance channels for WATCH_INSTANCE
- Global channel for WATCH_ALL with filtering
- Filter by machine, states, event type

**Authentication**:

- Bearer token authentication
- SHA-256 token hashes stored in config
- Constant-time comparison for security

**TLS Support**:

- Server certificate and private key (PEM)
- Optional mTLS with client certificate verification

### Storage

The storage layer (`rstmdb-storage`) manages snapshots:

**Snapshots**:

- Point-in-time instance state capture
- Used for WAL compaction (allows segment deletion)
- Metadata tracking for recovery

**Compaction**:

- Automatic: triggered by event count or WAL size thresholds
- Manual: via COMPACT operation
- Process: snapshot instances → find min offset → delete old segments

## Data Flow

### Request Processing

```
1. TCP packet received
2. Frame decoded (binary header + JSON payload)
3. CRC32C validated (if present)
4. Session state checked (auth required?)
5. Command dispatched to handler
6. Engine processes operation
7. WAL entry written (for mutations)
8. Response formatted and sent
9. Event broadcasters notified (for state changes)
```

### State Change Flow (APPLY_EVENT)

```
1. Receive APPLY_EVENT request
2. Load instance from engine
3. Load machine definition
4. Find matching transition (from_state, event)
5. Evaluate guard expression (if present)
6. If guard passes:
   a. Update instance state and context
   b. Append ApplyEvent to WAL
   c. Wait for fsync (per policy)
   d. Notify broadcasters
   e. Return success with new state
7. If guard fails:
   a. Return GUARD_FAILED error
```

### Recovery Flow

```
1. Server starts
2. Scan WAL directory for segments
3. For each segment (in order):
   a. Read and validate records
   b. Replay PutMachine → register definitions
   c. Replay CreateInstance → create instances
   d. Replay ApplyEvent → update instance state
   e. Replay DeleteInstance → mark deleted
   f. Handle Snapshot → update snapshot metadata
4. Engine ready with reconstructed state
5. Accept client connections
```

## Design Decisions

### WAL-Based Durability

All mutations must be durably written to the WAL before acknowledgment. This ensures:

- No data loss on crashes (within fsync policy)
- Full audit trail of all changes
- Support for replication (future)

### In-Memory State + Persistent Log

- Reads are fast (no disk I/O for queries)
- Writes go to WAL first, then update memory
- Recovery replays log to rebuild state
- Trade-off: memory usage scales with instance count

### Idempotency Cache

- Clients provide idempotency keys for mutations
- Server caches results keyed by (instance_id, idempotency_key)
- Retries return cached response instead of re-applying
- Prevents duplicate events on network retries

### Guard Expressions

- Simple expression language (no Turing-complete scripting)
- Evaluated synchronously during event application
- Access to instance context only (not external state)
- Designed for safety and predictability

### Event Streaming

- Push-based notifications via WATCH operations
- Reduces polling overhead
- Enables real-time UI updates and integrations

### Segment-Based WAL

- Fixed-size segments simplify compaction
- Old segments can be deleted after snapshotting
- Offset encoding allows ~1M segments of ~1TB each

### Token-Based Authentication

- Simple bearer token model
- Tokens stored as SHA-256 hashes
- No complex session management needed

## Concurrency Model

**Data Structures**:

- `DashMap` for machine definitions (lock-free concurrent reads)
- `DashMap<InstanceId, RwLock<Instance>>` for instances
- Per-instance `RwLock` prevents concurrent modifications to same instance

**Atomicity**:

- Single instance operations are atomic (hold write lock)
- Batch operations with `mode: atomic` use transaction semantics
- WAL writes are sequential (single writer)

**Metrics**:

- Atomic counters for request/error counts
- Lock-free metric updates

## Configuration

Configuration is loaded from YAML with environment variable overrides:

```yaml
# Network settings
network:
  bind_addr: "127.0.0.1:7401"
  idle_timeout_secs: 300
  max_connections: 1000

# Storage settings
storage:
  data_dir: "./data"
  wal_segment_size_mb: 64
  fsync_policy: "every_write" # every_write, every_n, every_ms, never

# Authentication
auth:
  required: false
  token_hashes: []
  secrets_file: null

# TLS
tls:
  enabled: false
  cert_path: null
  key_path: null
  require_client_cert: false
  client_ca_path: null

# Metrics
metrics:
  enabled: false
  bind_addr: "0.0.0.0:9090"

# Compaction
compaction:
  enabled: true
  events_threshold: 10000
  size_threshold_mb: 100
  min_interval_secs: 60
```

**Environment Variables**:

- `RSTMDB_CONFIG` - Path to config file
- Individual settings can be overridden via `RSTMDB_<SECTION>_<KEY>` format

## See Also

- [RCP Protocol Overview](./protocol.md)
- [Project Roadmap](../ROADMAP.md)
