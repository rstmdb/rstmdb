# RCP Protocol Overview

RCP (rstmdb Command Protocol) is a TCP-based, command-oriented protocol for interacting with the rstmdb server.

## Quick Reference

### Connection

| Property       | Value                   |
| -------------- | ----------------------- |
| Default Port   | 7401                    |
| Transport      | TCP with optional TLS   |
| Authentication | Bearer token (optional) |

### Wire Modes

**Binary JSON (default)**

- 16-byte frame header with magic bytes `RCP1`
- CRC32C checksum validation (optional)
- Efficient for production use

**JSONL (debug mode)**

- Line-delimited JSON (`\n` separator)
- Human-readable for debugging
- Negotiated via HELLO handshake

### Handshake

```
Client                              Server
  │                                   │
  │─────── HELLO ────────────────────►│
  │  protocol_version: 1              │
  │  wire_modes: [binary_json, jsonl] │
  │  features: [idempotency, batch]   │
  │                                   │
  │◄────── HELLO_OK ──────────────────│
  │  protocol_version: 1              │
  │  wire_mode: binary_json           │
  │  features: [idempotency, batch,   │
  │             watch, wal_read]      │
  │                                   │
  │─────── AUTH (optional) ──────────►│
  │  method: bearer                   │
  │  token: <token>                   │
  │                                   │
  │◄────── AUTH_OK ───────────────────│
  │                                   │
```

## Operations Summary

### Session Management

| Operation | Description                                |
| --------- | ------------------------------------------ |
| `HELLO`   | Protocol negotiation and feature discovery |
| `AUTH`    | Authenticate with bearer token             |
| `PING`    | Connection keepalive, returns PONG         |
| `BYE`     | Graceful disconnect                        |
| `INFO`    | Server capabilities and limits             |

### Machine Definitions

| Operation       | Description                                       |
| --------------- | ------------------------------------------------- |
| `PUT_MACHINE`   | Register or update a machine definition           |
| `GET_MACHINE`   | Retrieve a machine definition by name and version |
| `LIST_MACHINES` | List all machines with optional pagination        |

### Instance Lifecycle

| Operation         | Description                                  |
| ----------------- | -------------------------------------------- |
| `CREATE_INSTANCE` | Create a new state machine instance          |
| `GET_INSTANCE`    | Get current state and context of an instance |
| `DELETE_INSTANCE` | Soft-delete an instance                      |

### Events

| Operation     | Description                                         |
| ------------- | --------------------------------------------------- |
| `APPLY_EVENT` | Apply an event to trigger a state transition        |
| `BATCH`       | Execute multiple operations (atomic or best_effort) |

### Storage & WAL

| Operation           | Description                                       |
| ------------------- | ------------------------------------------------- |
| `SNAPSHOT_INSTANCE` | Force snapshot creation for an instance           |
| `WAL_READ`          | Read WAL entries from offset (supports streaming) |
| `COMPACT`           | Trigger WAL compaction                            |

### Streaming

| Operation        | Description                                     |
| ---------------- | ----------------------------------------------- |
| `WATCH_INSTANCE` | Subscribe to state changes for an instance      |
| `WATCH_ALL`      | Subscribe to all events with optional filtering |
| `UNWATCH`        | Cancel a subscription                           |

## Message Format

### Request

```json
{
  "type": "request",
  "id": "unique-request-id",
  "op": "OPERATION_NAME",
  "params": {
    // operation-specific parameters
  }
}
```

### Success Response

```json
{
  "type": "response",
  "id": "same-as-request-id",
  "status": "ok",
  "result": {
    // operation-specific result
  },
  "meta": {
    "server_time": "2024-01-15T10:30:00Z",
    "wal_offset": 12345
  }
}
```

### Error Response

```json
{
  "type": "response",
  "id": "same-as-request-id",
  "status": "error",
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description",
    "retryable": false,
    "details": {}
  }
}
```

## Error Codes

| Code                     | Category    | Retryable | Description                  |
| ------------------------ | ----------- | --------- | ---------------------------- |
| `UNSUPPORTED_PROTOCOL`   | Protocol    | No        | Invalid protocol version     |
| `BAD_REQUEST`            | Protocol    | No        | Malformed request            |
| `UNAUTHORIZED`           | Auth        | No        | Authentication required      |
| `AUTH_FAILED`            | Auth        | No        | Invalid credentials          |
| `NOT_FOUND`              | Resource    | No        | Generic not found            |
| `MACHINE_NOT_FOUND`      | Resource    | No        | Machine definition not found |
| `MACHINE_VERSION_EXISTS` | Resource    | No        | Version already exists       |
| `INSTANCE_NOT_FOUND`     | Resource    | No        | Instance not found           |
| `INSTANCE_EXISTS`        | Resource    | No        | Instance already exists      |
| `INVALID_TRANSITION`     | State       | No        | Transition not allowed       |
| `GUARD_FAILED`           | State       | No        | Guard condition failed       |
| `CONFLICT`               | Concurrency | No        | State/offset mismatch        |
| `WAL_IO_ERROR`           | System      | Yes       | WAL I/O failure              |
| `INTERNAL_ERROR`         | System      | Yes       | Server error                 |
| `RATE_LIMITED`           | System      | Yes       | Rate limit exceeded          |

