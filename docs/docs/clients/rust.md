---
sidebar_position: 2
---

# Rust Client

The official Rust client library for rstmdb.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-client = "0.1"
tokio = { version = "1", features = ["full"] }
```

## Quick Start

```rust
use rstmdb_client::{Client, Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to server
    let client = Client::connect("127.0.0.1:7401").await?;

    // Ping
    client.ping().await?;
    println!("Connected!");

    // Create a machine
    client.put_machine("order", 1, r#"{
        "states": ["pending", "paid", "shipped"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "SHIP", "to": "shipped"}
        ]
    }"#).await?;

    // Create an instance
    let instance = client.create_instance("order", 1, "order-001", json!({
        "customer": "alice",
        "total": 99.99
    })).await?;
    println!("Created: {:?}", instance);

    // Apply event
    let result = client.apply_event("order-001", "PAY", json!({
        "payment_id": "pay-123"
    })).await?;
    println!("State: {} -> {}", result.previous_state, result.current_state);

    Ok(())
}
```

## Configuration

### Basic Configuration

```rust
use rstmdb_client::{Client, Config};

let config = Config::new("127.0.0.1:7401")
    .with_token("my-secret-token");

let client = Client::with_config(config).await?;
```

### TLS Configuration

```rust
use rstmdb_client::{Client, Config, TlsConfig};

let tls = TlsConfig::new()
    .with_ca_cert("/path/to/ca.pem")
    .with_client_cert("/path/to/client.pem", "/path/to/client-key.pem");

let config = Config::new("secure.example.com:7401")
    .with_token("my-secret-token")
    .with_tls(tls);

let client = Client::with_config(config).await?;
```

### Connection Pool

```rust
use rstmdb_client::{Client, Config, PoolConfig};

let pool_config = PoolConfig::new()
    .min_connections(5)
    .max_connections(20)
    .idle_timeout(Duration::from_secs(300));

let config = Config::new("127.0.0.1:7401")
    .with_pool(pool_config);

let client = Client::with_config(config).await?;
```

## API Reference

### Session Operations

```rust
// Ping
client.ping().await?;

// Server info
let info = client.info().await?;
println!("Version: {}", info.version);
println!("Instances: {}", info.stats.instances);
```

### Machine Operations

```rust
use serde_json::json;

// Put machine definition
client.put_machine("order", 1, r#"{
    "states": ["pending", "paid"],
    "initial": "pending",
    "transitions": [
        {"from": "pending", "event": "PAY", "to": "paid"}
    ]
}"#).await?;

// Get machine
let machine = client.get_machine("order", Some(1)).await?;
println!("States: {:?}", machine.definition.states);

// List machines
let machines = client.list_machines().await?;
for m in machines.machines {
    println!("{}: {} versions", m.name, m.versions.len());
}
```

### Instance Operations

```rust
use serde_json::json;

// Create instance
let instance = client.create_instance(
    "order",           // machine name
    1,                 // version
    "order-001",       // instance id
    json!({            // initial context
        "customer": "alice"
    })
).await?;

// Create with options
let instance = client.create_instance_with_options(
    "order", 1, "order-002",
    json!({"customer": "bob"}),
    CreateOptions::new()
        .idempotency_key("create-order-002")
).await?;

// Get instance
let instance = client.get_instance("order-001").await?;
println!("State: {}", instance.state);
println!("Context: {:?}", instance.context);

// List instances
let instances = client.list_instances()
    .machine("order")
    .state("pending")
    .limit(100)
    .execute()
    .await?;

// Delete instance
client.delete_instance("order-001").await?;
```

### Event Operations

```rust
use serde_json::json;

// Apply event
let result = client.apply_event(
    "order-001",       // instance id
    "PAY",             // event name
    json!({            // payload
        "amount": 99.99
    })
).await?;

println!("Transition: {} -> {}", result.previous_state, result.current_state);

// Apply with options
let result = client.apply_event_with_options(
    "order-001",
    "SHIP",
    json!({}),
    ApplyOptions::new()
        .idempotency_key("ship-order-001")
        .expected_state("paid")
).await?;

