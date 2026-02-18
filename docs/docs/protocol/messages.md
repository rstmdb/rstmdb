---
sidebar_position: 3
---

# Message Types

This document describes all message types in the RCP protocol.

## Request Messages

### Common Fields

All requests have these fields:

```json
{
  "type": "request",
  "id": "unique-id",
  "op": "OPERATION_NAME",
  "params": { /* operation-specific */ }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Always `"request"` |
| `id` | string | Yes | Unique request identifier |
| `op` | string | Yes | Operation name (`SCREAMING_SNAKE_CASE`) |
| `params` | object | No | Operation parameters (defaults to `{}`) |

### Operation List

| Operation | Description |
|-----------|-------------|
| `HELLO` | Protocol handshake |
| `AUTH` | Authenticate with token |
| `PING` | Health check |
| `BYE` | Graceful disconnect |
| `INFO` | Server information |
| `PUT_MACHINE` | Register machine definition |
| `GET_MACHINE` | Get machine definition |
| `LIST_MACHINES` | List all machines |
| `CREATE_INSTANCE` | Create instance |
| `GET_INSTANCE` | Get instance |
| `LIST_INSTANCES` | List instances |
| `DELETE_INSTANCE` | Delete instance |
| `APPLY_EVENT` | Apply event to instance |
| `BATCH` | Batch operations |
| `WATCH_INSTANCE` | Subscribe to instance |
| `WATCH_ALL` | Subscribe to all events |
| `UNWATCH` | Cancel subscription |
| `SNAPSHOT_INSTANCE` | Create instance snapshot |
| `WAL_READ` | Read WAL entries |
| `WAL_STATS` | Get WAL statistics |
| `COMPACT` | Trigger compaction |

## Response Messages

### Success Response

```json
{
  "type": "response",
  "id": "matching-request-id",
  "status": "ok",
  "result": { /* operation result */ },
  "meta": {
    "server_time": "2024-01-15T10:30:00Z",
    "leader": true,
    "wal_offset": 12345,
    "trace_id": "abc-123"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"response"` |
| `id` | string | Matches request id |
| `status` | string | `"ok"` for success |
| `result` | object | Operation result |
| `meta` | object | Response metadata (optional, omitted if empty) |

### Error Response

```json
{
  "type": "response",
  "id": "matching-request-id",
  "status": "error",
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable message",
    "retryable": false,
    "details": {}
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `error.code` | string | Machine-readable error code (`SCREAMING_SNAKE_CASE`) |
| `error.message` | string | Human-readable description |
| `error.retryable` | boolean | Whether retry may succeed |
| `error.details` | object | Additional error context |

## Event Messages

Sent asynchronously for active subscriptions. Event fields are **top-level** (not nested inside an `event` object):

```json
{
  "type": "event",
  "subscription_id": "sub-123",
  "instance_id": "order-001",
  "machine": "order",
  "version": 1,
  "event": "PAY",
  "from_state": "pending",
  "to_state": "paid",
  "payload": {"amount": 99.99},
  "ctx": {"customer": "alice", "amount": 99.99},
  "wal_offset": 12345
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"event"` |
| `subscription_id` | string | Subscription identifier |
| `instance_id` | string | Affected instance |
| `machine` | string | Machine name |
| `version` | integer | Machine version |
| `event` | string | Event name that triggered the transition |
| `from_state` | string | State before transition |
| `to_state` | string | State after transition |
| `payload` | object | Event payload (null if absent) |
| `ctx` | object | Instance context after transition (only if `include_ctx: true`) |
| `wal_offset` | integer | WAL offset of this event |

## Detailed Operation Messages

### HELLO

**Request:**
```json
{
  "type": "request",
  "id": "1",
  "op": "HELLO",
  "params": {
    "protocol_version": 1,
    "client_name": "my-app",
    "wire_modes": ["binary_json", "jsonl"],
    "features": ["idempotency", "batch", "wal_read"]
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "1",
  "status": "ok",
  "result": {
    "protocol_version": 1,
    "wire_mode": "binary_json",
    "server_name": "rstmdb",
    "server_version": "0.1.1",
    "features": ["idempotency", "batch", "wal_read"]
  }
}
```

- `wire_modes` is a priority-ordered list; the server picks the first supported mode.
- `features` are intersection-negotiated (server returns supported subset).

### AUTH

**Request:**
```json
{
  "type": "request",
  "id": "2",
  "op": "AUTH",
  "params": {
    "method": "bearer",
    "token": "secret-token"
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "2",
  "status": "ok",
  "result": {
    "authenticated": true
  }
}
```

- Only `"bearer"` method is currently supported.

### PUT_MACHINE

**Request:**
```json
{
  "type": "request",
  "id": "3",
  "op": "PUT_MACHINE",
  "params": {
    "machine": "order",
    "version": 1,
    "definition": {
      "states": ["pending", "paid", "shipped"],
      "initial": "pending",
      "transitions": [
        {"from": "pending", "event": "PAY", "to": "paid"},
        {"from": "paid", "event": "SHIP", "to": "shipped"}
      ]
    },
    "checksum": "optional-sha256-hex"
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "3",
  "status": "ok",
  "result": {
    "machine": "order",
    "version": 1,
    "stored_checksum": "a1b2c3...",
    "created": true
  }
}
```

- `created: false` when re-submitting an identical definition (idempotent).

### CREATE_INSTANCE

**Request:**
```json
{
  "type": "request",
  "id": "4",
  "op": "CREATE_INSTANCE",
  "params": {
    "instance_id": "order-001",
    "machine": "order",
    "version": 1,
    "initial_ctx": {"customer": "alice"},
    "idempotency_key": "create-order-001"
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "4",
  "status": "ok",
  "result": {
    "instance_id": "order-001",
    "state": "pending",
    "wal_offset": 1
  }
}
```

- `instance_id` is optional — a UUID v4 is auto-generated if omitted.
- `initial_ctx` is optional (defaults to `{}`).

### APPLY_EVENT

**Request:**
```json
{
  "type": "request",
  "id": "5",
  "op": "APPLY_EVENT",
  "params": {
    "instance_id": "order-001",
    "event": "PAY",
    "payload": {"amount": 99.99},
    "expected_state": "pending",
    "expected_wal_offset": 1,
    "event_id": "evt-unique-id",
    "idempotency_key": "pay-order-001"
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "5",
  "status": "ok",
  "result": {
    "from_state": "pending",
    "to_state": "paid",
    "ctx": {
      "customer": "alice",
      "amount": 99.99
    },
    "wal_offset": 5,
    "applied": true,
    "event_id": "evt-unique-id"
  }
}
```

- `applied: false` means the event was a duplicate idempotency key replay.
- `expected_state` and `expected_wal_offset` enable optimistic concurrency.

### WATCH_ALL

**Request:**
```json
{
  "type": "request",
  "id": "6",
  "op": "WATCH_ALL",
  "params": {
    "machines": ["order"],
    "to_states": ["shipped", "delivered"],
    "include_ctx": true,
    "from_offset": 0
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "6",
  "status": "ok",
  "result": {
    "subscription_id": "sub-abc123",
    "wal_offset": 42
  }
}
```

**Subsequent events (top-level fields, not nested):**
```json
{
  "type": "event",
  "subscription_id": "sub-abc123",
  "instance_id": "order-001",
  "machine": "order",
  "version": 1,
  "event": "SHIP",
  "from_state": "paid",
  "to_state": "shipped",
  "payload": {},
  "ctx": {"customer": "alice", "amount": 99.99},
  "wal_offset": 43
}
```

### BATCH

**Request:**
```json
{
  "type": "request",
  "id": "7",
  "op": "BATCH",
  "params": {
    "mode": "best_effort",
    "ops": [
      {
        "op": "APPLY_EVENT",
        "params": {"instance_id": "order-001", "event": "PAY"}
      },
      {
        "op": "APPLY_EVENT",
        "params": {"instance_id": "order-002", "event": "PAY"}
      }
    ]
  }
}
```

**Response:**
```json
{
  "type": "response",
  "id": "7",
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {"from_state": "pending", "to_state": "paid", "ctx": {}, "wal_offset": 10, "applied": true}},
      {"status": "ok", "result": {"from_state": "pending", "to_state": "paid", "ctx": {}, "wal_offset": 11, "applied": true}}
    ]
  }
}
```

- Batch operations array field is `ops` (not `operations`).
- Max ops per batch: 100 (server default).

## Request ID Requirements

- Must be unique per connection
- Maximum length: 256 bytes
- Recommended format: sequential integers or UUIDs
- Reusing IDs may cause undefined behavior
