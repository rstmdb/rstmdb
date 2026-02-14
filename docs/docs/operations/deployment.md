---
sidebar_position: 1
---

# Deployment

Guide for deploying rstmdb in various environments.

## System Requirements

### Minimum Requirements

| Resource | Minimum | Recommended |
|----------|---------|-------------|
| CPU | 1 core | 2+ cores |
| Memory | 512 MB | 2+ GB |
| Disk | 1 GB | 10+ GB SSD |
| OS | Linux, macOS | Linux (Ubuntu 22.04+) |

### Memory Sizing

rstmdb stores all instances in memory. Estimate requirements:

```
Memory ≈ (instance_count × avg_context_size) + overhead
```

Example:
- 100,000 instances
- Average context size: 1 KB
- Memory: ~100 MB for instances + ~200 MB overhead = 300 MB

## Installation Methods

### From Source

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/rstmdb/rstmdb.git
cd rstmdb
cargo build --release

# Install binaries
sudo cp target/release/rstmdb /usr/local/bin/
sudo cp target/release/rstmdb-cli /usr/local/bin/
```

### Using Docker

```bash
docker pull rstmdb/rstmdb:latest
```

### From Package (Coming Soon)

```bash
# Debian/Ubuntu
sudo apt install rstmdb

# macOS
brew install rstmdb
```

## Systemd Service

Create `/etc/systemd/system/rstmdb.service`:

```ini
[Unit]
Description=rstmdb State Machine Database
After=network.target

[Service]
Type=simple
User=rstmdb
Group=rstmdb
ExecStart=/usr/local/bin/rstmdb
Restart=always
RestartSec=5

# Environment
Environment=RSTMDB_CONFIG=/etc/rstmdb/config.yaml
Environment=RUST_LOG=info

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/rstmdb
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

Setup:

```bash
# Create user and directories
sudo useradd -r -s /bin/false rstmdb
sudo mkdir -p /var/lib/rstmdb /etc/rstmdb
sudo chown rstmdb:rstmdb /var/lib/rstmdb

# Create config
sudo cat > /etc/rstmdb/config.yaml << 'EOF'
network:
  bind_addr: "0.0.0.0:7401"

storage:
  data_dir: "/var/lib/rstmdb"
  fsync_policy: every_write

auth:
  required: true
  secrets_file: "/etc/rstmdb/tokens"

metrics:
  enabled: true
  bind_addr: "0.0.0.0:9090"
EOF

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable rstmdb
sudo systemctl start rstmdb
```

## Directory Structure

```
/etc/rstmdb/
├── config.yaml       # Main configuration
├── tokens            # Token hashes
├── server.pem        # TLS certificate
└── server-key.pem    # TLS private key

/var/lib/rstmdb/
├── wal/              # WAL segments
│   ├── 0000000000000001.wal
│   └── ...
└── snapshots/        # Snapshots
    ├── index.json
    └── ...

/var/log/rstmdb/      # Logs (if file logging enabled)
└── rstmdb.log
```

## Network Configuration

### Firewall Rules

```bash
# Allow rstmdb port
sudo ufw allow 7401/tcp

# Allow metrics port (internal only)
sudo ufw allow from 10.0.0.0/8 to any port 9090
```

### Load Balancer

For TCP load balancing (HAProxy example):

```
frontend rstmdb_front
    bind *:7401
    mode tcp
    default_backend rstmdb_back

backend rstmdb_back
    mode tcp
    balance roundrobin
    option tcp-check
    server rstmdb1 10.0.1.10:7401 check
    server rstmdb2 10.0.1.11:7401 check backup
```

**Note:** rstmdb is currently single-node. Use backup server for failover only.

## Health Checks

### TCP Health Check

```bash
nc -zv localhost 7401
```

### Application Health Check

```bash
rstmdb-cli -s localhost:7401 ping
```

### HTTP Health Check (via metrics)

```bash
curl -f http://localhost:9090/health
```

## Log Management

### Structured Logging

Configure JSON logging for log aggregation:

```yaml
logging:
  format: "json"
```

Output:
```json
{"timestamp":"2024-01-15T10:30:00Z","level":"INFO","message":"Server started","bind_addr":"0.0.0.0:7401"}
```

### Log Rotation

Using logrotate (`/etc/logrotate.d/rstmdb`):

```
/var/log/rstmdb/*.log {
    daily
    rotate 7
    compress
    delaycompress
    missingok
    notifempty
    create 640 rstmdb rstmdb
    postrotate
        systemctl reload rstmdb
    endscript
}
```

## Backup Strategy

### WAL Backup

```bash
#!/bin/bash
# Backup WAL and snapshots

BACKUP_DIR="/backup/rstmdb/$(date +%Y%m%d)"
DATA_DIR="/var/lib/rstmdb"

mkdir -p "$BACKUP_DIR"

# Stop writes temporarily or use filesystem snapshot
rsync -av "$DATA_DIR/wal/" "$BACKUP_DIR/wal/"
rsync -av "$DATA_DIR/snapshots/" "$BACKUP_DIR/snapshots/"

# Compress
tar -czf "$BACKUP_DIR.tar.gz" -C /backup/rstmdb "$(date +%Y%m%d)"
rm -rf "$BACKUP_DIR"
```

### Restore

```bash
#!/bin/bash
# Restore from backup

BACKUP_FILE="/backup/rstmdb/20240115.tar.gz"
DATA_DIR="/var/lib/rstmdb"

# Stop server
systemctl stop rstmdb

# Clear and restore
rm -rf "$DATA_DIR"/*
tar -xzf "$BACKUP_FILE" -C "$DATA_DIR" --strip-components=1

# Fix permissions
chown -R rstmdb:rstmdb "$DATA_DIR"

# Start server
systemctl start rstmdb
```

## Upgrade Procedure

```bash
# 1. Build new version
cd rstmdb
git pull
cargo build --release

# 2. Stop service
sudo systemctl stop rstmdb

# 3. Backup current binary
sudo cp /usr/local/bin/rstmdb /usr/local/bin/rstmdb.bak

# 4. Install new binary
sudo cp target/release/rstmdb /usr/local/bin/

# 5. Start service
sudo systemctl start rstmdb

# 6. Verify
rstmdb-cli info
```

## Troubleshooting

### Server Won't Start

Check logs:
```bash
journalctl -u rstmdb -f
```

Common issues:
- Port already in use: `ss -tlnp | grep 7401`
- Permission denied: Check file ownership
- Config syntax error: Validate YAML

### Connection Refused

```bash
# Check if server is running
systemctl status rstmdb

# Check if port is open
ss -tlnp | grep 7401

# Check firewall
sudo ufw status
```

### High Memory Usage

```bash
# Check instance count
rstmdb-cli info | grep instances

# Trigger compaction
rstmdb-cli compact
```

### Slow Performance

- Check disk I/O: `iostat -x 1`
- Check CPU: `top -p $(pgrep rstmdb)`
- Consider adjusting fsync policy