// Batch operations
let results = client.batch()
    .mode(BatchMode::Atomic)
    .apply_event("order-001", "PAY", json!({}))
    .apply_event("order-002", "PAY", json!({}))
    .execute()
    .await?;
```

### Subscriptions

```rust
// Watch single instance
let mut stream = client.watch_instance("order-001").await?;
while let Some(event) = stream.next().await {
    let event = event?;
    println!("Event: {} -> {}", event.from_state, event.to_state);
}

// Watch all with filters
let mut stream = client.watch_all()
    .machines(vec!["order"])
    .to_states(vec!["shipped", "delivered"])
    .from_offset(0)  // Replay from beginning
    .execute()
    .await?;

while let Some(event) = stream.next().await {
    let event = event?;
    println!("{}: {} -> {}", event.instance_id, event.event, event.to_state);
}

// Unwatch
stream.unsubscribe().await?;
```

### Storage Operations

```rust
// Read WAL
let entries = client.wal_read()
    .from_offset(0)
    .limit(100)
    .execute()
    .await?;

for entry in entries.entries {
    println!("Offset {}: {:?}", entry.offset, entry.entry_type);
}

// WAL stats
let stats = client.wal_stats().await?;
println!("WAL size: {} bytes", stats.total_size_bytes);

// Compact
client.compact().await?;
```

## Error Handling

```rust
use rstmdb_client::{Error, ErrorCode};

match client.apply_event("order-001", "PAY", json!({})).await {
    Ok(result) => println!("Success: {}", result.current_state),
    Err(Error::Api { code, message, .. }) => {
        match code {
            ErrorCode::InstanceNotFound => {
                println!("Instance doesn't exist");
            }
            ErrorCode::InvalidTransition => {
                println!("Cannot apply PAY from current state");
            }
            ErrorCode::GuardFailed => {
                println!("Guard condition not met");
            }
            _ => {
                println!("Error: {}", message);
            }
        }
    }
    Err(e) => {
        println!("Connection error: {}", e);
    }
}
```

### Retry with Backoff

```rust
use tokio::time::{sleep, Duration};

async fn apply_with_retry(
    client: &Client,
    instance_id: &str,
    event: &str,
    payload: Value,
    max_retries: u32,
) -> Result<ApplyResult, Error> {
    let mut retries = 0;

    loop {
        match client.apply_event(instance_id, event, payload.clone()).await {
            Ok(result) => return Ok(result),
            Err(e) if e.is_retryable() && retries < max_retries => {
                retries += 1;
                let delay = Duration::from_millis(100 * 2_u64.pow(retries));
                sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}
```

## Examples

### Order Processing

```rust
use rstmdb_client::{Client, Config};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect("127.0.0.1:7401").await?;

    // Create order
    let order_id = format!("order-{}", uuid::Uuid::new_v4());

    client.create_instance("order", 1, &order_id, json!({
        "customer_id": "cust-123",
        "items": ["item-1", "item-2"],
        "total": 149.99
    })).await?;

    // Process order
    client.apply_event(&order_id, "PAY", json!({
        "payment_id": "pay-abc",
        "paid_at": chrono::Utc::now().to_rfc3339()
    })).await?;

    client.apply_event(&order_id, "SHIP", json!({
        "tracking_number": "1Z999AA10123456784",
        "carrier": "UPS"
    })).await?;

    // Check final state
    let order = client.get_instance(&order_id).await?;
    println!("Order {} is now: {}", order_id, order.state);

    Ok(())
}
```

### Event Consumer

```rust
use rstmdb_client::Client;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect("127.0.0.1:7401").await?;

    // Subscribe to shipped orders
    let mut stream = client.watch_all()
        .machines(vec!["order"])
        .to_states(vec!["shipped"])
        .execute()
        .await?;

    println!("Listening for shipped orders...");

    while let Some(event) = stream.next().await {
        let event = event?;
        println!(
            "Order {} shipped! Tracking: {:?}",
            event.instance_id,
            event.context.get("tracking_number")
        );

        // Send notification, update external system, etc.
    }

    Ok(())
}
```
