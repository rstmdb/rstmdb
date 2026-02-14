---
sidebar_position: 3
---

# Architecture

rstmdb is designed as a single-node, in-memory state machine database with durable storage through Write-Ahead Logging (WAL).

## System Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              rstmdb Server                                  │
│                                                                             │
│  ┌─────────────┐     ┌─────────────────┐     ┌──────────────────────────┐   │
│  │   TCP/TLS   │────▶│ Session Manager │────▶│    Command Dispatcher    │   │
│  │  Listener   │◀────│   (per-conn)    │◀────│                          │   │
│  └─────────────┘     └─────────────────┘     └────────────┬─────────────┘   │
│                                                           │                 │
│        ┌──────────────────────────────────────────────────┼──────────┐      │
│        │                                                  ▼          │      │
│        │  ┌────────────────┐     ┌─────────────────────────────────┐ │      │
│        │  │    Machine     │     │         Instance Store          │ │      │
│        │  │   Registry     │     │  (DashMap - concurrent HashMap) │ │      │
│        │  └────────────────┘     └─────────────────────────────────┘ │      │
│        │                                       │                     │      │
│        │                         State Machine Engine                │      │
│        └─────────────────────────────┬───────────────────────────────┘      │
│                                      │                                      │
│  ┌───────────────────────────────────┼───────────────────────────────────┐  │
│  │                                   ▼                                   │  │
│  │  ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐  │  │
│  │  │    Snapshot     │     │   WAL Writer    │     │   Compaction    │  │  │
│  │  │    Manager      │────▶│   (segments)    │◀────│    Service      │  │  │
│  │  └─────────────────┘     └─────────────────┘     └─────────────────┘  │  │
│  │                                                                       │  │
│  │                          Storage Layer                                │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                              File System                                    │
│                                                                             │
│    data/                                                                    │
│    ├── wal/                                                                 │
│    │   ├── 0000000000000001.wal                                             │
│    │   ├── 0000000000000002.wal                                             │
│    │   └── ...                                                              │
│    └── snapshots/                                                           │
│        ├── index.json                                                       │
│        └── snap-*.snap                                                      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Core Components

### TCP Server

The server accepts TCP connections with optional TLS encryption. Each connection creates a session that handles:

- **Protocol Negotiation** - HELLO handshake establishes protocol version and capabilities
- **Authentication** - Optional bearer token validation
- **Request Dispatch** - Routes commands to appropriate handlers
- **Subscription Management** - Maintains active watch subscriptions

### Session Manager

Each client connection gets a dedicated session that:

- Maintains connection state and authentication status
- Buffers incoming requests and outgoing responses
- Manages subscriptions for that connection
- Handles graceful disconnection

### State Machine Engine

The core of rstmdb, responsible for:

#### Machine Registry

- Stores state machine definitions (states, transitions, guards)
- Supports multiple versions per machine name
- Validates definitions on registration

#### Instance Store

- In-memory storage using DashMap for concurrent access
- Stores current state, context, and metadata for each instance
- Supports atomic state transitions

#### Guard Evaluator

- Parses and evaluates guard expressions
- Supports comparisons, logical operators, and context field access
- Evaluates against instance context at transition time

### Storage Layer

#### Write-Ahead Log (WAL)

The WAL provides durability with these characteristics:

- **Segment-based storage** - 64 MiB segments by default
- **Sequential writes** - Append-only for performance
- **CRC32C checksums** - Data integrity verification
- **Configurable fsync** - Trade-off between durability and performance

WAL Entry Types:

- `PutMachine` - Machine definition registration
- `CreateInstance` - Instance creation
- `ApplyEvent` - State transition with payload
- `DeleteInstance` - Soft deletion marker
- `Snapshot` - Snapshot reference
- `Checkpoint` - Recovery checkpoint

#### Snapshot Manager

- Creates point-in-time snapshots of instance state
- Enables WAL compaction by providing recovery baseline
- Stores snapshots as compressed files

#### Compaction Service

- Removes WAL segments that are superseded by snapshots
- Can run automatically based on thresholds or manually triggered
- Reclaims disk space while maintaining recoverability

## Data Flow

### Write Path

