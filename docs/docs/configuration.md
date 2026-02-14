---
sidebar_position: 9
---

# Configuration

rstmdb is configured through a YAML file and environment variables.

## Configuration Loading

Priority (highest to lowest):
1. Environment variables
2. Configuration file
3. Default values

Specify config file path:
```bash
RSTMDB_CONFIG=/etc/rstmdb/config.yaml rstmdb
```

## Full Configuration Reference

```yaml
# Network settings
network:
  # Address to bind to
  bind_addr: "127.0.0.1:7401"

  # Connection idle timeout in seconds
  idle_timeout_secs: 300

  # Maximum concurrent connections
  max_connections: 1000

# Storage settings
storage:
  # Data directory for WAL and snapshots
  data_dir: "./data"

  # WAL segment size in megabytes
  wal_segment_size_mb: 64

  # Fsync policy for durability
  # Options: every_write, never, {every_n: N}, {every_ms: N}
  fsync_policy: every_write

  # Maximum versions per machine (0 = unlimited)
  max_machine_versions: 0

# Authentication settings
auth:
  # Require authentication
  required: false

  # SHA-256 hashes of valid tokens
  token_hashes:
    - "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"

  # External file containing token hashes (one per line)
  secrets_file: "/etc/rstmdb/tokens"

# TLS settings
tls:
  # Enable TLS
  enabled: false

  # Server certificate path
  cert_path: "/etc/rstmdb/server.pem"

  # Server private key path
  key_path: "/etc/rstmdb/server-key.pem"

  # Require client certificates (mTLS)
  require_client_cert: false

  # CA certificate for client verification
  client_ca_path: "/etc/rstmdb/client-ca.pem"

# Automatic compaction settings
compaction:
  # Enable automatic compaction
  enabled: true

  # Compact after this many events
  events_threshold: 10000

  # Compact when WAL exceeds this size (MB)
  size_threshold_mb: 100

  # Minimum seconds between compactions
  min_interval_secs: 60

# Metrics settings
metrics:
  # Enable Prometheus metrics endpoint
  enabled: true

  # Metrics endpoint bind address
  bind_addr: "0.0.0.0:9090"

# Logging settings
logging:
  # Log level: trace, debug, info, warn, error
  level: "info"

  # Log format: json, pretty
  format: "json"
```

## Environment Variables

All configuration options can be set via environment variables:

### Network

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_BIND` | `network.bind_addr` | `127.0.0.1:7401` |
| `RSTMDB_IDLE_TIMEOUT` | `network.idle_timeout_secs` | `300` |
| `RSTMDB_MAX_CONNECTIONS` | `network.max_connections` | `1000` |

### Storage

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_DATA` | `storage.data_dir` | `./data` |
| `RSTMDB_WAL_SEGMENT_SIZE_MB` | `storage.wal_segment_size_mb` | `64` |
| `RSTMDB_FSYNC_POLICY` | `storage.fsync_policy` | `every_write` |
| `RSTMDB_MAX_MACHINE_VERSIONS` | `storage.max_machine_versions` | `0` |

### Authentication

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_AUTH_REQUIRED` | `auth.required` | `false` |
| `RSTMDB_AUTH_TOKEN_HASH` | `auth.token_hashes[0]` | None |
| `RSTMDB_AUTH_SECRETS_FILE` | `auth.secrets_file` | None |

### TLS

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_TLS_ENABLED` | `tls.enabled` | `false` |
| `RSTMDB_TLS_CERT` | `tls.cert_path` | None |
| `RSTMDB_TLS_KEY` | `tls.key_path` | None |
| `RSTMDB_TLS_CLIENT_CA` | `tls.client_ca_path` | None |
| `RSTMDB_TLS_REQUIRE_CLIENT_CERT` | `tls.require_client_cert` | `false` |

