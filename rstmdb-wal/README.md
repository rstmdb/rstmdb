# rstmdb-wal

Write-ahead log implementation for [rstmdb](https://github.com/rstmdb/rstmdb) - durable, append-only storage.

## Overview

`rstmdb-wal` provides a high-performance write-ahead log (WAL) implementation that ensures durability for the rstmdb state machine database. It handles append-only writes with configurable fsync policies and CRC32C checksums for data integrity.

## Features

- **Append-only storage** - Efficient sequential writes optimized for durability
- **CRC32C checksums** - Data integrity verification on every record
- **Configurable fsync** - Choose between performance and durability guarantees
- **Segment-based storage** - Automatic segment rotation and management
- **Recovery support** - Replay logs to restore state after crashes

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
rstmdb-wal = "0.1"
```

## Usage

```rust
use rstmdb_wal::{Wal, WalConfig};

// Create a WAL with default configuration
let config = WalConfig::default();
let wal = Wal::open("/path/to/wal", config)?;

// Append entries
let entry = b"state machine event data";
let lsn = wal.append(entry)?;

// Sync to disk
wal.sync()?;

// Read entries for recovery
for entry in wal.iter()? {
    let (lsn, data) = entry?;
    // Process recovered entry
}
```

## Configuration

```rust
use rstmdb_wal::WalConfig;

let config = WalConfig {
    segment_size: 64 * 1024 * 1024,  // 64MB segments
    sync_on_append: false,            // Batch syncs for performance
    ..Default::default()
};
```

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