```
Client Request
      │
      ▼
┌─────────────┐
│  Validate   │
│   Request   │
└──────┬──────┘
       │
       ▼
┌─────────────┐     ┌─────────────┐
│   Apply     │────▶│  Write to   │
│  to Memory  │     │    WAL      │
└──────┬──────┘     └──────┬──────┘
       │                   │
       ▼                   ▼
┌─────────────┐     ┌─────────────┐
│  Broadcast  │     │   fsync     │
│  to Watchers│     │  (if conf)  │
└──────┬──────┘     └─────────────┘
       │
       ▼
┌─────────────┐
│   Return    │
│  Response   │
└─────────────┘
```

### Read Path

```
Client Request
      │
      ▼
┌─────────────┐
│   Lookup    │
│  in Memory  │
└──────┬──────┘
       │
       ▼
┌─────────────┐
│   Return    │
│  Response   │
└─────────────┘
```

### Recovery Path

On startup:

```
┌─────────────┐
│ Load Latest │
│  Snapshot   │
└──────┬──────┘
       │
       ▼
┌─────────────┐
│ Replay WAL  │
│ from offset │
└──────┬──────┘
       │
       ▼
┌─────────────┐
│  Ready to   │
│   Serve     │
└─────────────┘
```

## Concurrency Model

rstmdb uses Tokio for async I/O with these concurrency patterns:

### Per-Instance Locking

- Each instance has its own lock
- Concurrent operations on different instances proceed in parallel
- Operations on the same instance are serialized

### Connection Handling

- One Tokio task per connection
- Non-blocking I/O for all network operations
- Graceful shutdown with connection draining

### WAL Writing

- Single writer for sequential consistency
- Writes are queued and batched when possible
- Configurable fsync policies

## Memory Model

### In-Memory State

All active data is kept in memory for fast access:

```
Machine Definitions: HashMap<(name, version), Definition>
Instance Store: DashMap<instance_id, Instance>
Subscriptions: HashMap<subscription_id, Subscription>
```

### Memory Efficiency

- Instances share machine definition references
- Context data is stored as serde_json::Value (compact)
- Old WAL segments are memory-mapped only during replay

## Protocol Architecture

The RCP (rstmdb Command Protocol) uses a simple request-response model:

```
┌────────────────┐                    ┌────────────────┐
│     Client     │                    │     Server     │
└───────┬────────┘                    └───────┬────────┘
        │                                     │
        │  ──────── HELLO ──────────▶         │
        │  ◀─────── HELLO (ack) ────          │
        │                                     │
        │  ──────── AUTH ───────────▶         │
        │  ◀─────── OK ─────────────          │
        │                                     │
        │  ──────── REQUEST ────────▶         │
        │  ◀─────── RESPONSE ───────          │
        │                                     │
        │  ──────── WATCH_ALL ──────▶         │
        │  ◀─────── OK ─────────────          │
        │  ◀─────── EVENT ──────────          │
        │  ◀─────── EVENT ──────────          │
        │                                     │
        │  ──────── BYE ────────────▶         │
        │  ◀─────── BYE ────────────          │
        │                                     │
```

## Durability Guarantees

### Fsync Policies

| Policy        | Durability              | Performance |
| ------------- | ----------------------- | ----------- |
| `every_write` | Highest - no data loss  | Slowest     |
| `every_n: N`  | N operations at risk    | Balanced    |
| `every_ms: N` | N ms of data at risk    | Balanced    |
| `never`       | Full data loss on crash | Fastest     |

### Recovery Guarantees

- All acknowledged writes are durable (with appropriate fsync policy)
- Crash recovery replays WAL from last snapshot
- CRC32C validation detects corrupted entries
- Partial writes at end of segment are detected and discarded

## Limitations

### Current Limitations

- **Single node only** - No replication or clustering yet
- **Memory-bound** - All instances must fit in RAM
- **No transactions** - Each operation is atomic, but no multi-operation transactions

### Planned Features

- WAL streaming replication for read replicas
- Raft consensus for automatic failover
- Sharding for horizontal scaling

## Crate Structure

```
rstmdb/
├── rstmdb-protocol   # Wire protocol, framing, message types
├── rstmdb-wal        # Write-ahead log implementation
├── rstmdb-core       # State machine engine, guards
├── rstmdb-storage    # Snapshots, compaction, persistence
├── rstmdb-server     # TCP server, handlers, sessions
├── rstmdb-client     # Rust client library
├── rstmdb-cli        # Command-line interface
└── rstmdb-bench      # Benchmarks
```