### Compaction

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_COMPACT_ENABLED` | `compaction.enabled` | `true` |
| `RSTMDB_COMPACT_EVENTS` | `compaction.events_threshold` | `10000` |
| `RSTMDB_COMPACT_SIZE_MB` | `compaction.size_threshold_mb` | `100` |
| `RSTMDB_COMPACT_INTERVAL` | `compaction.min_interval_secs` | `60` |

### Metrics

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RSTMDB_METRICS_ENABLED` | `metrics.enabled` | `true` |
| `RSTMDB_METRICS_BIND` | `metrics.bind_addr` | `0.0.0.0:9090` |

### Logging

| Variable | Config Path | Default |
|----------|-------------|---------|
| `RUST_LOG` | `logging.level` | `info` |
| `RSTMDB_LOG_FORMAT` | `logging.format` | `json` |

## Fsync Policies

### every_write (Default)

Safest option. Every write is synced to disk before acknowledgment.

```yaml
storage:
  fsync_policy: every_write
```

**Durability:** No data loss on crash
**Performance:** Slowest

### every_n

Sync after every N writes.

```yaml
storage:
  fsync_policy:
    every_n: 100
```

**Durability:** Up to N-1 writes at risk
**Performance:** Balanced

### every_ms

Sync at most every N milliseconds.

```yaml
storage:
  fsync_policy:
    every_ms: 100
```

**Durability:** Up to N ms of writes at risk
**Performance:** Balanced

### never

Never explicitly sync. Relies on OS buffering.

```yaml
storage:
  fsync_policy: never
```

**Durability:** All unsynced data at risk
**Performance:** Fastest

## Authentication Setup

### Generate Token Hash

```bash
rstmdb-cli hash-token my-secret-token
# 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

### Configure Server

```yaml
auth:
  required: true
  token_hashes:
    - "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
```

Or via environment:
```bash
export RSTMDB_AUTH_REQUIRED=true
export RSTMDB_AUTH_TOKEN_HASH=9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
```

### Using Secrets File

```bash
# /etc/rstmdb/tokens (one hash per line)
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
a5f3c6c86f1a6d3b8c4e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1
```

```yaml
auth:
  required: true
  secrets_file: "/etc/rstmdb/tokens"
```

## TLS Setup

### Generate Certificates

```bash
# Generate CA
openssl genrsa -out ca-key.pem 4096
openssl req -new -x509 -days 365 -key ca-key.pem -out ca.pem -subj "/CN=rstmdb-ca"

# Generate server certificate
openssl genrsa -out server-key.pem 4096
openssl req -new -key server-key.pem -out server.csr -subj "/CN=localhost"
openssl x509 -req -days 365 -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -out server.pem
```

### Configure TLS

```yaml
tls:
  enabled: true
  cert_path: "/etc/rstmdb/server.pem"
  key_path: "/etc/rstmdb/server-key.pem"
```

### mTLS (Mutual TLS)

```yaml
tls:
  enabled: true
  cert_path: "/etc/rstmdb/server.pem"
  key_path: "/etc/rstmdb/server-key.pem"
  require_client_cert: true
  client_ca_path: "/etc/rstmdb/client-ca.pem"
```

## Example Configurations

### Development

```yaml
network:
  bind_addr: "127.0.0.1:7401"

storage:
  data_dir: "./data"
  fsync_policy: never  # Fast for development

auth:
  required: false

metrics:
  enabled: false
```

### Production

```yaml
network:
  bind_addr: "0.0.0.0:7401"
  idle_timeout_secs: 300
  max_connections: 5000

storage:
  data_dir: "/var/lib/rstmdb"
  wal_segment_size_mb: 128
  fsync_policy: every_write

auth:
  required: true
  secrets_file: "/etc/rstmdb/tokens"

tls:
  enabled: true
  cert_path: "/etc/rstmdb/server.pem"
  key_path: "/etc/rstmdb/server-key.pem"

compaction:
  enabled: true
  events_threshold: 100000
  size_threshold_mb: 1000
  min_interval_secs: 300

metrics:
  enabled: true
  bind_addr: "0.0.0.0:9090"

logging:
  level: "info"
  format: "json"
```

### High Throughput

```yaml
storage:
  fsync_policy:
    every_ms: 100
  wal_segment_size_mb: 256

compaction:
  events_threshold: 500000
  size_threshold_mb: 5000
```
