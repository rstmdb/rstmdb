---
sidebar_position: 4
---

# Error Codes

This document lists all error codes returned by rstmdb.

## Error Response Format

```json
{
  "type": "response",
  "id": "request-id",
  "status": "error",
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description",
    "retryable": false,
    "details": {}
  }
}
```

## Error Categories

### Protocol Errors

Errors related to protocol violations.

| Code | Retryable | Description |
|------|-----------|-------------|
| `UNSUPPORTED_PROTOCOL` | No | Client protocol version not supported |
| `BAD_REQUEST` | No | Malformed request (invalid JSON, missing fields) |
| `UNKNOWN_OPERATION` | No | Unrecognized operation name |

**Example:**
```json
{
  "error": {
    "code": "BAD_REQUEST",
    "message": "Missing required field 'op'",
    "retryable": false
  }
}
```

### Authentication Errors

Errors related to authentication and authorization.

| Code | Retryable | Description |
|------|-----------|-------------|
| `UNAUTHORIZED` | No | Authentication required but not provided |
| `AUTH_FAILED` | No | Invalid credentials |

**Example:**
```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Authentication required",
    "retryable": false
  }
}
```

### Resource Errors

Errors related to machines and instances.

| Code | Retryable | Description |
|------|-----------|-------------|
| `NOT_FOUND` | No | Generic resource not found |
| `MACHINE_NOT_FOUND` | No | Machine definition not found |
| `MACHINE_VERSION_EXISTS` | No | Machine version already exists |
| `MACHINE_VERSION_LIMIT_EXCEEDED` | No | Maximum versions per machine reached |
| `INSTANCE_NOT_FOUND` | No | Instance not found |
| `INSTANCE_EXISTS` | No | Instance already exists |

**Example:**
```json
{
  "error": {
    "code": "INSTANCE_NOT_FOUND",
    "message": "Instance 'order-999' not found",
    "retryable": false,
    "details": {
      "instance_id": "order-999"
    }
  }
}
```

### State Machine Errors

Errors related to state transitions.

| Code | Retryable | Description |
|------|-----------|-------------|
| `INVALID_TRANSITION` | No | No valid transition for event from current state |
| `GUARD_FAILED` | No | Guard condition evaluated to false |
| `INVALID_DEFINITION` | No | Machine definition is invalid |

**Example: Invalid Transition**
```json
{
  "error": {
    "code": "INVALID_TRANSITION",
    "message": "No transition from 'shipped' on event 'PAY'",
    "retryable": false,
    "details": {
      "current_state": "shipped",
      "event": "PAY"
    }
  }
}
```

**Example: Guard Failed**
```json
{
  "error": {
    "code": "GUARD_FAILED",
    "message": "Guard condition 'ctx.amount <= 1000' failed",
    "retryable": false,
    "details": {
      "guard": "ctx.amount <= 1000",
      "context": {"amount": 5000}
    }
  }
}
```

### Concurrency Errors

Errors related to concurrent operations.

| Code | Retryable | Description |
|------|-----------|-------------|
| `CONFLICT` | Yes | Concurrent modification detected |

**Example:**
```json
{
  "error": {
    "code": "CONFLICT",
    "message": "Instance was modified by another request",
    "retryable": true,
    "details": {
      "expected_offset": 100,
      "actual_offset": 105
    }
  }
}
```

### System Errors

Errors related to server internals.

| Code | Retryable | Description |
|------|-----------|-------------|
| `WAL_IO_ERROR` | Yes | Failed to write to WAL |
| `INTERNAL_ERROR` | Yes | Unexpected server error |
| `RATE_LIMITED` | Yes | Too many requests |

**Example:**
```json
{
  "error": {
    "code": "INTERNAL_ERROR",
    "message": "An unexpected error occurred",
    "retryable": true
  }
}
```

## Handling Errors

### Non-Retryable Errors

These indicate a logical error that won't succeed with retry:

```javascript
if (error.code === 'INSTANCE_NOT_FOUND') {
  // Instance doesn't exist, create it first
}

if (error.code === 'INVALID_TRANSITION') {
  // Event not valid for current state
  // Check instance state before retrying
}
```

### Retryable Errors

These may succeed on retry:

```javascript
async function applyEventWithRetry(instanceId, event, maxRetries = 3) {
  for (let i = 0; i < maxRetries; i++) {
    try {
      return await client.applyEvent(instanceId, event);
    } catch (error) {
      if (!error.retryable || i === maxRetries - 1) {
        throw error;
      }
      await sleep(100 * Math.pow(2, i)); // Exponential backoff
    }
  }
}
```

### Conflict Resolution

For `CONFLICT` errors, re-fetch and retry:

```javascript
async function safeApplyEvent(instanceId, event, payload) {
  while (true) {
    try {
      return await client.applyEvent(instanceId, event, payload);
    } catch (error) {
      if (error.code !== 'CONFLICT') {
        throw error;
      }
      // State changed, get current state and decide
      const instance = await client.getInstance(instanceId);
      if (shouldStillApply(instance, event)) {
        continue; // Retry
      } else {
        return instance; // Accept current state
      }
    }
  }
}
```

## Error Details

Some errors include additional details:

### INVALID_DEFINITION

```json
{
  "error": {
    "code": "INVALID_DEFINITION",
    "message": "Invalid machine definition",
    "details": {
      "errors": [
        {"path": "initial", "message": "Initial state 'foo' not in states list"},
        {"path": "transitions[0].to", "message": "Target state 'bar' not in states list"}
      ]
    }
  }
}
```

### BATCH Partial Failure

When batch mode is `best_effort`:

```json
{
  "status": "ok",
  "result": {
    "results": [
      {"status": "ok", "result": {...}},
      {"status": "error", "error": {"code": "INSTANCE_NOT_FOUND", ...}},
      {"status": "ok", "result": {...}}
    ]
  }
}
```

## Best Practices

1. **Always check `retryable`** before implementing retry logic
2. **Use exponential backoff** for retryable errors
3. **Log error details** for debugging
4. **Handle specific codes** rather than generic error handling
5. **Use idempotency keys** to safely retry operations
