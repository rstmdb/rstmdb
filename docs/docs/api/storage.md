---
sidebar_position: 7
---

# Storage Commands

Commands for WAL management and compaction.

## WAL_READ

Reads entries from the Write-Ahead Log.

### Request

```json
{
  "op": "WAL_READ",
  "params": {
    "from_offset": 0,
    "limit": 100
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `from_offset` | integer | Yes | Starting offset |
| `limit` | integer | No | Max entries (default: 100, max: 1000) |

### Response

```json
{
  "status": "ok",
  "result": {
    "records": [
      {
        "sequence": 1,
        "offset": 0,
        "entry": {
          "type": "put_machine",
          "machine": "order",
          "version": 1,
          "definition_hash": "a1b2c3...",
          "definition": {...}
        }
      },
      {
        "sequence": 2,
        "offset": 1,
        "entry": {
          "type": "create_instance",
          "instance_id": "order-001",
          "machine": "order",
          "version": 1,
          "initial_state": "pending",
          "initial_ctx": {}
        }
      },
      {
        "sequence": 3,
        "offset": 2,
        "entry": {
          "type": "apply_event",
          "instance_id": "order-001",
          "event": "PAY",
          "from_state": "pending",
          "to_state": "paid",
          "payload": {"amount": 99.99},
          "ctx": {"customer": "alice", "amount": 99.99}
        }
      }
    ],
    "next_offset": 3
  }
}
```

### Record Fields

| Field | Description |
|-------|-------------|
| `sequence` | Monotonically increasing sequence number |
| `offset` | WAL offset |
| `entry` | Entry payload with `type` discriminator |

### Entry Types

| Type | Description |
|------|-------------|
| `put_machine` | Machine definition registration |
| `create_instance` | Instance creation |
| `apply_event` | Event application |
| `delete_instance` | Instance deletion |
| `snapshot` | Snapshot marker |
| `checkpoint` | Recovery checkpoint |

### Pagination

Use `next_offset` for pagination:

```json
// First page
{"op": "WAL_READ", "params": {"from_offset": 0, "limit": 100}}
// Response: next_offset = 100

// Second page
{"op": "WAL_READ", "params": {"from_offset": 100, "limit": 100}}
// Response: next_offset = 200
```

---

## WAL_STATS

Returns WAL statistics.

### Request

```json
{
  "op": "WAL_STATS"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "entry_count": 50000,
    "segment_count": 3,
    "total_size_bytes": 157286400,
    "latest_offset": 49999,
    "io_stats": {
      "bytes_written": 157286400,
      "bytes_read": 52428800,
      "writes": 50000,
      "reads": 10000,
      "fsyncs": 50000
    }
  }
}
```

| Field | Description |
|-------|-------------|
| `entry_count` | Total number of WAL entries |
| `segment_count` | Number of segment files |
| `total_size_bytes` | Total WAL size on disk |
| `latest_offset` | Latest (highest) WAL offset |
| `io_stats` | I/O statistics |

### I/O Statistics

| Field | Description |
|-------|-------------|
| `bytes_written` | Total bytes written to WAL |
| `bytes_read` | Total bytes read from WAL |
| `writes` | Number of write operations |
| `reads` | Number of read operations |
| `fsyncs` | Number of fsync operations |

---

## SNAPSHOT_INSTANCE

Creates a snapshot of a specific instance.

### Request

```json
{
  "op": "SNAPSHOT_INSTANCE",
  "params": {
    "instance_id": "order-001"
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `instance_id` | string | Yes | Instance to snapshot |

### Response

```json
{
  "status": "ok",
  "result": {
    "instance_id": "order-001",
    "snapshot_id": "snap-abc123",
    "wal_offset": 12345,
    "size_bytes": 1024,
    "checksum": "a1b2c3..."
  }
}
```

| Field | Description |
|-------|-------------|
| `instance_id` | Instance that was snapshotted |
| `snapshot_id` | Unique snapshot identifier |
| `wal_offset` | WAL offset at time of snapshot |
| `size_bytes` | Snapshot size (only if snapshot store configured) |
| `checksum` | SHA-256 of snapshot data (only if snapshot store configured) |

### Errors

| Code | Description |
|------|-------------|
| `INSTANCE_NOT_FOUND` | Instance doesn't exist |

---

## COMPACT

Triggers WAL compaction.

### Request

```json
{
  "op": "COMPACT",
  "params": {
    "force_snapshot": false
  }
}
```

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `force_snapshot` | boolean | No | Re-snapshot all instances even if unchanged (default: `false`) |

### Response

```json
{
  "status": "ok",
  "result": {
    "snapshots_created": 5,
    "segments_deleted": 2,
    "bytes_reclaimed": 134217728,
    "total_snapshots": 10,
    "wal_segments": 1
  }
}
```

| Field | Description |
|-------|-------------|
| `snapshots_created` | Number of new snapshots created |
| `segments_deleted` | Number of old WAL segments removed |
| `bytes_reclaimed` | Disk space freed |
| `total_snapshots` | Total snapshots after compaction |
| `wal_segments` | Remaining WAL segment count |

### Compaction Process

1. Create snapshot of all instances (or only changed ones unless `force_snapshot`)
2. Record snapshot offset in WAL
3. Delete segments before snapshot offset
4. Update segment index

```
Before:
  [Seg 1] [Seg 2] [Seg 3] [Seg 4] [Seg 5]
                    ^
                 snapshot

After:
                  [Seg 3'] [Seg 4] [Seg 5]
                    ^
                 snapshot
```

Requires a snapshot store to be configured on the server.

### Automatic Compaction

Configure in server settings:

```yaml
compaction:
  enabled: true
  events_threshold: 10000
  size_threshold_mb: 100
  min_interval_secs: 60
```

---

## Examples

### Export WAL for Backup

```bash
#!/bin/bash
# Export all WAL entries to JSON file

offset=0
while true; do
  result=$(rstmdb-cli wal-read --from-offset $offset --limit 1000 --json)

  records=$(echo "$result" | jq '.records')
  echo "$records" >> wal-backup.json

  next=$(echo "$result" | jq '.next_offset')
  count=$(echo "$records" | jq 'length')

  if [ "$count" -lt 1000 ]; then
    break
  fi

  offset=$next
done
```

### Monitor WAL Growth

```bash
#!/bin/bash
# Alert if WAL exceeds threshold

MAX_SIZE_MB=500

stats=$(rstmdb-cli wal-stats --json)
size_bytes=$(echo "$stats" | jq '.total_size_bytes')
size_mb=$((size_bytes / 1048576))

if [ $size_mb -gt $MAX_SIZE_MB ]; then
  echo "WARNING: WAL size ${size_mb}MB exceeds ${MAX_SIZE_MB}MB"
  # Trigger compaction
  rstmdb-cli compact
fi
```

### Build Event Replay

```javascript
// Rebuild state from WAL
async function replayWAL(client, handler) {
  let offset = 0;

  while (true) {
    const result = await client.walRead({
      from_offset: offset,
      limit: 1000
    });

    for (const record of result.records) {
      await handler(record.entry);
    }

    if (result.records.length < 1000) {
      break;
    }

    offset = result.next_offset;
  }
}

// Example: Count entries by type
const counts = {};
await replayWAL(client, (entry) => {
  counts[entry.type] = (counts[entry.type] || 0) + 1;
});
console.log(counts);
// {put_machine: 5, create_instance: 1000, apply_event: 5000}
```
