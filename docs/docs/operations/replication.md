---
sidebar_position: 6
---

# Replication

rstmdb supports **primary → replica WAL streaming replication**: the primary streams every write-ahead log entry to one or more read-only replicas over TCP (plain or TLS), so replicas converge on the same state as the primary.

Replication is:

- **Asynchronous by default** — writes on the primary return as soon as the WAL is durable, and replicas apply in the background.
- **Synchronous on demand** — opt in with `mode: sync` to have writes wait for N replica ACKs before returning.
- **Self-healing** — replicas reconnect with exponential backoff and resume from their last applied offset.
- **Observable** — per-replica lag, connected count, throughput, and slow-replica events are exposed as Prometheus metrics.

## Topology

```
                    writes
          ┌──────────────────────┐
          │                      │
    ┌─────▼──────┐        ┌──────┴──────┐
    │  Clients   │        │   Primary   │
    └─────┬──────┘        │  (:7401)    │
          │ reads         └──────┬──────┘
          │                      │  WAL stream
          │                      │  (TCP / TLS)
          ▼                      ├────────────┐
    ┌─────────────┐        ┌─────▼────┐  ┌────▼─────┐
    │  Replica 1  │◀───────│ Replica 1│  │Replica 2 │
    │   (:7402)   │ reads  │ (:7402)  │  │ (:7403)  │
    └─────────────┘        └──────────┘  └──────────┘
```

- Exactly one **primary** accepts writes.
- Any number of **replicas** connect to the primary, catch up from their last known WAL offset, then live-stream new entries.
- Clients can connect to replicas for reads — write commands return `READ_ONLY_MODE`.

## How It Works

1. **WAL tailing** — the primary runs a background task that polls its WAL for new entries and fans them out to every connected replica over a per-replica bounded channel.
2. **Catch-up on connect** — when a replica connects, it sends its last applied primary WAL offset in the handshake. The primary replays any entries the replica doesn't yet have, then hands off to the live stream.
3. **ACK flow** — the replica applies each entry to its own WAL and state store, then sends an ACK back. The primary tracks per-replica last-acked sequence for sync mode and lag metrics.
4. **Backpressure** — if a replica can't keep up and its bounded channel fills, the primary disconnects it and closes the TCP socket. The replica detects the EOF, reconnects, and catches up from its last ACKed offset. No entries are silently dropped.
5. **Heartbeats** — the primary sends periodic heartbeats so replicas can detect stalls and compute time-based lag even when no writes are happening.

Replication is offset-based, not sequence-based: WAL offsets are strictly monotonic on disk while sequences are not (concurrent writes race for sequence numbers before hitting disk). Catch-up and live streaming both filter by offset so no entries are skipped under concurrent load.

## Read-Only Replicas

A server with `role: replica` rejects all write commands with `READ_ONLY_MODE`:

| Read-allowed commands                   | Write-rejected commands                                                 |
| --------------------------------------- | ----------------------------------------------------------------------- |
| `GetMachine`, `ListMachines`            | `PutMachine`                                                            |
| `GetInstance`, `ListInstances`          | `CreateInstance`, `ApplyEvent`, `DeleteInstance`                        |
| `Subscribe`, `WatchInstance`            | `Batch` (if it contains any write)                                      |
| `Hello`, `Auth`, `Ping`, `Bye`          | `FlushAll`, `Compact`                                                   |

Subscriptions work on replicas: they observe events as they are applied from the primary's WAL stream.

## Configuration

Replication is configured under a top-level `replication:` block in the server's YAML config. All fields can also be overridden via `RSTMDB_REPL_*` environment variables.

### Primary

```yaml
# config.primary.yaml
network:
  bind_addr: "0.0.0.0:7401"

storage:
  data_dir: "./data/primary"

replication:
  role: primary
  mode: async                # async (default) or sync

  # Preferred: hashed tokens. Replicas authenticate with the plaintext token;
  # the primary compares sha256(token) against this list.
  # Generate with: rstmdb-cli hash-token <token>
  auth_token_hashes:
    - "ec46c1b607eb52e1db74a115a1ec14a872b19f47909a5ce1d57a651d7d7b116c"

  # How often the WAL tailer polls for new entries.
  poll_interval_ms: 10
  heartbeat_interval_secs: 5
  lag_check_interval_secs: 10

  # Sync-mode knobs (only used when mode: sync).
  sync_replicas: 1
  sync_timeout_ms: 5000
```

### Replica

```yaml
# config.replica1.yaml
network:
  bind_addr: "0.0.0.0:7402"

storage:
  data_dir: "./data/replica1"

replication:
  role: replica
  upstream: "primary.internal:7401"

  # Replicas send this plaintext token; the primary hashes and compares.
  auth_token: "repl-dev-token"

  # Exponential-backoff reconnect window (jittered).
  reconnect_delay_secs: 1
  reconnect_max_delay_secs: 60

  # Lag thresholds for alerting.
  max_lag_entries: 10000
  max_lag_seconds: 30
  lag_check_interval_secs: 10
```

### Full Reference

