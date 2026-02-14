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
// HELLO - Handshake
{"op": "HELLO", "params": {"protocol_version": 1}}

// AUTH - Authenticate
{"op": "AUTH", "params": {"token": "secret"}}

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
  "name": "order",
  "version": 1,
  "definition": {"states": [...], "initial": "...", "transitions": [...]}
}}

// GET_MACHINE - Get machine
{"op": "GET_MACHINE", "params": {"name": "order", "version": 1}}

// LIST_MACHINES - List all machines
{"op": "LIST_MACHINES", "params": {"limit": 100, "offset": 0}}
```

### Instance Operations

```json
// CREATE_INSTANCE
{"op": "CREATE_INSTANCE", "params": {
  "machine": "order",
  "version": 1,
  "id": "order-001",
  "context": {"customer": "alice"}
}}

// GET_INSTANCE
{"op": "GET_INSTANCE", "params": {"id": "order-001"}}

// LIST_INSTANCES
{"op": "LIST_INSTANCES", "params": {
  "machine": "order",
  "state": "pending",
  "limit": 100
}}

// DELETE_INSTANCE
{"op": "DELETE_INSTANCE", "params": {"id": "order-001"}}
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
  "mode": "atomic",
  "operations": [
    {"op": "APPLY_EVENT", "params": {...}},
    {"op": "APPLY_EVENT", "params": {...}}
  ]
}}
```

### Subscription Operations

```json
// WATCH_INSTANCE
{"op": "WATCH_INSTANCE", "params": {"instance_id": "order-001"}}

// WATCH_ALL
{"op": "WATCH_ALL", "params": {
  "machines": ["order"],
  "to_states": ["shipped"]
}}

// UNWATCH
{"op": "UNWATCH", "params": {"subscription_id": "sub-123"}}
```

### Storage Operations

```json
// WAL_READ
{"op": "WAL_READ", "params": {"limit": 100, "from_offset": 0}}

// WAL_STATS
{"op": "WAL_STATS"}

// COMPACT
{"op": "COMPACT"}
```

## Common Parameters

### Pagination

Many list operations support pagination:

| Parameter | Type | Description |
|-----------|------|-------------|
| `limit` | integer | Maximum items to return (default: 100, max: 1000) |
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

All responses include metadata:

```json
{
  "meta": {
    "server_time": "2024-01-15T10:30:00Z",
    "wal_offset": 12345,
    "request_duration_ms": 5
  }
}
```

| Field | Description |
|-------|-------------|
| `server_time` | Server timestamp (ISO 8601) |
| `wal_offset` | WAL position after operation |
| `request_duration_ms` | Processing time in milliseconds |
