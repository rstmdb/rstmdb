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
| `op` | string | Yes | Operation name |
| `params` | object | No | Operation parameters |

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
    "wal_offset": 12345
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"response"` |
| `id` | string | Matches request id |
| `status` | string | `"ok"` for success |
| `result` | object | Operation result |
| `meta` | object | Response metadata |

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
| `error.code` | string | Machine-readable error code |
| `error.message` | string | Human-readable description |
| `error.retryable` | boolean | Whether retry may succeed |
| `error.details` | object | Additional error context |

## Event Messages

Sent asynchronously for active subscriptions:

```json
{
  "type": "event",
  "subscription_id": "sub-123",
  "event": {
    "instance_id": "order-001",
    "machine": "order",
    "version": 1,
    "event": "PAY",
    "from_state": "pending",
    "to_state": "paid",
    "payload": {"amount": 99.99},
    "timestamp": "2024-01-15T10:30:00Z",
    "wal_offset": 12345
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `type` | string | Always `"event"` |
| `subscription_id` | string | Subscription identifier |
| `event` | object | Event details |

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
    "client_version": "1.0.0"
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
    "server_version": "0.1.0",
    "auth_required": false,
    "features": ["subscriptions", "batch"]
  }
}
```

### AUTH

**Request:**
```json
{
  "type": "request",
  "id": "2",
  "op": "AUTH",
  "params": {
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

### PUT_MACHINE

**Request:**
```json
{
  "type": "request",
  "id": "3",
  "op": "PUT_MACHINE",
  "params": {
    "name": "order",
    "version": 1,
    "definition": {
      "states": ["pending", "paid", "shipped"],
      "initial": "pending",
      "transitions": [
        {"from": "pending", "event": "PAY", "to": "paid"},
        {"from": "paid", "event": "SHIP", "to": "shipped"}
      ]
    }
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
    "name": "order",
    "version": 1,
    "created": true
  }
}
```

### CREATE_INSTANCE

**Request:**
```json
{
  "type": "request",
  "id": "4",
  "op": "CREATE_INSTANCE",
  "params": {
    "machine": "order",
    "version": 1,
    "id": "order-001",
    "context": {"customer": "alice"},
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
    "id": "order-001",
    "machine": "order",
    "version": 1,
    "state": "pending",
    "context": {"customer": "alice"},
    "created": true
  }
}
```

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
    "previous_state": "pending",
    "current_state": "paid",
    "transition": {
      "from": "pending",
      "event": "PAY",
      "to": "paid"
    }
  },
  "meta": {
    "wal_offset": 12345
  }
}
```

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
    "subscription_id": "sub-abc123"
  }
}
```

**Subsequent events:**
```json
{
  "type": "event",
  "subscription_id": "sub-abc123",
  "event": {
    "instance_id": "order-001",
    "event": "SHIP",
    "from_state": "paid",
    "to_state": "shipped",
    "timestamp": "2024-01-15T10:30:00Z"
  }
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
    "mode": "atomic",
    "operations": [
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
      {"status": "ok", "result": {"current_state": "paid"}},
      {"status": "ok", "result": {"current_state": "paid"}}
    ]
  }
}
```

## Request ID Requirements

- Must be unique per connection
- Maximum length: 256 bytes
- Recommended format: sequential integers or UUIDs
- Reusing IDs may cause undefined behavior
