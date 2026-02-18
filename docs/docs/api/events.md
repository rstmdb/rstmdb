---
sidebar_position: 5
---

# Event Commands

Commands for applying events and batch operations.

## APPLY_EVENT

Applies an event to an instance, triggering a state transition.

### Request

```json
{
  "op": "APPLY_EVENT",
  "params": {
    "instance_id": "order-001",
    "event": "PAY",
    "payload": {
      "payment_id": "pay-123",
      "amount": 99.99,
      "method": "card"
    },
    "expected_state": "pending",
    "expected_wal_offset": 1,
    "event_id": "evt-unique-id",
    "idempotency_key": "pay-order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Target instance |
| `event` | string | Yes | Event name |
| `payload` | object | No | Data to merge into context (default: `{}`) |
| `expected_state` | string | No | Optimistic concurrency: require this state |
| `expected_wal_offset` | integer | No | Optimistic concurrency: require this WAL offset |
| `event_id` | string | No | User-supplied event identifier |
| `idempotency_key` | string | No | Deduplication key |

### Response

```json
{
  "status": "ok",
  "result": {
    "from_state": "pending",
    "to_state": "paid",
    "ctx": {
      "customer": "alice",
      "total": 99.99,
      "payment_id": "pay-123",
      "amount": 99.99,
      "method": "card"
    },
    "wal_offset": 5,
    "applied": true,
    "event_id": "evt-unique-id"
  }
}
```

| Field | Description |
|-------|-------------|
| `from_state` | State before transition |
| `to_state` | State after transition |
| `ctx` | Updated context after payload merge |
| `wal_offset` | WAL offset of this event |
| `applied` | `true` if newly applied, `false` if idempotency key replay |
| `event_id` | Echo of user-supplied event ID (if provided) |

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |
| `INVALID_TRANSITION` | No valid transition for this event from current state |
| `GUARD_FAILED` | Guard condition not satisfied |
| `CONFLICT` | `expected_state` or `expected_wal_offset` doesn't match |

### Payload Merging

The payload is shallow-merged into the instance context:

```javascript
// Before: ctx = {a: 1, b: 2}
// Payload: {b: 3, c: 4}
// After:  ctx = {a: 1, b: 3, c: 4}
```

### Optimistic Concurrency

Use `expected_state` or `expected_wal_offset` to detect concurrent modifications:

```json
{
  "op": "APPLY_EVENT",
  "params": {
    "instance_id": "order-001",
    "event": "SHIP",
    "expected_state": "paid"
  }
}
```

If the instance is not in the expected state, returns `CONFLICT`.

---

## BATCH

Executes multiple operations in a single request.

### Request

```json
{
  "op": "BATCH",
  "params": {
    "mode": "best_effort",
    "ops": [
      {
        "op": "APPLY_EVENT",
        "params": {
          "instance_id": "order-001",
          "event": "PAY",
          "payload": {}
        }
      },
      {
        "op": "APPLY_EVENT",
        "params": {
          "instance_id": "order-002",
          "event": "PAY",
          "payload": {}
        }
      },
      {
        "op": "CREATE_INSTANCE",
        "params": {
          "machine": "notification",
          "version": 1,
          "instance_id": "notif-001",
          "initial_ctx": {"type": "payment"}
        }
      }
    ]
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mode` | string | Yes | `"atomic"` or `"best_effort"` |
| `ops` | Operation[] | Yes | List of operations (max 100) |

### Batch Modes

#### Atomic Mode

All operations succeed or the batch fails:

```json
{"mode": "atomic", ...}
```

- Operations are applied in order
- If any operation fails, the batch stops and returns an error
- **Note:** This is NOT transactional — already-applied writes within the batch are not rolled back

#### Best Effort Mode

Apply as many as possible:

```json
{"mode": "best_effort", ...}
```

- Operations are applied in order
- Failures don't stop subsequent operations
- Response includes per-operation results

### Supported Operations

Operations that can be batched:

- `CREATE_INSTANCE`
- `APPLY_EVENT`
- `DELETE_INSTANCE`

Read operations (GET_INSTANCE, LIST_INSTANCES) cannot be batched.

### Response (Best Effort)

```json
{
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {"from_state": "pending", "to_state": "paid", "ctx": {}, "wal_offset": 10, "applied": true}, "error": null},
      {"status": "error", "result": null, "error": {"code": "INSTANCE_NOT_FOUND", "message": "...", "retryable": false}},
      {"status": "ok", "result": {"instance_id": "notif-001", "state": "pending", "wal_offset": 11}, "error": null}
    ]
  }
}
```

### Response (Atomic — All Succeed)

```json
{
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {...}, "error": null},
      {"status": "ok", "result": {...}, "error": null}
    ]
  }
}
```

---

## Examples

### Multiple State Transitions

```json
{
  "op": "BATCH",
  "params": {
    "mode": "atomic",
    "ops": [
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-001", "event": "PAY"}},
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-001", "event": "SHIP"}},
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-001", "event": "DELIVER"}}
    ]
  }
}
```

### Process Multiple Orders

```json
{
  "op": "BATCH",
  "params": {
    "mode": "best_effort",
    "ops": [
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-001", "event": "SHIP"}},
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-002", "event": "SHIP"}},
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-003", "event": "SHIP"}}
    ]
  }
}
```

### Create with Initial Event

```json
{
  "op": "BATCH",
  "params": {
    "mode": "atomic",
    "ops": [
      {
        "op": "CREATE_INSTANCE",
        "params": {
          "machine": "order",
          "version": 1,
          "instance_id": "order-new",
          "initial_ctx": {"customer": "alice"}
        }
      },
      {
        "op": "APPLY_EVENT",
        "params": {
          "instance_id": "order-new",
          "event": "PAY",
          "payload": {"amount": 99.99}
        }
      }
    ]
  }
}
```
