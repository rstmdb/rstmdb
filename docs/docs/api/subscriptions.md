---
sidebar_position: 6
---

# Subscription Commands

Commands for real-time event streaming.

## WATCH_INSTANCE

Subscribes to events on a specific instance.

### Request

```json
{
  "op": "WATCH_INSTANCE",
  "params": {
    "instance_id": "order-001",
    "include_ctx": true,
    "from_offset": 0
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Instance to watch |
| `include_ctx` | boolean | No | Include context in events (default: `true`) |
| `from_offset` | integer | No | Replay events from this WAL offset |

### Response

```json
{
  "status": "ok",
  "result": {
    "subscription_id": "sub-abc123",
    "instance_id": "order-001",
    "current_state": "pending",
    "current_wal_offset": 1
  }
}
```

### Events

After subscribing, events are pushed when the instance changes. Event fields are **top-level** (not nested inside an `event` object):

```json
{
  "type": "event",
  "subscription_id": "sub-abc123",
  "instance_id": "order-001",
  "machine": "order",
  "version": 1,
  "event": "PAY",
  "from_state": "pending",
  "to_state": "paid",
  "payload": {"amount": 99.99},
  "ctx": {"customer": "alice", "amount": 99.99},
  "wal_offset": 5
}
```

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |

---

## WATCH_ALL

Subscribes to events across all instances with optional filtering.

### Request

```json
{
  "op": "WATCH_ALL",
  "params": {
    "machines": ["order", "payment"],
    "events": ["PAY", "REFUND"],
    "from_states": ["pending"],
    "to_states": ["paid", "refunded"],
    "include_ctx": true,
    "from_offset": 0
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machines` | string[] | No | Filter by machine names |
| `events` | string[] | No | Filter by event names |
| `from_states` | string[] | No | Filter by source state |
| `to_states` | string[] | No | Filter by target state |
| `include_ctx` | boolean | No | Include context in events (default: `true`) |
| `from_offset` | integer | No | Start from WAL offset |

All filters are optional. If omitted, all events are delivered. Empty arrays also match all.

### Filter Behavior

Filters use AND logic between different filter types:

```json
{
  "machines": ["order"],      // AND
  "to_states": ["shipped"]    // = orders that transition TO shipped
}
```

Within a filter, values use OR logic:

```json
{
  "to_states": ["shipped", "delivered"]  // shipped OR delivered
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "subscription_id": "sub-xyz789",
    "wal_offset": 42
  }
}
```

| Field | Description |
|-------|-------------|
| `subscription_id` | Use this ID to identify events and to unsubscribe |
| `wal_offset` | Current WAL head; events after this offset will be streamed |

### Events

```json
{
  "type": "event",
  "subscription_id": "sub-xyz789",
  "instance_id": "order-001",
  "machine": "order",
  "version": 1,
  "event": "PAY",
  "from_state": "pending",
  "to_state": "paid",
  "payload": {"amount": 99.99},
  "ctx": {"customer": "alice", "amount": 99.99},
  "wal_offset": 43
}
```

### Replay from Offset

Use `from_offset` to replay events from a specific point:

```json
{
  "op": "WATCH_ALL",
  "params": {
    "from_offset": 10000
  }
}
```

This delivers:
1. All events from offset 10000 to current (historical)
2. New events as they occur (live)

Useful for:
- Catching up after disconnect
- Building read models
- Event synchronization

---

## UNWATCH

Cancels a subscription.

### Request

```json
{
  "op": "UNWATCH",
  "params": {
    "subscription_id": "sub-abc123"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `subscription_id` | string | Yes | Subscription to cancel |

### Response

```json
{
  "status": "ok",
  "result": {
    "subscription_id": "sub-abc123",
    "removed": true
  }
}
```

### Errors

| Code | Description |
|------|-------------|
| `NOT_FOUND` | Subscription doesn't exist |

---

## Event Message Format

All subscription events have this format (fields are top-level, not nested):

```json
{
  "type": "event",
  "subscription_id": "sub-abc123",
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

| Field | Description |
|-------|-------------|
| `subscription_id` | Subscription that matched this event |
| `instance_id` | Affected instance |
| `machine` | Machine name |
| `version` | Machine version |
| `event` | Event that was applied |
| `from_state` | Previous state |
| `to_state` | New state |
| `payload` | Event payload (null if absent) |
| `ctx` | Context after transition (only if `include_ctx: true`) |
| `wal_offset` | WAL position |

**Note:** If a subscriber can't keep up (channel full), events may be silently dropped for that subscriber with a server-side warning log.

---

## Examples

### Watch Shipped Orders

```json
{
  "op": "WATCH_ALL",
  "params": {
    "machines": ["order"],
    "to_states": ["shipped"]
  }
}
```

### Watch All Failures

```json
{
  "op": "WATCH_ALL",
  "params": {
    "events": ["FAIL", "ERROR", "REJECT"]
  }
}
```

### Replay and Follow

```json
// Get current WAL position
{"op": "WAL_STATS"}
// Response: {"result": {"latest_offset": 50000, ...}}

// Subscribe from beginning
{"op": "WATCH_ALL", "params": {"from_offset": 0}}

// Or from a saved checkpoint
{"op": "WATCH_ALL", "params": {"from_offset": 45000}}
```

### Multiple Subscriptions

A single connection can have multiple subscriptions:

```json
// Watch orders
{"op": "WATCH_ALL", "params": {"machines": ["order"]}}
// Response: subscription_id = "sub-1"

// Watch payments
{"op": "WATCH_ALL", "params": {"machines": ["payment"]}}
// Response: subscription_id = "sub-2"

// Events arrive with their subscription_id
{"type": "event", "subscription_id": "sub-1", "instance_id": "...", ...}
{"type": "event", "subscription_id": "sub-2", "instance_id": "...", ...}
```

---

## Best Practices

### Track Offsets for Reliability

Store the last processed offset for recovery:

```javascript
let lastOffset = loadCheckpoint();

subscribe({from_offset: lastOffset}, (event) => {
  processEvent(event);
  lastOffset = event.wal_offset;
  saveCheckpoint(lastOffset);
});
```

### Use Specific Filters

Narrow filters reduce network traffic and processing:

```json
// Good - specific
{"machines": ["order"], "to_states": ["shipped"]}

// Avoid - too broad
{}  // All events
```

### Handle Disconnects

Re-subscribe from last known offset:

```javascript
async function reliableSubscribe(filters, handler) {
  let offset = loadOffset();

  while (true) {
    try {
      await subscribe({...filters, from_offset: offset}, (event) => {
        handler(event);
        offset = event.wal_offset;
        saveOffset(offset);
      });
    } catch (error) {
      if (isDisconnect(error)) {
        await sleep(1000);
        continue;  // Reconnect
      }
      throw error;
    }
  }
}
```
