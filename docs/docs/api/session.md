---
sidebar_position: 2
---

# Session Commands

Commands for managing client sessions.

## HELLO

Initiates a connection and negotiates protocol version.

**Must be the first command sent on a new connection.**

### Request

```json
{
  "op": "HELLO",
  "params": {
    "protocol_version": 1,
    "client_name": "my-app",
    "client_version": "1.0.0",
    "features": ["subscriptions"]
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `protocol_version` | integer | Yes | Requested protocol version |
| `client_name` | string | No | Client application name |
| `client_version` | string | No | Client version |
| `features` | string[] | No | Requested features |

### Response

```json
{
  "status": "ok",
  "result": {
    "protocol_version": 1,
    "server_version": "0.1.0",
    "auth_required": true,
    "features": ["subscriptions", "batch"],
    "limits": {
      "max_frame_size": 16777216,
      "max_instances_per_list": 1000,
      "max_subscriptions": 100
    }
  }
}
```

| Field | Description |
|-------|-------------|
| `protocol_version` | Negotiated version |
| `server_version` | Server version string |
| `auth_required` | Whether AUTH is required |
| `features` | Available features |
| `limits` | Server limits |

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
    "token": "your-secret-token"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `token` | string | Yes | Bearer token |

### Response

```json
{
  "status": "ok",
  "result": {
    "authenticated": true,
    "permissions": ["read", "write", "admin"]
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
    "version": "0.1.0",
    "protocol_version": 1,
    "uptime_secs": 3600,
    "features": ["subscriptions", "batch", "tls"],
    "stats": {
      "connections": 10,
      "machines": 5,
      "instances": 1000,
      "wal_offset": 50000,
      "wal_size_bytes": 10485760
    },
    "config": {
      "auth_required": true,
      "max_connections": 1000,
      "fsync_policy": "every_write"
    }
  }
}
```

| Field | Description |
|-------|-------------|
| `version` | Server version |
| `uptime_secs` | Seconds since start |
| `features` | Enabled features |
| `stats` | Runtime statistics |
| `config` | Relevant configuration |

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
