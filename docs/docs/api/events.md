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
    "idempotency_key": "pay-order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Target instance |
| `event` | string | Yes | Event name |
| `payload` | object | No | Data to merge into context |
| `idempotency_key` | string | No | Deduplication key |
| `expected_state` | string | No | Require instance to be in this state |

### Response

```json
{
  "status": "ok",
  "result": {
    "previous_state": "pending",
    "current_state": "paid",
    "transition": {
      "from": "pending",
      "event": "PAY",
      "to": "paid"
    },
    "context": {
      "customer": "alice",
      "total": 99.99,
      "payment_id": "pay-123",
      "amount": 99.99,
      "method": "card"
    }
  },
  "meta": {
    "wal_offset": 12345
  }
}
```

| Field | Description |
|-------|-------------|
| `previous_state` | State before transition |
| `current_state` | State after transition |
| `transition` | The transition that was applied |
| `context` | Updated context |

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |
| `INVALID_TRANSITION` | No valid transition for this event |
| `GUARD_FAILED` | Guard condition not satisfied |
| `CONFLICT` | Expected state doesn't match |

### Payload Merging

The payload is shallow-merged into the instance context:

```javascript
// Before: context = {a: 1, b: 2}
// Payload: {b: 3, c: 4}
// After:  context = {a: 1, b: 3, c: 4}
```

### Expected State

Use `expected_state` for optimistic concurrency:

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
    "mode": "atomic",
    "operations": [
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
          "id": "notif-001",
          "context": {"type": "payment"}
        }
      }
    ]
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `mode` | string | Yes | `"atomic"` or `"best_effort"` |
| `operations` | Operation[] | Yes | List of operations |

### Batch Modes

#### Atomic Mode

All operations succeed or all fail:

```json
{"mode": "atomic", ...}
```

- Operations are applied in order
- If any operation fails, all are rolled back
- Response is all-or-nothing

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

### Response (Atomic)

```json
{
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {"current_state": "paid"}},
      {"status": "ok", "result": {"current_state": "paid"}},
      {"status": "ok", "result": {"created": true}}
    ]
  }
}
```

### Response (Best Effort with Failures)

```json
{
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {"current_state": "paid"}},
      {"status": "error", "error": {"code": "INSTANCE_NOT_FOUND", "message": "..."}},
      {"status": "ok", "result": {"created": true}}
    ],
    "success_count": 2,
    "failure_count": 1
  }
}
```

### Atomic Failure

If atomic batch fails:

```json
{
  "status": "error",
  "error": {
    "code": "BATCH_FAILED",
    "message": "Operation 2 failed: INSTANCE_NOT_FOUND",
    "details": {
      "failed_index": 1,
      "operation_error": {
        "code": "INSTANCE_NOT_FOUND",
        "message": "Instance 'order-999' not found"
      }
    }
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
    "operations": [
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
    "operations": [
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
    "operations": [
      {
        "op": "CREATE_INSTANCE",
        "params": {
          "machine": "order",
          "version": 1,
          "id": "order-new",
          "context": {"customer": "alice"}
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
