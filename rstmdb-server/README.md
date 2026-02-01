# rstmdb-server

TCP server for [rstmdb](https://github.com/rstmdb/rstmdb) - connection handling, command dispatch, and sessions.

## Overview

`rstmdb-server` implements the TCP server that handles client connections, authentication, and command processing. It provides TLS/mTLS support, connection pooling, and Prometheus metrics for observability.

## Features

- **TCP server** - High-performance async server built on Tokio
- **TLS/mTLS support** - Secure connections with optional client certificates
- **Authentication** - Username/password and token-based auth
- **Session management** - Per-connection session state
- **Prometheus metrics** - Built-in `/metrics` endpoint
- **Graceful shutdown** - Clean connection draining on SIGTERM

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-server = "0.1"
```

## Usage

```rust
use rstmdb_server::{Server, ServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig {
        bind_address: "0.0.0.0:7401".parse()?,
        data_dir: "/var/lib/rstmdb".into(),
        ..Default::default()
    };

    let server = Server::new(config)?;
    server.run().await?;

    Ok(())
}
```

## Configuration

```rust
use rstmdb_server::ServerConfig;

let config = ServerConfig {
    bind_address: "0.0.0.0:7401".parse()?,
    metrics_address: Some("0.0.0.0:9090".parse()?),
    data_dir: "/var/lib/rstmdb".into(),

    // TLS configuration
    tls_cert_path: Some("/path/to/cert.pem".into()),
    tls_key_path: Some("/path/to/key.pem".into()),
    tls_ca_path: Some("/path/to/ca.pem".into()),  // For mTLS

    // Authentication
    auth_enabled: true,
    users: vec![("admin".into(), "hashed_password".into())],

    // Limits
    max_connections: 1000,
    idle_timeout: Duration::from_secs(300),

    ..Default::default()
};
```

## Configuration File (YAML)

```yaml
bind_address: "0.0.0.0:7401"
metrics_address: "0.0.0.0:9090"
data_dir: /var/lib/rstmdb

tls:
  cert: /etc/rstmdb/tls/server.crt
  key: /etc/rstmdb/tls/server.key
  ca: /etc/rstmdb/tls/ca.crt  # Optional, for mTLS

auth:
  enabled: true
  users:
    - username: admin
      password_hash: "$argon2id$..."

limits:
  max_connections: 1000
  idle_timeout_secs: 300
```

## Prometheus Metrics

The server exposes metrics at the configured metrics address:

- `rstmdb_connections_total` - Total connections accepted
- `rstmdb_connections_active` - Current active connections
- `rstmdb_commands_total` - Commands processed by type
- `rstmdb_command_duration_seconds` - Command latency histogram
- `rstmdb_events_applied_total` - Total events applied
- `rstmdb_instances_total` - Total instances by state machine

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
