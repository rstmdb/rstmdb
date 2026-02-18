---
sidebar_position: 4
---

# Instance Commands

Commands for managing state machine instances.

## CREATE_INSTANCE

Creates a new instance of a state machine.

### Request

```json
{
  "op": "CREATE_INSTANCE",
  "params": {
    "instance_id": "order-001",
    "machine": "order",
    "version": 1,
    "initial_ctx": {
      "customer": "alice",
      "total": 99.99
    },
    "idempotency_key": "create-order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | No | Unique instance ID (auto-generated UUID if omitted) |
| `machine` | string | Yes | Machine name |
| `version` | integer | Yes | Machine version |
| `initial_ctx` | object | No | Initial context (default: `{}`) |
| `idempotency_key` | string | No | Deduplication key |

### Response

```json
{
  "status": "ok",
  "result": {
    "instance_id": "order-001",
    "state": "pending",
    "wal_offset": 1
  }
}
```

| Field | Description |
|-------|-------------|
| `instance_id` | Instance ID (provided or auto-generated) |
| `state` | Initial state from machine definition |
| `wal_offset` | WAL offset of the creation event |

### Errors

| Code | Description |
|------|-------------|
| `MACHINE_NOT_FOUND` | Machine or version doesn't exist |
| `INSTANCE_EXISTS` | Instance ID already in use |

---

## GET_INSTANCE

Retrieves an instance's current state and context.

### Request

```json
{
  "op": "GET_INSTANCE",
  "params": {
    "instance_id": "order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Instance ID |

### Response

```json
{
  "status": "ok",
  "result": {
    "machine": "order",
    "version": 1,
    "state": "paid",
    "ctx": {
      "customer": "alice",
      "total": 99.99,
      "payment_id": "pay-123"
    },
    "last_event_id": "evt-xyz",
    "last_wal_offset": 5
  }
}
```

| Field | Description |
|-------|-------------|
| `machine` | Machine name |
| `version` | Machine version |
| `state` | Current state |
| `ctx` | Current context |
| `last_event_id` | ID of last applied event (if set) |
| `last_wal_offset` | WAL offset of last update |

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |

---

## LIST_INSTANCES

Lists instances with optional filtering.

### Request

```json
{
  "op": "LIST_INSTANCES",
  "params": {
    "machine": "order",
    "state": "pending",
    "limit": 100,
    "offset": 0
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine` | string | No | Filter by machine name |
| `state` | string | No | Filter by current state |
| `limit` | integer | No | Max items (default: 100) |
| `offset` | integer | No | Skip items (default: 0) |

### Response

```json
{
  "status": "ok",
  "result": {
    "instances": [
      {
        "id": "order-001",
        "machine": "order",
        "version": 1,
        "state": "pending",
        "created_at": 1705312200,
        "updated_at": 1705312201,
        "last_wal_offset": 5
      },
      {
        "id": "order-002",
        "machine": "order",
        "version": 1,
        "state": "pending",
        "created_at": 1705312500,
        "updated_at": 1705312500,
        "last_wal_offset": 8
      }
    ],
    "total": 50,
    "has_more": true
  }
}
```

**Note:** List results include summary info only. Use GET_INSTANCE for full context.

---

## DELETE_INSTANCE

Soft-deletes an instance.

### Request

```json
{
  "op": "DELETE_INSTANCE",
  "params": {
    "instance_id": "order-001",
    "idempotency_key": "optional-key"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Instance ID |
| `idempotency_key` | string | No | Deduplication key |

### Response

```json
{
  "status": "ok",
  "result": {
    "instance_id": "order-001",
    "deleted": true,
    "wal_offset": 10
  }
}
```

### Behavior

- Instance is marked as deleted
- Excluded from LIST_INSTANCES by default
- GET_INSTANCE returns INSTANCE_NOT_FOUND
- Events cannot be applied to deleted instances
- Physically removed during compaction

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |

---

## Examples

### Create with Rich Context

```json
{
  "op": "CREATE_INSTANCE",
  "params": {
    "machine": "order",
    "version": 1,
    "instance_id": "order-2024-001",
    "initial_ctx": {
      "customer": {
        "id": "cust-123",
        "name": "Alice Smith",
        "tier": "gold"
      },
      "items": [
        {"sku": "ITEM-001", "qty": 2, "price": 29.99},
        {"sku": "ITEM-002", "qty": 1, "price": 49.99}
      ],
      "shipping": {
        "method": "express",
        "address": "123 Main St"
      },
      "total": 109.97
    }
  }
}
```

### List with Filters

```json
// All pending orders
{
  "op": "LIST_INSTANCES",
  "params": {
    "machine": "order",
    "state": "pending"
  }
}

// First 10 instances
{
  "op": "LIST_INSTANCES",
  "params": {
    "limit": 10
  }
}

// Page 2 of results
{
  "op": "LIST_INSTANCES",
  "params": {
    "limit": 100,
    "offset": 100
  }
}
```

### Idempotent Creation

```json
// First request - creates instance
{
  "op": "CREATE_INSTANCE",
  "params": {
    "machine": "order",
    "version": 1,
    "instance_id": "order-001",
    "initial_ctx": {},
    "idempotency_key": "create-order-001-abc123"
  }
}
// Response: {"result": {"instance_id": "order-001", "state": "pending", "wal_offset": 1}}

// Retry with same key - returns same result
{
  "op": "CREATE_INSTANCE",
  "params": {
    "machine": "order",
    "version": 1,
    "instance_id": "order-001",
    "initial_ctx": {},
    "idempotency_key": "create-order-001-abc123"
  }
}
// Response: cached result from first call
```
