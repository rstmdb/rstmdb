---
sidebar_position: 5
---

# Write-Ahead Log (WAL)

The Write-Ahead Log (WAL) is rstmdb's durability mechanism. All state changes are written to the WAL before being acknowledged, ensuring data survives crashes and restarts.

## How WAL Works

### Write Path

1. **Receive request** - Client sends a write operation (create instance, apply event, etc.)
2. **Write to WAL** - Operation is appended to the current WAL segment
3. **Fsync** - Depending on configuration, data is flushed to disk
4. **Update memory** - In-memory state is updated
5. **Acknowledge** - Response sent to client with WAL offset

```
Client Request
      │
      ▼
┌─────────────┐     ┌─────────────┐
│  Serialize  │────▶│  Append to  │
│   Entry     │     │  WAL File   │
└─────────────┘     └──────┬──────┘
                           │
                           ▼
                    ┌─────────────┐
                    │   fsync()   │
                    │ (if policy) │
                    └──────┬──────┘
                           │
                           ▼
                    ┌─────────────┐
                    │   Update    │
                    │   Memory    │
                    └──────┬──────┘
                           │
                           ▼
                    ┌─────────────┐
                    │  Respond    │
                    │  to Client  │
                    └─────────────┘
```

### Recovery Path

On startup:

1. **Find segments** - Scan WAL directory for segment files
2. **Load snapshot** - If available, load the latest snapshot
3. **Replay WAL** - Replay entries from the snapshot offset
4. **Validate checksums** - Verify CRC32C for each entry
5. **Ready** - Server is ready to accept connections

## Segment Format

### File Structure

WAL files are stored as numbered segments:

```
data/
└── wal/
    ├── 0000000000000001.wal   (64 MiB)
    ├── 0000000000000002.wal   (64 MiB)
    ├── 0000000000000003.wal   (in progress)
    └── ...
```

### Entry Format

Each WAL entry has a 24-byte header:

```
Offset  Size  Field       Description
──────────────────────────────────────────
0       4     magic       "WLOG" (0x574C4F47)
4       1     type        Entry type code
5       1     flags       Reserved flags
6       2     reserved    Must be 0
8       4     length      Payload length (big-endian)
12      4     crc32c      CRC32C checksum of payload
16      8     sequence    Monotonic sequence number
24+     var   payload     JSON-serialized entry data
```

### Entry Types

| Type | Code | Description |
|------|------|-------------|
| `PutMachine` | 1 | Machine definition registration |
| `CreateInstance` | 2 | Instance creation |
| `ApplyEvent` | 3 | Event application / state transition |
| `DeleteInstance` | 4 | Instance soft deletion |
| `Snapshot` | 5 | Snapshot reference marker |
| `Checkpoint` | 6 | Recovery checkpoint |

## Global Offset

Each WAL entry has a globally unique offset encoded as:

```
offset = (segment_id << 40) | offset_in_segment
```

This allows:
- Referencing any entry across segments
- Efficient seeking to a specific entry
- Tracking replication position

## Fsync Policies

The fsync policy controls durability vs. performance:

### `every_write`

```yaml
storage:
  fsync_policy: every_write
```

- **Durability**: Highest - no data loss on crash
- **Performance**: Slowest - one fsync per write
- **Use case**: Financial data, critical state

### `every_n`

```yaml
storage:
  fsync_policy:
    every_n: 100
```

- **Durability**: Up to N writes at risk
- **Performance**: Balanced
- **Use case**: General workloads

### `every_ms`

```yaml
storage:
  fsync_policy:
    every_ms: 100
```

- **Durability**: Up to N ms of writes at risk
- **Performance**: Balanced
- **Use case**: High-throughput workloads

### `never`

```yaml
storage:
  fsync_policy: never
```

- **Durability**: All unsynced data at risk
- **Performance**: Fastest
- **Use case**: Testing, non-critical data

## Reading the WAL

### CLI Access

```bash
# Read last 100 entries
rstmdb-cli wal-read -l 100

# Read from specific offset
rstmdb-cli wal-read --from-offset 12345 -l 50

# Get WAL statistics
rstmdb-cli wal-stats
```

### WAL Statistics

```json
{
  "current_offset": 12345,
  "segment_count": 3,
  "total_size_bytes": 157286400,
  "oldest_segment": 1,
  "newest_segment": 3
}
```

## Compaction

Compaction removes old WAL segments that are no longer needed for recovery.

### How Compaction Works

1. **Create snapshot** - Capture current state of all instances
2. **Mark safe offset** - Record the WAL offset at snapshot time
3. **Delete old segments** - Remove segments entirely before the safe offset

```
Before compaction:
  [Segment 1] [Segment 2] [Segment 3] [Segment 4]
                            ↑
                         snapshot
                         offset

After compaction:
                          [Segment 3] [Segment 4]
                            ↑
                         snapshot
                         offset
```

### Manual Compaction

```bash
rstmdb-cli compact
```

### Automatic Compaction

Configure thresholds in `config.yaml`:

```yaml
compaction:
  enabled: true
  events_threshold: 10000    # Compact after N events
  size_threshold_mb: 100     # Compact when WAL > N MB
  min_interval_secs: 60      # Minimum time between compactions
```

## Snapshots

Snapshots are point-in-time captures of instance state.

### Snapshot Structure

```
data/
└── snapshots/
    ├── index.json           # Snapshot metadata
    └── snap-1705312200.snap # Snapshot data (compressed)
```

### Snapshot Content

```json
{
  "timestamp": "2024-01-15T10:30:00Z",
  "wal_offset": 12345,
  "instance_count": 1000,
  "machines": ["order", "user", "document"]
}
```

### Creating Snapshots

```bash
# Snapshot specific instance
rstmdb-cli snapshot-instance order-001

# Trigger full snapshot (via compaction)
rstmdb-cli compact
```

## Data Integrity

### CRC32C Checksums

Every WAL entry includes a CRC32C checksum of its payload:

- Computed using the Castagnoli polynomial (hardware-accelerated on modern CPUs)
- Validated during recovery
- Corrupted entries are detected and can be skipped

### Handling Corruption

During recovery, if a corrupted entry is detected:

1. Log a warning with the corrupted offset
2. Skip the corrupted entry
3. Continue with next valid entry
4. Report corruption count at startup

### Partial Writes

The WAL handles partial writes (incomplete entries at end of file):

- Detected by incomplete header or mismatched length
- Truncated to last valid entry
- No data loss for previously acknowledged writes

## Best Practices

### Choose Appropriate Fsync Policy

| Workload | Recommended Policy |
|----------|-------------------|
| Financial/Critical | `every_write` |
| General | `every_ms: 100` |
| High throughput | `every_n: 1000` |
| Testing | `never` |

### Size WAL Segments Appropriately

```yaml
storage:
  wal_segment_size_mb: 64  # Default, good for most cases
```

- Smaller segments: Faster compaction, more files
- Larger segments: Fewer files, slower compaction

### Monitor WAL Size

Watch for unbounded WAL growth:

```bash
# Check WAL stats
rstmdb-cli wal-stats

# Or via metrics endpoint
curl http://localhost:9090/metrics | grep wal
```

### Enable Automatic Compaction

For long-running deployments:

```yaml
compaction:
  enabled: true
  events_threshold: 100000
  size_threshold_mb: 1000
```

## Limitations

- **Single writer**: Only one server can write to a WAL directory
- **No real-time replication**: WAL streaming is planned but not yet implemented
- **Memory requirement**: All data must fit in memory; WAL is for durability only
