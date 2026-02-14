---
sidebar_position: 4
---

# Guards

Guards are boolean expressions that control when a transition can occur. They evaluate against the instance's context to enable conditional state transitions.

## Basic Syntax

Guards are specified as strings in transition definitions:

```json
{
  "from": "pending",
  "event": "APPROVE",
  "to": "approved",
  "guard": "ctx.amount <= 1000"
}
```

## Expression Language

### Field Access

Access context fields using `ctx.` prefix:

```
ctx.amount          // Top-level field
ctx.user.role       // Nested field
ctx.items.length    // Array property
```

### Comparison Operators

| Operator | Description | Example |
|----------|-------------|---------|
| `==` | Equal | `ctx.status == "active"` |
| `!=` | Not equal | `ctx.status != "blocked"` |
| `>` | Greater than | `ctx.amount > 100` |
| `>=` | Greater or equal | `ctx.amount >= 100` |
| `<` | Less than | `ctx.amount < 1000` |
| `<=` | Less or equal | `ctx.amount <= 1000` |

### Logical Operators

| Operator | Description | Example |
|----------|-------------|---------|
| `&&` | AND | `ctx.a && ctx.b` |
| `\|\|` | OR | `ctx.a \|\| ctx.b` |
| `!` | NOT | `!ctx.disabled` |

### Truthiness

A field is truthy if it exists and is not:
- `null`
- `false`
- `0`
- `""` (empty string)

```
ctx.enabled          // True if enabled is truthy
!ctx.disabled        // True if disabled is falsy or missing
```

## Examples

### Simple Comparison

```json
{
  "guard": "ctx.amount <= 1000"
}
```

### String Comparison

```json
{
  "guard": "ctx.status == \"active\""
}
```

### Boolean Check

```json
{
  "guard": "ctx.approved"
}
```

### Negation

```json
{
  "guard": "!ctx.blocked"
}
```

### Combined Conditions (AND)

```json
{
  "guard": "ctx.amount <= 1000 && ctx.approved"
}
```

### Combined Conditions (OR)

```json
{
  "guard": "ctx.vip || ctx.amount < 100"
}
```

### Complex Expression

```json
{
  "guard": "(ctx.tier == \"gold\" || ctx.tier == \"platinum\") && ctx.balance >= 0"
}
```

### Nested Field Access

```json
{
  "guard": "ctx.user.role == \"admin\" && ctx.request.priority == \"high\""
}
```

## Practical Patterns

### Approval Thresholds

Route approvals based on amount:

```json
{
  "transitions": [
    {"from": "pending", "event": "APPROVE", "to": "approved", "guard": "ctx.amount <= 1000"},
    {"from": "pending", "event": "APPROVE", "to": "manager_review", "guard": "ctx.amount > 1000 && ctx.amount <= 10000"},
    {"from": "pending", "event": "APPROVE", "to": "director_review", "guard": "ctx.amount > 10000"}
  ]
}
```

### Role-Based Access

Allow transitions based on user role:

```json
{
  "transitions": [
    {"from": "draft", "event": "PUBLISH", "to": "published", "guard": "ctx.author_role == \"editor\" || ctx.author_role == \"admin\""},
    {"from": "published", "event": "ARCHIVE", "to": "archived", "guard": "ctx.author_role == \"admin\""}
  ]
}
```

### Feature Flags

Enable transitions based on flags:

```json
{
  "transitions": [
    {"from": "standard", "event": "UPGRADE", "to": "premium", "guard": "ctx.premium_enabled && ctx.payment_verified"}
  ]
}
```

### Retry Limits

Limit retry attempts:

```json
{
  "transitions": [
    {"from": "failed", "event": "RETRY", "to": "processing", "guard": "ctx.retry_count < 3"},
    {"from": "failed", "event": "RETRY", "to": "abandoned", "guard": "ctx.retry_count >= 3"}
  ]
}
```

## Guard Evaluation

### Order of Evaluation

When multiple transitions match (same `from` and `event`), guards are evaluated in definition order. The first transition with a passing guard (or no guard) is used.

```json
{
  "transitions": [
    {"from": "pending", "event": "PROCESS", "to": "fast_track", "guard": "ctx.priority == \"high\""},
    {"from": "pending", "event": "PROCESS", "to": "standard", "guard": "ctx.priority == \"normal\""},
    {"from": "pending", "event": "PROCESS", "to": "standard"}  // Catch-all (no guard)
  ]
}
```

### Guard Failure

If all matching transitions have guards and none pass, the event fails with `GUARD_FAILED`:

```json
{
  "status": "error",
  "error": {
    "code": "GUARD_FAILED",
    "message": "No transition guard passed for event 'APPROVE' from state 'pending'"
  }
}
```

### Missing Fields

Accessing a missing field evaluates to `null`, which is falsy:

```
ctx.missing_field        // false (field doesn't exist)
ctx.missing_field == null  // true
!ctx.missing_field       // true
```

## Best Practices

### Keep Guards Simple

```json
// Good - clear and readable
"guard": "ctx.amount <= 1000"

// Avoid - overly complex
"guard": "((ctx.a && ctx.b) || (ctx.c && !ctx.d)) && (ctx.e >= 10 || ctx.f)"
```

### Use Meaningful Context Fields

```json
// Good - clear intent
"guard": "ctx.is_verified && ctx.has_payment_method"

// Avoid - unclear abbreviations
"guard": "ctx.v && ctx.pm"
```

### Provide Catch-All Transitions

When using multiple guarded transitions, consider a fallback:

```json
{
  "transitions": [
    {"from": "pending", "event": "DECIDE", "to": "approved", "guard": "ctx.score >= 80"},
    {"from": "pending", "event": "DECIDE", "to": "review", "guard": "ctx.score >= 50"},
    {"from": "pending", "event": "DECIDE", "to": "rejected"}  // Catch-all for score < 50
  ]
}
```

### Initialize Required Context

Ensure instances have the context fields guards depend on:

```bash
# Guard checks ctx.tier, so include it at creation
rstmdb-cli create-instance -m approval -V 1 -i req-001 -c '{
  "amount": 5000,
  "tier": "standard"
}'
```

## Limitations

Current guard expressions do not support:

- Array operations (`includes`, `length > N`)
- Arithmetic operations (`ctx.a + ctx.b > 100`)
- Date/time comparisons
- Regular expressions
- Function calls

For complex logic, consider:
- Pre-computing values in your application and storing in context
- Breaking complex conditions into multiple states/transitions
