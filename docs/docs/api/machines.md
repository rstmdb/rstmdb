---
sidebar_position: 3
---

# Machine Commands

Commands for managing state machine definitions.

## PUT_MACHINE

Registers a new machine definition or version.

### Request

```json
{
  "op": "PUT_MACHINE",
  "params": {
    "machine": "order",
    "version": 1,
    "definition": {
      "states": ["pending", "paid", "shipped", "delivered"],
      "initial": "pending",
      "transitions": [
        {"from": "pending", "event": "PAY", "to": "paid"},
        {"from": "paid", "event": "SHIP", "to": "shipped"},
        {"from": "shipped", "event": "DELIVER", "to": "delivered"}
      ],
      "meta": {
        "description": "Order lifecycle"
      }
    },
    "checksum": "optional-sha256-hex"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine` | string | Yes | Machine name |
| `version` | integer | Yes | Version number (>= 1) |
| `definition` | object | Yes | Machine definition |
| `checksum` | string | No | Optional SHA-256 hex for verification |

### Definition Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `states` | string[] | Yes | List of valid states |
| `initial` | string | Yes | Initial state for new instances |
| `transitions` | Transition[] | Yes | Allowed transitions |
| `meta` | object | No | Arbitrary metadata |

### Transition Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `from` | string \| string[] | Yes | Source state(s) |
| `event` | string | Yes | Event name |
| `to` | string | Yes | Target state |
| `guard` | string | No | Guard expression |

### Response

```json
{
  "status": "ok",
  "result": {
    "machine": "order",
    "version": 1,
    "stored_checksum": "a1b2c3d4...",
    "created": true
  }
}
```

| Field | Description |
|-------|-------------|
| `machine` | Machine name |
| `version` | Version number |
| `stored_checksum` | SHA-256 hex of stored definition |
| `created` | True if newly created, false if identical definition already existed (idempotent) |

### Errors

| Code | Description |
|------|-------------|
| `MACHINE_VERSION_EXISTS` | Version already exists with different definition |
| `MACHINE_VERSION_LIMIT_EXCEEDED` | Max versions reached |
| `BAD_REQUEST` | Definition validation failed |

### Validation Rules

The definition is validated:

1. `states` must be non-empty
2. `initial` must be in `states`
3. All transition `from` states must be in `states`
4. All transition `to` states must be in `states`
5. Guard expressions must be syntactically valid

---

## GET_MACHINE

Retrieves a machine definition.

### Request

```json
{
  "op": "GET_MACHINE",
  "params": {
    "machine": "order",
    "version": 1
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine` | string | Yes | Machine name |
| `version` | integer | Yes | Specific version |

### Response

```json
{
  "status": "ok",
  "result": {
    "definition": {
      "states": ["pending", "paid", "shipped", "delivered"],
      "initial": "pending",
      "transitions": [...]
    },
    "checksum": "a1b2c3d4..."
  }
}
```

| Field | Description |
|-------|-------------|
| `definition` | Machine definition object |
| `checksum` | SHA-256 hex of stored definition |

### Errors

| Code | Description |
|------|-------------|
| `MACHINE_NOT_FOUND` | Machine or version not found |

---

## LIST_MACHINES

Lists all registered machines.

### Request

```json
{
  "op": "LIST_MACHINES"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "items": [
      {
        "machine": "order",
        "versions": [1, 2]
      },
      {
        "machine": "user",
        "versions": [1]
      }
    ]
  }
}
```

| Field | Description |
|-------|-------------|
| `items` | List of machine summaries (sorted alphabetically by name) |
| `items[].machine` | Machine name |
| `items[].versions` | Available version numbers |

---

## Examples

### Create with Guards

```json
{
  "op": "PUT_MACHINE",
  "params": {
    "machine": "approval",
    "version": 1,
    "definition": {
      "states": ["pending", "approved", "escalated", "rejected"],
      "initial": "pending",
      "transitions": [
        {
          "from": "pending",
          "event": "APPROVE",
          "to": "approved",
          "guard": "ctx.amount <= 1000"
        },
        {
          "from": "pending",
          "event": "APPROVE",
          "to": "escalated",
          "guard": "ctx.amount > 1000"
        },
        {
          "from": "pending",
          "event": "REJECT",
          "to": "rejected"
        },
        {
          "from": "escalated",
          "event": "APPROVE",
          "to": "approved"
        },
        {
          "from": "escalated",
          "event": "REJECT",
          "to": "rejected"
        }
      ]
    }
  }
}
```

### Multi-Source Transitions

```json
{
  "op": "PUT_MACHINE",
  "params": {
    "machine": "task",
    "version": 1,
    "definition": {
      "states": ["todo", "in_progress", "done", "cancelled"],
      "initial": "todo",
      "transitions": [
        {"from": "todo", "event": "START", "to": "in_progress"},
        {"from": "in_progress", "event": "COMPLETE", "to": "done"},
        {"from": ["todo", "in_progress"], "event": "CANCEL", "to": "cancelled"}
      ]
    }
  }
}
```
