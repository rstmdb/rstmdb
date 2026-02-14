---
sidebar_position: 3
---

# Events

Events trigger state transitions in rstmdb. When an event is applied to an instance, the state machine evaluates possible transitions and moves to the appropriate next state.

## Event Structure

An event application includes:

```json
{
  "instance_id": "order-001",
  "event": "PAY",
  "payload": {
    "payment_id": "pay-123",
    "amount": 99.99,
    "method": "card"
  }
}
```

### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `instance_id` | `string` | Yes | Target instance |
| `event` | `string` | Yes | Event name |
| `payload` | `object` | No | Data to merge into context |

## Applying Events

### Basic Event

```bash
rstmdb-cli apply-event -i order-001 -e PAY
```

### With Payload

The payload is merged into the instance context:

```bash
rstmdb-cli apply-event -i order-001 -e PAY -p '{
  "payment_id": "pay-123",
  "amount": 99.99
}'
```

### With Idempotency

```bash
rstmdb-cli apply-event -i order-001 -e PAY -p '{}' --idempotency-key "pay-order-001-attempt-1"
```

## Transition Resolution

When an event is applied:

1. **Find matching transitions** - Look for transitions where `from` matches current state and `event` matches the applied event
2. **Evaluate guards** - For each matching transition, evaluate the guard expression (if any)
3. **Select transition** - Use the first transition whose guard passes (or has no guard)
4. **Apply transition** - Update state, merge payload into context, record in WAL
5. **Broadcast** - Notify any active subscriptions

### Example

Given this machine:

```json
{
  "states": ["pending", "approved", "escalated", "rejected"],
  "initial": "pending",
  "transitions": [
    {"from": "pending", "event": "APPROVE", "to": "approved", "guard": "ctx.amount <= 1000"},
    {"from": "pending", "event": "APPROVE", "to": "escalated", "guard": "ctx.amount > 1000"},
    {"from": "pending", "event": "REJECT", "to": "rejected"}
  ]
}
```

And an instance with context `{"amount": 500}`:

```bash
# This will transition to "approved" (amount <= 1000)
rstmdb-cli apply-event -i request-001 -e APPROVE
```

With context `{"amount": 5000}`:

```bash
# This will transition to "escalated" (amount > 1000)
rstmdb-cli apply-event -i request-002 -e APPROVE
```

## Event Response

A successful event application returns:

```json
{
  "status": "ok",
  "result": {
    "previous_state": "pending",
    "current_state": "approved",
    "transition": {
      "from": "pending",
      "event": "APPROVE",
      "to": "approved"
    }
  },
  "meta": {
    "wal_offset": 12345,
    "server_time": "2024-01-15T10:30:00Z"
  }
}
```

## Error Cases

### Invalid Transition

When no matching transition exists:

```json
{
  "status": "error",
  "error": {
    "code": "INVALID_TRANSITION",
    "message": "No transition from 'approved' on event 'APPROVE'"
  }
}
```

### Guard Failed

When a guard expression evaluates to false:

```json
{
  "status": "error",
  "error": {
    "code": "GUARD_FAILED",
    "message": "Guard 'ctx.amount <= 1000' failed"
  }
}
```

### Instance Not Found

```json
{
  "status": "error",
  "error": {
    "code": "INSTANCE_NOT_FOUND",
    "message": "Instance 'order-999' not found"
  }
}
```

## Payload Merging

Payloads are shallow-merged into the instance context:

```bash
# Context before: {"a": 1, "b": 2}
rstmdb-cli apply-event -i inst-001 -e UPDATE -p '{"b": 3, "c": 4}'
# Context after: {"a": 1, "b": 3, "c": 4}
```

### Nested Objects

Nested objects are replaced, not deep-merged:

```bash
# Context before: {"user": {"name": "alice", "role": "admin"}}
rstmdb-cli apply-event -i inst-001 -e UPDATE -p '{"user": {"name": "bob"}}'
# Context after: {"user": {"name": "bob"}}  (role is lost!)
```

To preserve nested fields, include them in the payload:

```bash
rstmdb-cli apply-event -i inst-001 -e UPDATE -p '{"user": {"name": "bob", "role": "admin"}}'
```

## Batch Events

Apply multiple events atomically:

```bash
# Via API (not CLI)
{
  "op": "BATCH",
  "params": {
    "mode": "atomic",
    "operations": [
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-001", "event": "PAY", "payload": {}}},
      {"op": "APPLY_EVENT", "params": {"instance_id": "order-002", "event": "PAY", "payload": {}}}
    ]
  }
}
```

Modes:
- `atomic` - All operations succeed or all fail
- `best_effort` - Apply as many as possible, report failures

## Best Practices

### Use Descriptive Event Names

```bash
# Good - clear intent
apply-event -e ORDER_PLACED
apply-event -e PAYMENT_RECEIVED
apply-event -e SHIPMENT_DISPATCHED

# Bad - ambiguous
apply-event -e UPDATE
apply-event -e NEXT
apply-event -e DONE
```

### Include Audit Data in Payloads

```bash
rstmdb-cli apply-event -i order-001 -e APPROVE -p '{
  "approved_by": "alice",
  "approved_at": "2024-01-15T10:30:00Z",
  "reason": "Within budget"
}'
```

### Handle Event Failures Gracefully

```bash
# Check response status
result=$(rstmdb-cli apply-event -i order-001 -e PAY -p '{}' 2>&1)
if echo "$result" | grep -q "INVALID_TRANSITION"; then
  echo "Order is not in a payable state"
fi
```

### Use Idempotency for Retries

```bash
# Safe to retry - same result each time
rstmdb-cli apply-event -i order-001 -e PAY \
  --idempotency-key "pay-order-001-$(date +%Y%m%d)"
```