| Field                        | Applies to | Default    | Description                                                          |
| ---------------------------- | ---------- | ---------- | -------------------------------------------------------------------- |
| `role`                       | both       | standalone | `standalone`, `primary`, or `replica`                                |
| `mode`                       | primary    | async      | `async` or `sync`                                                    |
| `upstream`                   | replica    | —          | `host:port` of the primary                                           |
| `sync_replicas`              | primary    | 1          | Min ACKs before a sync write returns                                 |
| `sync_timeout_ms`            | primary    | 5000       | Sync ACK wait timeout                                                |
| `max_lag_entries`            | both       | 10000      | Alerting threshold (entries behind)                                  |
| `max_lag_seconds`            | both       | 30         | Alerting threshold (seconds behind)                                  |
| `auth_token`                 | replica    | —          | Plaintext token the replica presents                                 |
| `auth_token_hashes`          | primary    | `[]`       | SHA-256 hex hashes of accepted tokens (rotation-friendly)            |
| `auth_secrets_file`          | primary    | —          | Path to a file of token hashes (one per line, `#` comments)          |
| `poll_interval_ms`           | primary    | 10         | WAL tailer poll interval                                             |
| `heartbeat_interval_secs`    | primary    | 5          | How often heartbeats are sent to replicas                            |
| `reconnect_delay_secs`       | replica    | 1          | Base delay for exponential-backoff reconnect                         |
| `reconnect_max_delay_secs`   | replica    | 60         | Cap for reconnect backoff                                            |
| `lag_check_interval_secs`    | both       | 10         | How often lag is measured and logged                                 |
| `tls_enabled`                | replica    | false      | Connect to primary over TLS                                          |
| `tls_ca_path`                | replica    | —          | Custom CA for verifying the primary's cert                           |
| `tls_insecure`               | replica    | false      | Skip TLS verification (development only)                             |

### Environment Variables

| Variable                              | Config path                             |
| ------------------------------------- | --------------------------------------- |
| `RSTMDB_REPL_ROLE`                    | `replication.role`                      |
| `RSTMDB_REPL_MODE`                    | `replication.mode`                      |
| `RSTMDB_REPL_UPSTREAM`                | `replication.upstream`                  |
| `RSTMDB_REPL_SYNC_REPLICAS`           | `replication.sync_replicas`             |
| `RSTMDB_REPL_SYNC_TIMEOUT_MS`         | `replication.sync_timeout_ms`           |
| `RSTMDB_REPL_AUTH_TOKEN`              | `replication.auth_token`                |
| `RSTMDB_REPL_AUTH_TOKEN_HASH`         | appends to `replication.auth_token_hashes` |
| `RSTMDB_REPL_AUTH_SECRETS_FILE`       | `replication.auth_secrets_file`         |
| `RSTMDB_REPL_POLL_INTERVAL_MS`        | `replication.poll_interval_ms`          |
| `RSTMDB_REPL_HEARTBEAT_INTERVAL_SECS` | `replication.heartbeat_interval_secs`   |
| `RSTMDB_REPL_RECONNECT_DELAY_SECS`    | `replication.reconnect_delay_secs`      |
| `RSTMDB_REPL_RECONNECT_MAX_DELAY_SECS`| `replication.reconnect_max_delay_secs`  |
| `RSTMDB_REPL_LAG_CHECK_INTERVAL_SECS` | `replication.lag_check_interval_secs`   |
| `RSTMDB_REPL_TLS_ENABLED`             | `replication.tls_enabled`               |
| `RSTMDB_REPL_TLS_CA`                  | `replication.tls_ca_path`               |
| `RSTMDB_REPL_TLS_INSECURE`            | `replication.tls_insecure`              |

## Async vs Sync Mode

### Async (default)

Primary writes return as soon as the entry is durable in the local WAL. Replicas apply in the background. This is the cheap path — latency is unaffected by replica health.

Use async when you want horizontal read scale-out and eventual-consistency reads are acceptable.

### Sync

Writes block until at least `sync_replicas` replicas have ACKed the new entry. If the timeout elapses first, the write fails with a `SYNC_TIMEOUT` error.

```yaml
replication:
  role: primary
  mode: sync
  sync_replicas: 1
  sync_timeout_ms: 5000
```

