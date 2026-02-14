---
sidebar_position: 1
---

# State Machines

State machines in rstmdb define the possible states and transitions for your data. They provide a formal model for managing state that ensures only valid transitions occur.

## Definition Format

A state machine definition is a JSON object with the following structure:

```json
{
  "states": ["state1", "state2", "state3"],
  "initial": "state1",
  "transitions": [
    {"from": "state1", "event": "EVENT_NAME", "to": "state2"},
    {"from": "state1", "event": "OTHER_EVENT", "to": "state3", "guard": "ctx.condition"}
  ],
  "meta": {
    "description": "Optional metadata"
  }
}
```

### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `states` | `string[]` | Yes | List of valid state names |
| `initial` | `string` | Yes | The starting state for new instances |
| `transitions` | `Transition[]` | Yes | List of allowed transitions |
| `meta` | `object` | No | Arbitrary metadata |

### Transition Object

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `from` | `string \| string[]` | Yes | Source state(s) |
| `event` | `string` | Yes | Event name that triggers the transition |
| `to` | `string` | Yes | Target state |
| `guard` | `string` | No | Condition expression |

## Creating a Machine

Use the `PUT_MACHINE` command via CLI or API:

```bash
rstmdb-cli put-machine -n order -v 1 '{
  "states": ["draft", "submitted", "approved", "rejected"],
  "initial": "draft",
  "transitions": [
    {"from": "draft", "event": "SUBMIT", "to": "submitted"},
    {"from": "submitted", "event": "APPROVE", "to": "approved"},
    {"from": "submitted", "event": "REJECT", "to": "rejected"},
    {"from": "rejected", "event": "RESUBMIT", "to": "submitted"}
  ]
}'
```

## Versioning

Machines are identified by name and version:

- **Name** - A unique identifier for the machine type (e.g., "order", "user", "document")
- **Version** - An integer version number (1, 2, 3, ...)

Instances are bound to a specific machine version at creation time. This allows you to evolve machine definitions while maintaining compatibility with existing instances.

```bash
# Create version 1
rstmdb-cli put-machine -n order -v 1 '{"states": ["a", "b"], ...}'

# Create version 2 with new states
rstmdb-cli put-machine -n order -v 2 '{"states": ["a", "b", "c"], ...}'

# New instances can use either version
rstmdb-cli create-instance -m order -V 1 -i old-order ...
rstmdb-cli create-instance -m order -V 2 -i new-order ...
```

## Multi-Source Transitions

A transition can originate from multiple states:

```json
{
  "states": ["pending", "processing", "completed", "failed", "cancelled"],
  "initial": "pending",
  "transitions": [
    {"from": "pending", "event": "START", "to": "processing"},
    {"from": "processing", "event": "COMPLETE", "to": "completed"},
    {"from": "processing", "event": "FAIL", "to": "failed"},
    {"from": ["pending", "processing", "failed"], "event": "CANCEL", "to": "cancelled"}
  ]
}
```

The `CANCEL` event can be applied from `pending`, `processing`, or `failed` states.

## Guard Conditions

Transitions can have guard conditions that must be satisfied:

```json
{
  "states": ["pending", "approved", "rejected", "escalated"],
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
    }
  ]
}
```

See [Guards](./guards) for the full expression language.

## Best Practices

### Use Meaningful State Names

```json
// Good - clear and descriptive
"states": ["draft", "pending_review", "approved", "published"]

// Bad - unclear abbreviations
"states": ["d", "pr", "a", "p"]
```

### Use Consistent Event Naming

```json
// Good - consistent verb format
"transitions": [
  {"event": "SUBMIT", ...},
  {"event": "APPROVE", ...},
  {"event": "REJECT", ...}
]

// Bad - inconsistent naming
"transitions": [
  {"event": "submit", ...},
  {"event": "APPROVED", ...},
  {"event": "rejection", ...}
]
```

### Model Terminal States Explicitly

Include states that represent completion:

```json
{
  "states": ["active", "completed", "cancelled", "expired"],
  "initial": "active",
  "transitions": [
    {"from": "active", "event": "COMPLETE", "to": "completed"},
    {"from": "active", "event": "CANCEL", "to": "cancelled"},
    {"from": "active", "event": "EXPIRE", "to": "expired"}
  ]
}
```

### Version for Breaking Changes

When you need to make incompatible changes:

1. Create a new version with the updated definition
2. New instances use the new version
3. Existing instances continue with their original version
4. Optionally migrate instances (create new, copy context, delete old)

## Examples

### Order Lifecycle

```json
{
  "states": ["cart", "checkout", "paid", "processing", "shipped", "delivered", "returned", "cancelled"],
  "initial": "cart",
  "transitions": [
    {"from": "cart", "event": "CHECKOUT", "to": "checkout"},
    {"from": "checkout", "event": "PAY", "to": "paid"},
    {"from": "checkout", "event": "ABANDON", "to": "cart"},
    {"from": "paid", "event": "PROCESS", "to": "processing"},
    {"from": "processing", "event": "SHIP", "to": "shipped"},
    {"from": "shipped", "event": "DELIVER", "to": "delivered"},
    {"from": "delivered", "event": "RETURN", "to": "returned"},
    {"from": ["cart", "checkout", "paid"], "event": "CANCEL", "to": "cancelled"}
  ]
}
```

### Document Approval

```json
{
  "states": ["draft", "submitted", "under_review", "approved", "rejected", "published"],
  "initial": "draft",
  "transitions": [
    {"from": "draft", "event": "SUBMIT", "to": "submitted"},
    {"from": "submitted", "event": "ASSIGN_REVIEWER", "to": "under_review"},
    {"from": "under_review", "event": "APPROVE", "to": "approved"},
    {"from": "under_review", "event": "REJECT", "to": "rejected"},
    {"from": "approved", "event": "PUBLISH", "to": "published"},
    {"from": "rejected", "event": "REVISE", "to": "draft"}
  ]
}
```

### IoT Device

```json
{
  "states": ["offline", "connecting", "online", "error", "maintenance"],
  "initial": "offline",
  "transitions": [
    {"from": "offline", "event": "CONNECT", "to": "connecting"},
    {"from": "connecting", "event": "CONNECTED", "to": "online"},
    {"from": "connecting", "event": "TIMEOUT", "to": "offline"},
    {"from": "online", "event": "DISCONNECT", "to": "offline"},
    {"from": "online", "event": "ERROR", "to": "error"},
    {"from": "error", "event": "RECOVER", "to": "online"},
    {"from": "error", "event": "DISCONNECT", "to": "offline"},
    {"from": ["online", "error"], "event": "MAINTAIN", "to": "maintenance"},
    {"from": "maintenance", "event": "COMPLETE", "to": "offline"}
  ]
}
```
