---
sidebar_position: 2
---

# Instances

Instances are individual occurrences of a state machine. Each instance has its own state, context, and history.

## Instance Structure

An instance contains:

```json
{
  "id": "order-123",
  "machine": "order",
  "version": 1,
  "state": "paid",
  "context": {
    "customer": "alice",
    "total": 99.99,
    "items": ["item-1", "item-2"]
  },
  "created_at": "2024-01-15T10:30:00Z",
  "updated_at": "2024-01-15T11:45:00Z"
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Unique identifier (client-provided or generated) |
| `machine` | `string` | Name of the state machine |
| `version` | `integer` | Version of the state machine |
| `state` | `string` | Current state |
| `context` | `object` | Arbitrary JSON data |
| `created_at` | `datetime` | Creation timestamp |
| `updated_at` | `datetime` | Last modification timestamp |

## Creating Instances

### Basic Creation

```bash
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"customer": "alice"}'
```

### With Initial Context

The context is a JSON object that can contain any data:

```bash
rstmdb-cli create-instance -m order -V 1 -i order-002 -c '{
  "customer": "bob",
  "email": "bob@example.com",
  "items": [
    {"sku": "SKU-001", "qty": 2, "price": 29.99},
    {"sku": "SKU-002", "qty": 1, "price": 49.99}
  ],
  "total": 109.97
}'
```

### Idempotent Creation

Use an idempotency key to safely retry creation:

```bash
# First call creates the instance
rstmdb-cli create-instance -m order -V 1 -i order-003 -c '{}' --idempotency-key "create-order-003"

# Retry with same key returns existing instance (no error)
rstmdb-cli create-instance -m order -V 1 -i order-003 -c '{}' --idempotency-key "create-order-003"
```

## Getting Instances

### Get by ID

```bash
rstmdb-cli get-instance order-001
```

### List Instances

```bash
# List all instances
rstmdb-cli list-instances

# Filter by machine
rstmdb-cli list-instances --machine order

# Filter by state
rstmdb-cli list-instances --machine order --state paid

# Pagination
rstmdb-cli list-instances --limit 100 --offset 0
```

## Instance Context

The context is mutable JSON data that travels with the instance. It's used for:

1. **Storing business data** - Order details, user preferences, etc.
2. **Guard evaluation** - Conditions can reference context fields
3. **Event payload merging** - Event payloads are merged into context

### Context Merging

When an event is applied, the payload is shallow-merged into the context:

```bash
# Initial context: {"customer": "alice"}
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"customer": "alice"}'

# Apply event with payload
rstmdb-cli apply-event -i order-001 -e PAY -p '{"payment_id": "pay-123", "amount": 99.99}'

# Context is now: {"customer": "alice", "payment_id": "pay-123", "amount": 99.99}
```

### Nested Context

Context supports nested objects:

```json
{
  "customer": {
    "id": "cust-001",
    "name": "Alice",
    "tier": "gold"
  },
  "shipping": {
    "address": "123 Main St",
    "method": "express"
  }
}
```

Guards can access nested fields: `ctx.customer.tier == "gold"`

## Deleting Instances

Instances support soft deletion:

```bash
rstmdb-cli delete-instance order-001
```

Deleted instances:
- Are marked as deleted but not immediately removed
- Can be excluded from list queries
- Are cleaned up during compaction

## Instance Lifecycle

```
┌─────────────┐
│   Create    │
└──────┬──────┘
       │
       ▼
┌─────────────┐     ┌─────────────┐
│   Initial   │────▶│   State 2   │────▶ ...
│   State     │     │             │
└─────────────┘     └─────────────┘
                          │
                          ▼
                    ┌─────────────┐
                    │  Terminal   │
                    │   State     │
                    └──────┬──────┘
                           │
                           ▼
                    ┌─────────────┐
                    │   Delete    │
                    │  (optional) │
                    └─────────────┘
```

## Best Practices

### Use Meaningful IDs

```bash
# Good - includes context
rstmdb-cli create-instance ... -i "order-2024-01-001"
rstmdb-cli create-instance ... -i "user-alice-session-abc123"

# Bad - opaque random IDs make debugging harder
rstmdb-cli create-instance ... -i "a1b2c3d4"
```

### Initialize with Required Context

Include all context needed for guards at creation:

```bash
# If guards check ctx.tier, include it at creation
rstmdb-cli create-instance -m approval -V 1 -i req-001 -c '{
  "amount": 5000,
  "tier": "standard",
  "requester": "alice"
}'
```

### Keep Context Focused

Store only what's needed for state management:

```json
// Good - relevant state data
{
  "order_id": "ORD-001",
  "total": 99.99,
  "paid": true
}

// Bad - excessive detail better stored elsewhere
{
  "order_id": "ORD-001",
  "total": 99.99,
  "items": [/* 500 line items */],
  "audit_log": [/* 1000 entries */]
}
```

### Handle Idempotency

For critical operations, use idempotency keys:

```bash
# Generate a unique key per logical operation
IDEMP_KEY="create-order-$(uuidgen)"
rstmdb-cli create-instance ... --idempotency-key "$IDEMP_KEY"
```