Use sync when you need stronger durability than single-node WAL fsync (e.g. losing the primary's disk still lets you recover from a replica) and you can tolerate the added write latency.

## Authentication

Prefer **hashed tokens** on the primary:

```bash
rstmdb-cli hash-token my-replica-token
# ec46c1b607eb52e1db74a115a1ec14a872b19f47909a5ce1d57a651d7d7b116c
```

Primary:

```yaml
replication:
  role: primary
  auth_token_hashes:
    - "ec46c1b607eb52e1db74a115a1ec14a872b19f47909a5ce1d57a651d7d7b116c"
    # additional hashes allowed — good for rotation
```

Replica:

```yaml
replication:
  role: replica
  auth_token: "my-replica-token"
```

For rotation without restarts, maintain a secrets file:

```yaml
replication:
  role: primary
  auth_secrets_file: "/etc/rstmdb/replication-tokens"
```

One hash per line, `#` for comments.

:::warning
`replication.auth_token` on the primary is accepted for backwards compatibility: it is hashed at load time and appended to `auth_token_hashes`. Prefer the hashes form so plaintext secrets never live in config files.
:::

## TLS

To encrypt the primary ↔ replica connection, enable TLS on the primary (same `tls:` block used for client connections) and tell each replica to use TLS when dialing:

```yaml
# replica
replication:
  role: replica
  upstream: "primary.internal:7401"
  tls_enabled: true
  tls_ca_path: "/etc/rstmdb/ca.pem"   # optional — defaults to system roots
  # tls_insecure: true                 # dev only; skips verification
```

The primary uses the same certificate for replication as it does for client connections.

## Backpressure & Slow Replicas

Every replica has a bounded in-memory fan-out channel on the primary side (4096 entries). If the channel ever fills — meaning the replica can't drain fast enough — the primary:

1. Drops the replica from the live-stream fan-out.
2. Increments `rstmdb_replication_slow_replica_disconnects_total`.
3. Cleanly shuts down its end of the TCP socket.

The replica observes EOF on its read, enters its reconnect loop with exponential backoff, and catches up from its last-acked offset.

This design trades live-stream continuity for correctness: a slow replica never causes the primary to stall or silently skip entries. If you see frequent slow-replica disconnects, investigate:

- Replica hardware (disk IOPS, CPU)
- `storage.fsync_policy` on the replica — `every_write` is slowest
- Network bandwidth between nodes

## Catch-up

When a replica connects (fresh or after disconnect) it sends the highest primary WAL offset it has already applied. The primary:

- Reads the WAL from that offset forward.
- Streams each missing entry over the same connection used for live fan-out.
- Hands off to the live tailer once catch-up completes.

The replica deduplicates using `last_applied_primary_offset` so overlap between catch-up and live streaming is safe: duplicate entries are ACKed but skipped.

:::note
Catching up a replica from an empty WAL involves sending the entire primary WAL over the network. For large WALs, consider seeding the replica's `storage.data_dir` from a recent primary snapshot + WAL copy, then starting replication. Once connected, only the delta streams.
:::

## Observability

### Metrics

| Metric                                                             | Type     | Emitted by |
| ------------------------------------------------------------------ | -------- | ---------- |
| `rstmdb_replication_connected_replicas`                            | Gauge    | primary    |
| `rstmdb_replication_entries_sent_total`                            | Counter  | primary    |
| `rstmdb_replication_slow_replica_disconnects_total`                | Counter  | primary    |
| `rstmdb_replication_sync_timeouts_total`                           | Counter  | primary    |
| `rstmdb_replication_replica_lag_entries{replica_id}`               | GaugeVec | primary    |
| `rstmdb_replication_replica_last_acked_sequence{replica_id}`       | GaugeVec | primary    |
| `rstmdb_replication_lag_entries`                                   | Gauge    | replica    |
| `rstmdb_replication_lag_seconds`                                   | Gauge    | replica    |

Useful alerts:

```promql
# Any replica more than 30s behind
rstmdb_replication_lag_seconds > 30

# Primary lost all replicas
rstmdb_replication_connected_replicas == 0

# Slow-replica thrash — disconnects per minute
rate(rstmdb_replication_slow_replica_disconnects_total[1m]) > 0
```

### Logs

The primary logs each replica connection, catch-up completion, slow-replica disconnect, and periodic lag summary. The replica logs every reconnect and the catch-up range on each connection.

Pair this with the bundled Grafana dashboard (`grafana/dashboards/rstmdb.json`) for per-role views.

## Operational Checklist

- Use **async mode** for read scale-out; reach for **sync** only when you need cross-node durability for each write.
- Run **at least two replicas** in production so losing one doesn't leave you unreplicated.
- Put replicas on **separate failure domains** (AZ / rack / host) from the primary.
- Use **hashed tokens** (`auth_token_hashes`) and rotate through `auth_secrets_file` without restarts.
- Enable **TLS** for cross-network replication. The same cert you use for client traffic works.
- **Alert on lag** (`rstmdb_replication_lag_seconds`, `rstmdb_replication_replica_lag_entries`) and on slow-replica disconnects.
- Give replicas the **same or better I/O** than the primary. A replica slower than the primary will always lag.
- Replica `data_dir` should never be shared with the primary — each node needs its own WAL.

## Limitations

- **Single primary** — no automatic failover or leader election (planned via Raft in a future release).
- **Manual promotion** — promoting a replica to primary is operator-driven: stop writes, pick the most-caught-up replica, update its config to `role: primary`, and repoint clients and other replicas.
- **No partial replication** — every replica mirrors the entire dataset; no per-machine sharding.

## See Also

- [Configuration](../configuration) — full configuration reference
- [Monitoring](./monitoring) — Prometheus metrics, Grafana dashboards
- [Security](./security) — authentication and TLS setup
- [Backup & Recovery](./backup-recovery) — snapshot-based seeding for new replicas
