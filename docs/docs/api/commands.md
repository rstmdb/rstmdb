---
sidebar_position: 1
---

# API Commands

Complete reference for all rstmdb API commands.

## Command Categories

| Category | Commands |
|----------|----------|
| [Session](./session) | HELLO, AUTH, PING, BYE, INFO |
| [Machines](./machines) | PUT_MACHINE, GET_MACHINE, LIST_MACHINES |
| [Instances](./instances) | CREATE_INSTANCE, GET_INSTANCE, LIST_INSTANCES, DELETE_INSTANCE |
| [Events](./events) | APPLY_EVENT, BATCH |
| [Subscriptions](./subscriptions) | WATCH_INSTANCE, WATCH_ALL, UNWATCH |
| [Storage](./storage) | SNAPSHOT_INSTANCE, WAL_READ, WAL_STATS, COMPACT |

## Quick Reference

### Session Management

```json
// HELLO - Handshake (must be first message)
{"op": "HELLO", "params": {"protocol_version": 1, "wire_modes": ["binary_json"]}}

// AUTH - Authenticate
{"op": "AUTH", "params": {"method": "bearer", "token": "secret"}}

// PING - Health check
{"op": "PING"}

// INFO - Server info
{"op": "INFO"}

// BYE - Disconnect
{"op": "BYE"}
```

### Machine Operations

```json
// PUT_MACHINE - Register machine
{"op": "PUT_MACHINE", "params": {
  "machine": "order",
  "version": 1,
  "definition": {"states": [...], "initial": "...", "transitions": [...]}
}}

// GET_MACHINE - Get machine
{"op": "GET_MACHINE", "params": {"machine": "order", "version": 1}}

// LIST_MACHINES - List all machines
{"op": "LIST_MACHINES"}
```

### Instance Operations

```json
// CREATE_INSTANCE
{"op": "CREATE_INSTANCE", "params": {
  "machine": "order",
  "version": 1,
  "instance_id": "order-001",
  "initial_ctx": {"customer": "alice"}
}}

// GET_INSTANCE
{"op": "GET_INSTANCE", "params": {"instance_id": "order-001"}}

// LIST_INSTANCES
{"op": "LIST_INSTANCES", "params": {
  "machine": "order",
  "state": "pending",
  "limit": 100
}}

// DELETE_INSTANCE
{"op": "DELETE_INSTANCE", "params": {"instance_id": "order-001"}}
```

### Event Operations

```json
// APPLY_EVENT
{"op": "APPLY_EVENT", "params": {
  "instance_id": "order-001",
  "event": "PAY",
  "payload": {"amount": 99.99}
}}

// BATCH
{"op": "BATCH", "params": {
  "mode": "best_effort",
  "ops": [
    {"op": "APPLY_EVENT", "params": {...}},
    {"op": "APPLY_EVENT", "params": {...}}
  ]
}}
```

### Subscription Operations

```json
// WATCH_INSTANCE
{"op": "WATCH_INSTANCE", "params": {"instance_id": "order-001", "include_ctx": true}}

// WATCH_ALL
{"op": "WATCH_ALL", "params": {
  "machines": ["order"],
  "to_states": ["shipped"],
  "include_ctx": true
}}

// UNWATCH
{"op": "UNWATCH", "params": {"subscription_id": "sub-123"}}
```

### Storage Operations

```json
// WAL_READ
{"op": "WAL_READ", "params": {"from_offset": 0, "limit": 100}}

// WAL_STATS
{"op": "WAL_STATS"}

// SNAPSHOT_INSTANCE
{"op": "SNAPSHOT_INSTANCE", "params": {"instance_id": "order-001"}}

// COMPACT
{"op": "COMPACT", "params": {"force_snapshot": false}}
```

## Common Parameters

### Pagination

List operations support pagination:

| Parameter | Type | Description |
|-----------|------|-------------|
| `limit` | integer | Maximum items to return (default: 100) |
| `offset` | integer | Number of items to skip |

### Idempotency

Write operations support idempotency keys:

| Parameter | Type | Description |
|-----------|------|-------------|
| `idempotency_key` | string | Unique key for deduplication |

Idempotency keys:
- Are scoped per operation type
- Expire after 24 hours
- Return cached result on duplicate

## Response Metadata

Responses may include optional metadata:

```json
{
  "meta": {
    "server_time": "2024-01-15T10:30:00Z",
    "leader": true,
    "wal_offset": 12345,
    "trace_id": "abc-123"
  }
}
```

| Field | Description |
|-------|-------------|
| `server_time` | Server timestamp (ISO 8601) |
| `leader` | Whether this node is the leader |
| `wal_offset` | WAL position after operation |
| `trace_id` | Request trace identifier |

Meta is omitted if empty.