## Common Examples

### Connect and Authenticate

```json
// 1. HELLO
{"type":"request","id":"1","op":"HELLO","params":{"protocol_version":1,"client_name":"my-app","wire_modes":["binary_json"],"features":["idempotency"]}}

// Response
{"type":"response","id":"1","status":"ok","result":{"protocol_version":1,"wire_mode":"binary_json","server_name":"rstmdb","server_version":"0.1.0","features":["idempotency","batch","watch","wal_read"]}}

// 2. AUTH (if required)
{"type":"request","id":"2","op":"AUTH","params":{"method":"bearer","token":"my-secret-token"}}

// Response
{"type":"response","id":"2","status":"ok","result":{}}
```

### Create Instance and Apply Event

```json
// 1. Register machine definition
{"type":"request","id":"10","op":"PUT_MACHINE","params":{"machine":"order","version":1,"definition":{"states":["created","paid","shipped"],"initial":"created","transitions":[{"from":"created","event":"PAY","to":"paid"},{"from":"paid","event":"SHIP","to":"shipped"}]}}}

// Response
{"type":"response","id":"10","status":"ok","result":{"machine":"order","version":1,"created":true}}

// 2. Create instance
{"type":"request","id":"11","op":"CREATE_INSTANCE","params":{"machine":"order","version":1,"initial_ctx":{"customer_id":"c-123"},"idempotency_key":"create-order-1"}}

// Response
{"type":"response","id":"11","status":"ok","result":{"instance_id":"i-abc123","state":"created","wal_offset":1001},"meta":{"wal_offset":1001}}

// 3. Apply event
{"type":"request","id":"12","op":"APPLY_EVENT","params":{"instance_id":"i-abc123","event":"PAY","payload":{"amount":99.99},"idempotency_key":"pay-order-1"}}

// Response
{"type":"response","id":"12","status":"ok","result":{"from_state":"created","to_state":"paid","wal_offset":1002,"applied":true}}
```

### Subscribe to Instance Changes

```json
// 1. Start watching
{"type":"request","id":"20","op":"WATCH_INSTANCE","params":{"instance_id":"i-abc123","include_ctx":true}}

// Initial response
{"type":"response","id":"20","status":"ok","result":{"subscription_id":"sub-xyz"}}

// Streamed events (when instance changes)
{"type":"event","subscription_id":"sub-xyz","instance_id":"i-abc123","wal_offset":1003,"from_state":"paid","to_state":"shipped","event":"SHIP"}

// 2. Cancel subscription
{"type":"request","id":"21","op":"UNWATCH","params":{"subscription_id":"sub-xyz"}}
```

### Optimistic Concurrency

```json
// Apply with expected state check
{"type":"request","id":"30","op":"APPLY_EVENT","params":{"instance_id":"i-abc123","event":"SHIP","expected_state":"paid"}}

// If state doesn't match
{"type":"response","id":"30","status":"error","error":{"code":"CONFLICT","message":"State mismatch","retryable":false,"details":{"expected_state":"paid","actual_state":"created"}}}
```

## Binary Frame Format

For production use, messages are wrapped in binary frames:

```
┌────────────────────────────────────────────────┐
│                 Frame Header (16 bytes)        │
├──────────┬──────────┬────────────┬─────────────┤
│  magic   │  flags   │ header_len │ payload_len │
│ (4 bytes)│ (2 bytes)│  (2 bytes) │  (4 bytes)  │
├──────────┴──────────┴────────────┴─────────────┤
│                   crc32c (4 bytes)             │
├────────────────────────────────────────────────┤
│           Header Extension (optional)          │
├────────────────────────────────────────────────┤
│                 JSON Payload                   │
└────────────────────────────────────────────────┘
```

- **magic**: ASCII `"RCP1"`
- **flags**: bit0=CRC_PRESENT, bit2=STREAM, bit3=END_STREAM
- **crc32c**: CRC32C of payload (validated if CRC_PRESENT flag set)

## See Also

- [Architecture](./architecture.md) - System design and internals
