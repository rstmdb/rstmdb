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
    "machine": "order",
    "version": 1,
    "id": "order-001",
    "context": {
      "customer": "alice",
      "total": 99.99
    },
    "idempotency_key": "create-order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine` | string | Yes | Machine name |
| `version` | integer | Yes | Machine version |
| `id` | string | Yes | Unique instance ID |
| `context` | object | No | Initial context (default: {}) |
| `idempotency_key` | string | No | Deduplication key |

### Response

```json
{
  "status": "ok",
  "result": {
    "id": "order-001",
    "machine": "order",
    "version": 1,
    "state": "pending",
    "context": {
      "customer": "alice",
      "total": 99.99
    },
    "created": true,
    "created_at": "2024-01-15T10:30:00Z"
  }
}
```

| Field | Description |
|-------|-------------|
| `created` | True if newly created, false if idempotent duplicate |
| `state` | Initial state from machine definition |

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
    "id": "order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | string | Yes | Instance ID |

### Response

```json
{
  "status": "ok",
  "result": {
    "id": "order-001",
    "machine": "order",
    "version": 1,
    "state": "paid",
    "context": {
      "customer": "alice",
      "total": 99.99,
      "payment_id": "pay-123"
    },
    "created_at": "2024-01-15T10:30:00Z",
    "updated_at": "2024-01-15T11:00:00Z",
    "wal_offset": 12345
  }
}
```

| Field | Description |
|-------|-------------|
| `state` | Current state |
| `context` | Current context |
| `wal_offset` | WAL position of last update |

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
    "version": 1,
    "state": "pending",
    "limit": 100,
    "offset": 0
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine` | string | No | Filter by machine name |
| `version` | integer | No | Filter by machine version |
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
        "created_at": "2024-01-15T10:30:00Z",
        "updated_at": "2024-01-15T10:30:00Z"
      },
      {
        "id": "order-002",
        "machine": "order",
        "version": 1,
        "state": "pending",
        "created_at": "2024-01-15T10:35:00Z",
        "updated_at": "2024-01-15T10:35:00Z"
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
    "id": "order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | string | Yes | Instance ID |

### Response

```json
{
  "status": "ok",
  "result": {
    "deleted": true
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
    "id": "order-2024-001",
    "context": {
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
    "id": "order-001",
    "context": {},
    "idempotency_key": "create-order-001-abc123"
  }
}
// Response: {"result": {"created": true}}

// Retry with same key - returns same result
{
  "op": "CREATE_INSTANCE",
  "params": {
    "machine": "order",
    "version": 1,
    "id": "order-001",
    "context": {},
    "idempotency_key": "create-order-001-abc123"
  }
}
// Response: {"result": {"created": false}}  // Cached result
```
