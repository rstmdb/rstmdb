---
sidebar_position: 3
---

# Monitoring

Monitor rstmdb using Prometheus metrics and logging.

## Metrics Endpoint

Enable metrics in configuration:

```yaml
metrics:
  enabled: true
  bind_addr: "0.0.0.0:9090"
```

Access metrics:
```bash
curl http://localhost:9090/metrics
```

## Available Metrics

### Connection Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_connections_active` | Gauge | Current active connections |
| `rstmdb_connections_total` | Counter | Total connections since start |
| `rstmdb_connections_rejected` | Counter | Rejected connections (limit reached) |

### Request Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_requests_total{op}` | Counter | Total requests by operation |
| `rstmdb_requests_duration_seconds{op}` | Histogram | Request duration by operation |
| `rstmdb_requests_errors_total{op,code}` | Counter | Errors by operation and code |

### State Machine Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_machines_total` | Gauge | Total machine definitions |
| `rstmdb_instances_total` | Gauge | Total instances |
| `rstmdb_instances_by_state{machine,state}` | Gauge | Instances by machine and state |
| `rstmdb_events_applied_total{machine}` | Counter | Events applied by machine |
| `rstmdb_transitions_total{machine,from,to}` | Counter | Transitions by machine and states |

### WAL Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_wal_offset` | Gauge | Current WAL offset |
| `rstmdb_wal_size_bytes` | Gauge | Total WAL size |
| `rstmdb_wal_segments` | Gauge | Number of WAL segments |
| `rstmdb_wal_writes_total` | Counter | WAL writes |
| `rstmdb_wal_write_duration_seconds` | Histogram | WAL write duration |
| `rstmdb_wal_fsyncs_total` | Counter | Fsync operations |

### Subscription Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_subscriptions_active` | Gauge | Active subscriptions |
| `rstmdb_events_broadcast_total` | Counter | Events broadcast to subscribers |

### System Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rstmdb_uptime_seconds` | Gauge | Server uptime |
| `rstmdb_memory_used_bytes` | Gauge | Memory usage |

## Prometheus Configuration

Add to `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'rstmdb'
    static_configs:
      - targets: ['localhost:9090']
    scrape_interval: 15s
```

### With Service Discovery

```yaml
scrape_configs:
  - job_name: 'rstmdb'
    dns_sd_configs:
      - names:
          - '_rstmdb._tcp.service.consul'
```

## Grafana Dashboard

Import the pre-built dashboard or create custom panels.

### Key Panels

#### Request Rate

```promql
rate(rstmdb_requests_total[5m])
```

#### Request Latency (p99)

```promql
histogram_quantile(0.99, rate(rstmdb_requests_duration_seconds_bucket[5m]))
```

#### Error Rate

```promql
rate(rstmdb_requests_errors_total[5m])
```

#### Instance Count

```promql
rstmdb_instances_total
```

#### WAL Size

```promql
rstmdb_wal_size_bytes / 1024 / 1024
```

#### Events per Second

```promql
rate(rstmdb_events_applied_total[1m])
```

### Sample Dashboard JSON

```json
{
  "dashboard": {
    "title": "rstmdb Overview",
    "panels": [
      {
        "title": "Request Rate",
        "type": "graph",
        "targets": [
          {
            "expr": "sum(rate(rstmdb_requests_total[5m])) by (op)",
            "legendFormat": "{{op}}"
          }
        ]
      },
      {
        "title": "Request Latency (p99)",
        "type": "graph",
        "targets": [
          {
            "expr": "histogram_quantile(0.99, sum(rate(rstmdb_requests_duration_seconds_bucket[5m])) by (le, op))",
            "legendFormat": "{{op}}"
          }
        ]
      },
      {
        "title": "Active Connections",
        "type": "stat",
        "targets": [
          {
            "expr": "rstmdb_connections_active"
          }
        ]
      },
      {
        "title": "Instance Count",
        "type": "stat",
        "targets": [
          {
            "expr": "rstmdb_instances_total"
          }
        ]
      },
      {
        "title": "WAL Size (MB)",
        "type": "stat",
        "targets": [
          {
            "expr": "rstmdb_wal_size_bytes / 1024 / 1024"
          }
        ]
      }
    ]
  }
}
```

## Alerting Rules

### Prometheus Alerting

```yaml
groups:
  - name: rstmdb
    rules:
      - alert: RstmdbDown
        expr: up{job="rstmdb"} == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "rstmdb is down"

      - alert: RstmdbHighErrorRate
        expr: rate(rstmdb_requests_errors_total[5m]) > 10
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High error rate in rstmdb"

      - alert: RstmdbHighLatency
        expr: histogram_quantile(0.99, rate(rstmdb_requests_duration_seconds_bucket[5m])) > 1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High latency in rstmdb (p99 > 1s)"

      - alert: RstmdbWALGrowing
        expr: increase(rstmdb_wal_size_bytes[1h]) > 1073741824
        for: 30m
        labels:
          severity: warning
        annotations:
          summary: "WAL growing rapidly (>1GB/hour)"

      - alert: RstmdbConnectionsNearLimit
        expr: rstmdb_connections_active / rstmdb_connections_limit > 0.8
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Connections near limit (>80%)"
```

## Logging

### Structured Logging

Configure JSON logging:

```yaml
logging:
  format: "json"
  level: "info"
```

Log output:
```json
{"timestamp":"2024-01-15T10:30:00.123Z","level":"INFO","target":"rstmdb_server","message":"Request completed","op":"APPLY_EVENT","duration_ms":5,"instance_id":"order-001"}
```

### Log Levels

| Level | Description |
|-------|-------------|
| `error` | Errors only |
| `warn` | Warnings and errors |
| `info` | Normal operations (default) |
| `debug` | Detailed debugging |
| `trace` | Very verbose tracing |

Set via environment:
```bash
RUST_LOG=debug rstmdb
```

### Log Aggregation

#### Fluentd

```yaml
# fluent.conf
<source>
  @type forward
  port 24224
</source>

<filter rstmdb.**>
  @type parser
  key_name log
  <parse>
    @type json
  </parse>
</filter>

<match rstmdb.**>
  @type elasticsearch
  host elasticsearch
  port 9200
  index_name rstmdb
</match>
```

#### Vector

```toml
# vector.toml
[sources.rstmdb]
type = "docker_logs"
include_containers = ["rstmdb"]

[transforms.parse_json]
type = "json_parser"
inputs = ["rstmdb"]

[sinks.elasticsearch]
type = "elasticsearch"
inputs = ["parse_json"]
endpoint = "http://elasticsearch:9200"
index = "rstmdb-%Y-%m-%d"
```

## Health Checks

### CLI Health Check

```bash
#!/bin/bash
# health-check.sh

if rstmdb-cli -s localhost:7401 ping > /dev/null 2>&1; then
  echo "OK"
  exit 0
else
  echo "FAIL"
  exit 1
fi
```

### HTTP Health Check

The metrics endpoint serves as a health check:

```bash
curl -f http://localhost:9090/health
```

### Kubernetes Probes

```yaml
livenessProbe:
  exec:
    command:
      - rstmdb-cli
      - -s
      - localhost:7401
      - ping
  initialDelaySeconds: 5
  periodSeconds: 10

readinessProbe:
  exec:
    command:
      - rstmdb-cli
      - -s
      - localhost:7401
      - ping
  initialDelaySeconds: 5
  periodSeconds: 5
```
