# rstmdb-storage

Storage layer for [rstmdb](https://github.com/rstmdb/rstmdb) - snapshots, instance persistence, and indexes.

## Overview

`rstmdb-storage` provides the persistence layer for rstmdb, managing snapshots, instance state, and indexes. It builds on top of `rstmdb-wal` and `rstmdb-core` to provide efficient storage and retrieval of state machine data.

## Features

- **Snapshots** - Point-in-time snapshots for faster recovery
- **Instance persistence** - Durable storage of state machine instances
- **Index management** - Efficient lookups by state, metadata, and time
- **Compaction** - Automatic cleanup of old WAL segments after snapshots
- **CRC32C verification** - Data integrity checks on all stored data

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-storage = "0.1"
```

## Usage

```rust
use rstmdb_storage::{Storage, StorageConfig};

// Open storage with configuration
let config = StorageConfig {
    data_dir: "/path/to/data".into(),
    snapshot_interval: 10000,  // Snapshot every 10k events
    ..Default::default()
};

let storage = Storage::open(config)?;

// Store instance state
storage.put_instance(&instance)?;

// Retrieve instance
let instance = storage.get_instance(instance_id)?;

// Create a snapshot
storage.create_snapshot()?;

// List instances by state
let pending = storage.list_by_state("order", "pending")?;
```

## Configuration

```rust
use rstmdb_storage::StorageConfig;

let config = StorageConfig {
    data_dir: "/var/lib/rstmdb".into(),
    snapshot_interval: 10000,       // Events between snapshots
    max_wal_segments: 10,           // Keep 10 WAL segments
    compaction_enabled: true,       // Auto-compact old segments
    ..Default::default()
};
```

## Storage Layout

```
/data
  /wal           # Write-ahead log segments
  /snapshots     # Point-in-time snapshots
  /indexes       # Secondary indexes
```

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
