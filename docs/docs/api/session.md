---
sidebar_position: 2
---

# Session Commands

Commands for managing client sessions.

## HELLO

Initiates a connection and negotiates protocol version and wire mode.

**Must be the first command sent on a new connection.**

### Request

```json
{
  "op": "HELLO",
  "params": {
    "protocol_version": 1,
    "client_name": "my-app",
    "wire_modes": ["binary_json", "jsonl"],
    "features": ["idempotency", "batch", "wal_read"]
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `protocol_version` | integer | Yes | Requested protocol version |
| `client_name` | string | No | Client application name |
| `wire_modes` | string[] | No | Preferred wire modes (priority-ordered) |
| `features` | string[] | No | Requested features |

### Response

```json
{
  "status": "ok",
  "result": {
    "protocol_version": 1,
    "wire_mode": "binary_json",
    "server_name": "rstmdb",
    "server_version": "0.1.1",
    "features": ["idempotency", "batch", "wal_read"]
  }
}
```

| Field | Description |
|-------|-------------|
| `protocol_version` | Negotiated version |
| `wire_mode` | Selected wire mode from client's priority list |
| `server_name` | Server name |
| `server_version` | Server version string |
| `features` | Intersection of requested and available features |

### Errors

| Code | Description |
|------|-------------|
| `UNSUPPORTED_PROTOCOL` | Protocol version not supported |

---

## AUTH

Authenticates the session with a bearer token.

### Request

```json
{
  "op": "AUTH",
  "params": {
    "method": "bearer",
    "token": "your-secret-token"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `method` | string | Yes | Authentication method (only `"bearer"` supported) |
| `token` | string | Yes | Bearer token |

### Response

```json
{
  "status": "ok",
  "result": {
    "authenticated": true
  }
}
```

### Errors

| Code | Description |
|------|-------------|
| `AUTH_FAILED` | Invalid token |

### Token Hashing

Tokens are validated against SHA-256 hashes stored in the server config:

```bash
# Generate hash for a token
rstmdb-cli hash-token my-secret-token
# Output: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

Multiple tokens can be configured simultaneously. Tokens can also be loaded from a secrets file via the `secrets_file` config option.

---

## PING

Health check command.

### Request

```json
{
  "op": "PING"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "pong": true
  }
}
```

Use for:
- Connection health checks
- Keep-alive in long-running connections
- Load balancer health probes

---

## INFO

Returns server information and capabilities.

### Request

```json
{
  "op": "INFO"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "server_name": "rstmdb",
    "server_version": "0.1.1",
    "protocol_version": 1,
    "features": ["idempotency", "batch", "wal_read"],
    "max_frame_bytes": 16777216,
    "max_batch_ops": 100
  }
}
```

| Field | Description |
|-------|-------------|
| `server_name` | Server name |
| `server_version` | Server version |
| `protocol_version` | Protocol version |
| `features` | Available features |
| `max_frame_bytes` | Maximum frame payload size |
| `max_batch_ops` | Maximum operations per batch |

---

## BYE

Gracefully closes the connection.

### Request

```json
{
  "op": "BYE"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "goodbye": true
  }
}
```

After sending BYE:
1. Server sends response
2. Server closes the connection
3. Any pending subscriptions are cancelled

**Note:** You can also close the TCP connection directly, but BYE ensures clean subscription cleanup.
