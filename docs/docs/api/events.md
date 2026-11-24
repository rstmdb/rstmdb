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
| `IDEMPOTENCY_CONFLICT` | Idempotency key reused for a different `event` or `payload` on `APPLY_EVENT` (per-attempt `event_id` is excluded; note JSON numbers such as `10` vs `10.0` produce distinct fingerprints), or a different `(machine, version, initial_ctx)` on `CREATE_INSTANCE` |

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

All operations are applied, or none are:

```json
{"mode": "atomic", ...}
```

- Every operation is parsed and validated first, in order, against the current
  state plus the effects of the operations before it (an op can use an instance
  created, or a context field written, by an earlier op of the same batch)
- If any operation would fail, the batch is rejected with `BAD_REQUEST` and
  nothing is applied or written to the WAL. The error names the failing op
  (see [Atomic batch failure](#atomic-batch-failure))
- Otherwise all operations are applied in order. No other write runs between
  validation and the last operation of the batch
- Only `PUT_MACHINE`, `CREATE_INSTANCE`, `APPLY_EVENT` and `DELETE_INSTANCE` are
  allowed; any other op rejects the batch
- `expected_wal_offset` cannot be used on an instance that an earlier op of the
  same batch changed (its new offset is only assigned when the batch commits);
  such a batch is rejected
- Limit: each operation is its own WAL record, and the WAL has no multi-record
  transaction. If a WAL write fails part way (an I/O error) the operations
  before it stay applied, and the error reports how many (`committed_ops`,
  not retryable). A crash while the batch is being written can likewise
  leave a prefix of it after recovery

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

- `PUT_MACHINE` (atomic mode)
- `CREATE_INSTANCE`
- `APPLY_EVENT`
- `DELETE_INSTANCE`

Read operations (GET_INSTANCE, LIST_INSTANCES) cannot be batched; atomic mode
rejects them.

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

### Response (Atomic, All Succeed)

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

### Atomic Batch Failure

If validation rejects op 1 of the batch, nothing is applied:

```json
{
  "status": "error",
  "error": {
    "code": "BAD_REQUEST",
    "message": "atomic batch rejected at op 1 (INSTANCE_NOT_FOUND): core error: instance not found: order-404; no operations were applied",
    "retryable": false,
    "details": {
      "failed_op_index": 1,
      "failed_op_code": "INSTANCE_NOT_FOUND",
      "committed_ops": 0
    }
  }
}
```

`failed_op_index` is the zero-based index in `ops`, and `failed_op_code` is the
error code the op would have returned on its own.

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
