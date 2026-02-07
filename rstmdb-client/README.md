# rstmdb-client

Client library for [rstmdb](https://github.com/rstmdb/rstmdb) - connection pooling and async API.

## Overview

`rstmdb-client` provides an async Rust client for connecting to rstmdb servers. It includes connection pooling, automatic reconnection, TLS support, and a typed API for all rstmdb operations.

## Features

- **Async API** - Built on Tokio for non-blocking operations
- **Connection pooling** - Efficient connection reuse
- **Auto-reconnect** - Automatic reconnection on failures
- **TLS support** - Secure connections with certificate verification
- **Typed responses** - Strongly-typed API for all commands
- **Streaming** - Support for streaming large result sets

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-client = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Usage

```rust
use rstmdb_client::{Client, ClientConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to server
    let client = Client::connect("localhost:7401").await?;

    // Authenticate
    client.auth("admin", "password").await?;

    // Create a state machine
    let sm_id = client.create_state_machine("order", serde_json::json!({
        "states": ["pending", "confirmed", "shipped"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "to": "confirmed", "event": "confirm"},
            {"from": "confirmed", "to": "shipped", "event": "ship"}
        ]
    })).await?;

    // Create an instance
    let instance_id = client.create_instance(sm_id, serde_json::json!({
        "order_id": "ORD-123"
    })).await?;

    // Apply an event
    client.apply_event(instance_id, "confirm", serde_json::json!({})).await?;

    // Get instance state
    let instance = client.get_instance(instance_id).await?;
    println!("Current state: {}", instance.current_state);

    // List instances with filtering
    let result = client.list_instances(
        Some("order"),  // filter by machine
        Some("paid"),   // filter by state
        Some(50),       // limit
        None            // offset
    ).await?;
    println!("Found {} instances", result.total);

    // Get WAL statistics
    let stats = client.wal_stats().await?;
    println!("WAL entries: {}", stats["entry_count"]);

    Ok(())
}
```

## Configuration

```rust
use rstmdb_client::{Client, ClientConfig};
use std::time::Duration;

let config = ClientConfig {
    address: "localhost:7401".to_string(),

    // Connection pool
    pool_size: 10,
    connect_timeout: Duration::from_secs(5),

    // TLS (optional)
    tls_enabled: true,
    tls_ca_cert: Some("/path/to/ca.pem".into()),

    // Authentication
    username: Some("admin".into()),
    password: Some("password".into()),

    ..Default::default()
};

let client = Client::with_config(config).await?;
```

## Connection Pooling

```rust
use rstmdb_client::Pool;

// Create a connection pool
let pool = Pool::builder()
    .max_size(20)
    .min_idle(5)
    .build("localhost:7401")
    .await?;

// Get a connection from the pool
let conn = pool.get().await?;

// Connection is returned to pool when dropped
```

## Error Handling

```rust
use rstmdb_client::{Client, Error};

match client.apply_event(instance_id, "invalid_event", payload).await {
    Ok(instance) => println!("New state: {}", instance.current_state),
    Err(Error::InvalidTransition { from, event }) => {
        println!("Cannot apply '{}' from state '{}'", event, from);
    }
    Err(Error::GuardFailed { guard }) => {
        println!("Guard condition failed: {}", guard);
    }
    Err(e) => return Err(e.into()),
}
```

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
