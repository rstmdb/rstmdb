---
sidebar_position: 1
---

# Protocol Overview

rstmdb uses the **RCP (rstmdb Command Protocol)**, a request-response protocol over TCP with optional TLS encryption.

## Connection Details

| Property | Value |
|----------|-------|
| Default Port | 7401 |
| Transport | TCP |
| Encryption | Optional TLS/mTLS |
| Authentication | Optional bearer token |

## Protocol Characteristics

- **Request-response model** - Each request gets exactly one response
- **Multiplexed** - Multiple requests can be in-flight on a single connection
- **Streaming support** - Subscriptions send multiple events after initial response
- **JSON-based** - All messages are JSON-encoded

## Connection Lifecycle

```
Client                                    Server
   │                                         │
   │ ─────────── TCP Connect ───────────────▶│
   │                                         │
   │ ─────────── HELLO ─────────────────────▶│
   │ ◀────────── HELLO (ack) ────────────────│
   │                                         │
   │ ─────────── AUTH (optional) ───────────▶│
   │ ◀────────── OK ─────────────────────────│
   │                                         │
   │ ─────────── REQUEST ───────────────────▶│
   │ ◀────────── RESPONSE ───────────────────│
   │                                         │
   │ ─────────── REQUEST ───────────────────▶│
   │ ◀────────── RESPONSE ───────────────────│
   │                                         │
   │ ─────────── BYE ───────────────────────▶│
   │ ◀────────── BYE ────────────────────────│
   │                                         │
   │ ◀────────── TCP Close ──────────────────│
```

## Wire Modes

### Binary JSON (Default)

The default mode uses a framed binary format:

```
┌────────────────────────────────────────────────┐
│                   Frame                         │
├────────┬────────┬──────────┬──────────────────┤
│ Magic  │ Flags  │ CRC32C   │ JSON Payload     │
│ "RCP1" │ 4 bytes│ 4 bytes  │ variable         │
└────────┴────────┴──────────┴──────────────────┘
```

Benefits:
- CRC32C checksums for data integrity
- Clear frame boundaries
- Efficient parsing

### JSONL Mode (Debug)

For debugging, a line-delimited JSON mode is available:

```
{"type":"request","id":"1","op":"PING"}\n
{"type":"response","id":"1","status":"ok"}\n
```

Enable with the `--jsonl` flag or `wire_mode: jsonl` in config.

## Message Types

### Request

```json
{
  "type": "request",
  "id": "unique-request-id",
  "op": "OPERATION_NAME",
  "params": {
    // Operation-specific parameters
  }
}
```

### Response

```json
{
  "type": "response",
  "id": "matching-request-id",
  "status": "ok",
  "result": {
    // Operation-specific result
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
  "id": "matching-request-id",
  "status": "error",
  "error": {
    "code": "ERROR_CODE",
    "message": "Human-readable description",
    "retryable": false,
    "details": {}
  }
}
```

### Event (Subscription)

```json
{
  "type": "event",
  "subscription_id": "sub-123",
  "event": {
    "instance_id": "order-001",
    "event": "PAY",
    "from_state": "pending",
    "to_state": "paid",
    "payload": {},
    "timestamp": "2024-01-15T10:30:00Z",
    "wal_offset": 12345
  }
}
```

## Handshake

Every connection starts with a HELLO exchange:

**Client sends:**
```json
{
  "type": "request",
  "id": "1",
  "op": "HELLO",
  "params": {
    "protocol_version": 1,
    "client_name": "rstmdb-cli",
    "client_version": "0.1.0"
  }
}
```

**Server responds:**
```json
{
  "type": "response",
  "id": "1",
  "status": "ok",
  "result": {
    "protocol_version": 1,
    "server_version": "0.1.0",
    "auth_required": true,
    "features": ["subscriptions", "batch"]
  }
}
```

## Authentication

If `auth_required` is true, authenticate before other operations:

**Client sends:**
```json
{
  "type": "request",
  "id": "2",
  "op": "AUTH",
  "params": {
    "token": "your-secret-token"
  }
}
```

**Server responds:**
```json
{
  "type": "response",
  "id": "2",
  "status": "ok",
  "result": {
    "authenticated": true
  }
}
```

## Pipelining

Multiple requests can be sent without waiting for responses:

```
Client                          Server
   │                               │
   │ ──── Request A (id=1) ───────▶│
   │ ──── Request B (id=2) ───────▶│
   │ ──── Request C (id=3) ───────▶│
   │                               │
   │ ◀─── Response (id=1) ─────────│
   │ ◀─── Response (id=2) ─────────│
   │ ◀─── Response (id=3) ─────────│
```

Responses may arrive out of order. Match responses to requests using the `id` field.

## Subscriptions

Subscriptions create persistent event streams:

```
Client                          Server
   │                               │
   │ ──── WATCH_ALL ──────────────▶│
   │ ◀─── OK (subscription_id) ────│
   │                               │
   │ ◀─── Event ───────────────────│  (async)
   │ ◀─── Event ───────────────────│  (async)
   │                               │
   │ ──── Other Request ──────────▶│
   │ ◀─── Response ────────────────│
   │                               │
   │ ◀─── Event ───────────────────│  (async)
   │                               │
   │ ──── UNWATCH ────────────────▶│
   │ ◀─── OK ──────────────────────│
```

Events are pushed asynchronously and can interleave with request/response pairs.

## Connection Limits

| Limit | Default |
|-------|---------|
| Max connections | 1000 |
| Max frame size | 16 MiB |
| Idle timeout | 300 seconds |
| Max request id length | 256 bytes |

## TLS

For encrypted connections:

```bash
# Client with TLS
rstmdb-cli --tls --ca-cert ca.pem -s server:7401 ping

# Client with mTLS
rstmdb-cli --tls --ca-cert ca.pem --client-cert client.pem --client-key client-key.pem ping
```

Server configuration:
```yaml
tls:
  enabled: true
  cert_path: /path/to/server.pem
  key_path: /path/to/server-key.pem
  require_client_cert: false  # true for mTLS
  client_ca_path: /path/to/client-ca.pem
```
