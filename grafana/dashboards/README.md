# rstmdb Grafana Dashboard

Comprehensive monitoring dashboard for [rstmdb](https://github.com/rstmdb/rstmdb) - a distributed state machine database.

## Panels

### Requests
- **Request Rate by Operation** - Requests per second by operation type (PUT_MACHINE, CREATE_INSTANCE, APPLY_EVENT, etc.)
- **Request Latency (p50/p90/p99)** - Latency percentiles by operation
- **Error Rate by Code** - Errors per second by error code
- **Connections** - Active connections and connection rate

### State Machines
- **Active Subscriptions** - Real-time watch subscriptions
- **Events Forwarded Rate** - Event streaming throughput
- **Total Instances** - Current instance count
- **Total Machines** - Registered machine definitions
- **WAL Entries** - Write-Ahead Log entry count
- **WAL Segments** - WAL segment count

### WAL I/O
- **WAL Size** - Total WAL storage size
- **WAL Throughput** - Read/write bytes per second
- **WAL Operations** - Writes, reads, and fsyncs per second

### System Resources (Linux)
- **CPU Usage** - Process CPU utilization
- **Memory Usage** - Resident and virtual memory
- **File Descriptors** - Open vs max file descriptors

## Requirements

- Grafana 10.0+
- Prometheus datasource
- rstmdb server with metrics enabled (default port 9090)

## Installation

### From grafana.com

1. Go to **Dashboards** → **New** → **Import**
2. Enter the dashboard ID or paste the JSON
3. Select your Prometheus datasource
4. Click **Import**

### From file

1. Copy `rstmdb.json` to your Grafana provisioning directory
2. Or import via **Dashboards** → **New** → **Import** → **Upload JSON file**

## Prometheus Configuration

Add rstmdb to your Prometheus scrape config:

```yaml
scrape_configs:
  - job_name: 'rstmdb'
    static_configs:
      - targets: ['localhost:9090']
```

## Metrics Reference

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_requests_total` | Counter | Total requests by operation |
| `rstmdb_request_duration_seconds` | Histogram | Request latency |
| `rstmdb_errors_total` | Counter | Errors by code |
| `rstmdb_connections_active` | Gauge | Active connections |
| `rstmdb_connections_total` | Counter | Total connections |
| `rstmdb_instances_total` | Gauge | Total instances |
| `rstmdb_machines_total` | Gauge | Total machines |
| `rstmdb_subscriptions_active` | Gauge | Active subscriptions |
| `rstmdb_events_forwarded_total` | Counter | Forwarded events |
| `rstmdb_wal_entries` | Gauge | WAL entry count |
| `rstmdb_wal_segments` | Gauge | WAL segment count |
| `rstmdb_wal_size_bytes` | Gauge | WAL size |
| `rstmdb_wal_bytes_written_total` | Counter | WAL bytes written |
| `rstmdb_wal_bytes_read_total` | Counter | WAL bytes read |
| `rstmdb_wal_writes_total` | Counter | WAL write operations |
| `rstmdb_wal_reads_total` | Counter | WAL read operations |
| `rstmdb_wal_fsyncs_total` | Counter | WAL fsync operations |

## License

BSL-1.1
