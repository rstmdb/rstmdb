# rstmdb-protocol

Wire protocol implementation for [rstmdb](https://github.com/rstmdb/rstmdb) - framing, messages, and serialization.

## Overview

`rstmdb-protocol` defines the RCP (rstmdb Command Protocol) wire format used for client-server communication. It handles message framing, serialization/deserialization, and protocol versioning.

## Features

- **Length-prefixed framing** - Reliable message boundaries over TCP streams
- **JSON serialization** - Human-readable message format for debugging
- **CRC32C checksums** - Message integrity verification
- **Request/Response types** - Typed messages for all rstmdb operations
- **UUID-based identifiers** - Unique IDs for state machines, instances, and requests

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-protocol = "0.1"
```

## Usage

```rust
use rstmdb_protocol::{Request, Response, Frame};

// Create a request
let request = Request::CreateStateMachine {
    name: "order".to_string(),
    definition: serde_json::json!({
        "states": ["pending", "confirmed", "shipped"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "to": "confirmed", "event": "confirm"},
            {"from": "confirmed", "to": "shipped", "event": "ship"}
        ]
    }),
};

// Serialize to wire format
let frame = Frame::from_request(&request)?;
let bytes = frame.encode();

// Parse response
let response_frame = Frame::decode(&received_bytes)?;
let response: Response = response_frame.into_response()?;
```

## Protocol Commands

The protocol supports the following command types:

- `HELLO` / `PING` / `BYE` - Session management
- `AUTH` - Authentication with bearer token
- `INFO` - Server capabilities and limits
- `PUT_MACHINE` / `GET_MACHINE` / `LIST_MACHINES` - Machine definition management
- `CREATE_INSTANCE` / `GET_INSTANCE` / `LIST_INSTANCES` / `DELETE_INSTANCE` - Instance lifecycle
- `APPLY_EVENT` / `BATCH` - Event operations
- `SNAPSHOT_INSTANCE` / `WAL_READ` / `WAL_STATS` / `COMPACT` - Storage & WAL
- `WATCH_INSTANCE` / `WATCH_ALL` / `UNWATCH` - Streaming subscriptions

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
