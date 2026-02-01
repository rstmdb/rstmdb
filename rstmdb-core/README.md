# rstmdb-core

State machine engine for [rstmdb](https://github.com/rstmdb/rstmdb) - definitions, transitions, and guard evaluation.

## Overview

`rstmdb-core` is the heart of rstmdb, implementing the state machine engine that manages definitions, instances, transitions, and guard conditions. It provides a robust foundation for building event-sourced applications with explicit state management.

## Features

- **State machine definitions** - Define states, transitions, and events
- **Guard conditions** - JSONPath-based guards for conditional transitions
- **Instance management** - Create and manage state machine instances
- **Event application** - Apply events with automatic state transitions
- **WAL integration** - Durable event storage via `rstmdb-wal`
- **Concurrent access** - Thread-safe operations with `DashMap`

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-core = "0.1"
```

## Usage

```rust
use rstmdb_core::{Engine, StateMachineDefinition, Transition};

// Create an engine
let engine = Engine::new("/path/to/data")?;

// Define a state machine
let definition = StateMachineDefinition {
    name: "order".to_string(),
    states: vec!["pending", "confirmed", "shipped", "delivered"]
        .into_iter().map(String::from).collect(),
    initial_state: "pending".to_string(),
    transitions: vec![
        Transition {
            from: "pending".to_string(),
            to: "confirmed".to_string(),
            event: "confirm".to_string(),
            guard: None,
        },
        Transition {
            from: "confirmed".to_string(),
            to: "shipped".to_string(),
            event: "ship".to_string(),
            guard: Some("$.tracking_number".to_string()),
        },
    ],
};

// Register the state machine
let sm_id = engine.create_state_machine(definition)?;

// Create an instance
let instance_id = engine.create_instance(sm_id, serde_json::json!({
    "order_id": "ORD-123",
    "items": ["item1", "item2"]
}))?;

// Apply events
engine.apply_event(instance_id, "confirm", serde_json::json!({}))?;
engine.apply_event(instance_id, "ship", serde_json::json!({
    "tracking_number": "TRK-456"
}))?;

// Get current state
let instance = engine.get_instance(instance_id)?;
assert_eq!(instance.current_state, "shipped");
```

## Guard Conditions

Guards use JSONPath expressions to validate event payloads:

```rust
Transition {
    from: "pending".to_string(),
    to: "approved".to_string(),
    event: "approve".to_string(),
    // Only allow approval if amount <= 1000
    guard: Some("$.amount <= 1000".to_string()),
}
```

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
