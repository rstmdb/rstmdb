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
| `from_offset` | integer | No | Starting offset (default: 0) |
| `limit` | integer | No | Max entries (default: 100, max: 1000) |

### Response

```json
{
  "status": "ok",
  "result": {
    "entries": [
      {
        "offset": 1,
        "type": "PutMachine",
        "timestamp": "2024-01-15T10:00:00Z",
        "data": {
          "name": "order",
          "version": 1,
          "definition": {...}
        }
      },
      {
        "offset": 2,
        "type": "CreateInstance",
        "timestamp": "2024-01-15T10:01:00Z",
        "data": {
          "id": "order-001",
          "machine": "order",
          "version": 1,
          "context": {}
        }
      },
      {
        "offset": 3,
        "type": "ApplyEvent",
        "timestamp": "2024-01-15T10:02:00Z",
        "data": {
          "instance_id": "order-001",
          "event": "PAY",
          "from_state": "pending",
          "to_state": "paid",
          "payload": {"amount": 99.99}
        }
      }
    ],
    "next_offset": 101,
    "has_more": true
  }
}
```

### Entry Types

| Type | Description |
|------|-------------|
| `PutMachine` | Machine definition registration |
| `CreateInstance` | Instance creation |
| `ApplyEvent` | Event application |
| `DeleteInstance` | Instance deletion |
| `Snapshot` | Snapshot marker |
| `Checkpoint` | Recovery checkpoint |

### Pagination

Use `next_offset` for pagination:

```json
// First page
{"op": "WAL_READ", "params": {"limit": 100}}
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
    "current_offset": 50000,
    "first_offset": 1000,
    "segment_count": 3,
    "total_size_bytes": 157286400,
    "segments": [
      {
        "id": 1,
        "first_offset": 1000,
        "last_offset": 20000,
        "size_bytes": 67108864
      },
      {
        "id": 2,
        "first_offset": 20001,
        "last_offset": 40000,
        "size_bytes": 67108864
      },
      {
        "id": 3,
        "first_offset": 40001,
        "last_offset": 50000,
        "size_bytes": 23068672
      }
    ]
  }
}
```

| Field | Description |
|-------|-------------|
| `current_offset` | Latest WAL offset |
| `first_offset` | Oldest available offset |
| `segment_count` | Number of segment files |
| `total_size_bytes` | Total WAL size |
| `segments` | Per-segment details |

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
    "snapshot_id": "snap-abc123",
    "instance_id": "order-001",
    "wal_offset": 12345,
    "state": "paid",
    "created_at": "2024-01-15T10:30:00Z"
  }
}
```

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
  "op": "COMPACT"
}
```

### Response

```json
{
  "status": "ok",
  "result": {
    "started": true,
    "previous_size_bytes": 314572800,
    "previous_segment_count": 5,
    "compaction_id": "compact-xyz789"
  }
}
```

### Compaction Process

1. Create snapshot of all instances
2. Record snapshot offset in WAL
3. Delete segments before snapshot offset
4. Update segment index

```
Before:
  [Seg 1] [Seg 2] [Seg 3] [Seg 4] [Seg 5]
                    ↑
                 snapshot

After:
                  [Seg 3'] [Seg 4] [Seg 5]
                    ↑
                 snapshot
```

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

  entries=$(echo "$result" | jq '.entries')
  echo "$entries" >> wal-backup.json

  has_more=$(echo "$result" | jq '.has_more')
  if [ "$has_more" = "false" ]; then
    break
  fi

  offset=$(echo "$result" | jq '.next_offset')
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

    for (const entry of result.entries) {
      await handler(entry);
    }

    if (!result.has_more) {
      break;
    }

    offset = result.next_offset;
  }
}

// Example: Count events by type
const counts = {};
await replayWAL(client, (entry) => {
  counts[entry.type] = (counts[entry.type] || 0) + 1;
});
console.log(counts);
// {PutMachine: 5, CreateInstance: 1000, ApplyEvent: 5000}
```
