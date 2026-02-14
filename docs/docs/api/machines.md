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
    "name": "order",
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
    }
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | Yes | Machine name |
| `version` | integer | Yes | Version number (≥ 1) |
| `definition` | object | Yes | Machine definition |

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
    "name": "order",
    "version": 1,
    "created": true
  }
}
```

| Field | Description |
|-------|-------------|
| `created` | True if newly created, false if updated |

### Errors

| Code | Description |
|------|-------------|
| `MACHINE_VERSION_EXISTS` | Version already exists |
| `MACHINE_VERSION_LIMIT_EXCEEDED` | Max versions reached |
| `INVALID_DEFINITION` | Definition validation failed |

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
    "name": "order",
    "version": 1
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | Yes | Machine name |
| `version` | integer | No | Specific version (omit for latest) |

### Response

```json
{
  "status": "ok",
  "result": {
    "name": "order",
    "version": 1,
    "definition": {
      "states": ["pending", "paid", "shipped", "delivered"],
      "initial": "pending",
      "transitions": [...]
    },
    "created_at": "2024-01-15T10:30:00Z"
  }
}
```

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
  "op": "LIST_MACHINES",
  "params": {
    "limit": 100,
    "offset": 0
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `limit` | integer | No | Max items (default: 100) |
| `offset` | integer | No | Skip items (default: 0) |

### Response

```json
{
  "status": "ok",
  "result": {
    "machines": [
      {
        "name": "order",
        "versions": [1, 2],
        "latest_version": 2,
        "instance_count": 500
      },
      {
        "name": "user",
        "versions": [1],
        "latest_version": 1,
        "instance_count": 100
      }
    ],
    "total": 2,
    "has_more": false
  }
}
```

| Field | Description |
|-------|-------------|
| `machines` | List of machine summaries |
| `total` | Total machine count |
| `has_more` | More items available |

### Machine Summary

| Field | Description |
|-------|-------------|
| `name` | Machine name |
| `versions` | Available versions |
| `latest_version` | Highest version number |
| `instance_count` | Number of instances |

---

## Examples

### Create with Guards

```json
{
  "op": "PUT_MACHINE",
  "params": {
    "name": "approval",
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
    "name": "task",
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
