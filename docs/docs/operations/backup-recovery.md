---
sidebar_position: 4
---

# Backup and Recovery

Strategies for backing up and recovering rstmdb data.

## Data Structure

rstmdb stores data in:

```
data/
├── wal/              # Write-ahead log segments
│   ├── 0000000000000001.wal
│   ├── 0000000000000002.wal
│   └── ...
└── snapshots/        # Point-in-time snapshots
    ├── index.json
    └── snap-*.snap
```

**Important:** Both `wal/` and `snapshots/` are required for recovery.

## Backup Methods

### Cold Backup

Stop the server and copy data directory:

```bash
#!/bin/bash
# cold-backup.sh

BACKUP_DIR="/backup/rstmdb/$(date +%Y%m%d-%H%M%S)"
DATA_DIR="/var/lib/rstmdb"

# Stop server
systemctl stop rstmdb

# Copy data
mkdir -p "$BACKUP_DIR"
cp -r "$DATA_DIR"/* "$BACKUP_DIR/"

# Compress
tar -czf "$BACKUP_DIR.tar.gz" -C "$(dirname $BACKUP_DIR)" "$(basename $BACKUP_DIR)"
rm -rf "$BACKUP_DIR"

# Start server
systemctl start rstmdb

echo "Backup created: $BACKUP_DIR.tar.gz"
```

### Hot Backup

Backup without stopping the server (recommended):

```bash
#!/bin/bash
# hot-backup.sh

BACKUP_DIR="/backup/rstmdb/$(date +%Y%m%d-%H%M%S)"
DATA_DIR="/var/lib/rstmdb"

# Trigger compaction to create consistent snapshot
rstmdb-cli compact

# Wait for compaction
sleep 5

# Get current WAL offset
WAL_OFFSET=$(rstmdb-cli --json wal-stats | jq '.current_offset')
echo "Backing up at WAL offset: $WAL_OFFSET"

# Copy snapshots first (immutable)
mkdir -p "$BACKUP_DIR/snapshots"
cp -r "$DATA_DIR/snapshots"/* "$BACKUP_DIR/snapshots/"

# Copy WAL segments
mkdir -p "$BACKUP_DIR/wal"
cp -r "$DATA_DIR/wal"/* "$BACKUP_DIR/wal/"

# Record backup metadata
cat > "$BACKUP_DIR/backup.json" << EOF
{
  "timestamp": "$(date -Iseconds)",
  "wal_offset": $WAL_OFFSET,
  "type": "hot"
}
EOF

# Compress
tar -czf "$BACKUP_DIR.tar.gz" -C "$(dirname $BACKUP_DIR)" "$(basename $BACKUP_DIR)"
rm -rf "$BACKUP_DIR"

echo "Backup created: $BACKUP_DIR.tar.gz"
```

### Filesystem Snapshots

Use filesystem snapshots for instant backups:

#### LVM

```bash
#!/bin/bash
# lvm-snapshot-backup.sh

VG="vg0"
LV="rstmdb"
MOUNT_POINT="/mnt/rstmdb-snapshot"
BACKUP_DIR="/backup/rstmdb"

# Create LVM snapshot
lvcreate -L 10G -s -n rstmdb-snap /dev/$VG/$LV

# Mount snapshot
mkdir -p $MOUNT_POINT
mount /dev/$VG/rstmdb-snap $MOUNT_POINT

# Copy from snapshot
BACKUP_FILE="$BACKUP_DIR/rstmdb-$(date +%Y%m%d-%H%M%S).tar.gz"
tar -czf "$BACKUP_FILE" -C $MOUNT_POINT .

# Cleanup
umount $MOUNT_POINT
lvremove -f /dev/$VG/rstmdb-snap

echo "Backup created: $BACKUP_FILE"
```

#### ZFS

```bash
#!/bin/bash
# zfs-snapshot-backup.sh

DATASET="tank/rstmdb"
BACKUP_DIR="/backup/rstmdb"

# Create snapshot
SNAPSHOT_NAME="backup-$(date +%Y%m%d-%H%M%S)"
zfs snapshot $DATASET@$SNAPSHOT_NAME

# Send to file
BACKUP_FILE="$BACKUP_DIR/$SNAPSHOT_NAME.zfs"
zfs send $DATASET@$SNAPSHOT_NAME > "$BACKUP_FILE"

# Optionally compress
gzip "$BACKUP_FILE"

echo "Backup created: $BACKUP_FILE.gz"
```

## Backup Schedule

### Cron Example

```cron
# /etc/cron.d/rstmdb-backup

# Hot backup every 6 hours
0 */6 * * * root /opt/rstmdb/scripts/hot-backup.sh

# Full cold backup weekly (Sunday 3 AM)
0 3 * * 0 root /opt/rstmdb/scripts/cold-backup.sh

# Cleanup old backups (keep 30 days)
0 4 * * * root find /backup/rstmdb -name "*.tar.gz" -mtime +30 -delete
```

## Recovery

### Full Recovery

Restore from a complete backup:

```bash
#!/bin/bash
# restore.sh

BACKUP_FILE="$1"
DATA_DIR="/var/lib/rstmdb"

if [ -z "$BACKUP_FILE" ]; then
  echo "Usage: $0 <backup-file.tar.gz>"
  exit 1
fi

# Stop server
systemctl stop rstmdb

# Clear existing data
rm -rf "$DATA_DIR"/*

# Extract backup
tar -xzf "$BACKUP_FILE" -C "$DATA_DIR" --strip-components=1

# Fix permissions
chown -R rstmdb:rstmdb "$DATA_DIR"

# Start server
systemctl start rstmdb

# Verify
rstmdb-cli info

echo "Recovery complete"
```

### Point-in-Time Recovery

Recover to a specific WAL offset:

```bash
#!/bin/bash
# pit-recovery.sh

BACKUP_FILE="$1"
TARGET_OFFSET="$2"
DATA_DIR="/var/lib/rstmdb"

# Extract backup
tar -xzf "$BACKUP_FILE" -C "$DATA_DIR" --strip-components=1

# Start server with recovery limit
# (Requires future feature: --recovery-target-offset)
RSTMDB_RECOVERY_TARGET_OFFSET=$TARGET_OFFSET rstmdb

echo "Recovered to offset $TARGET_OFFSET"
```

### Disaster Recovery

Complete recovery procedure:

```bash
#!/bin/bash
# disaster-recovery.sh

# 1. Find latest backup
LATEST_BACKUP=$(ls -t /backup/rstmdb/*.tar.gz | head -1)
echo "Using backup: $LATEST_BACKUP"

# 2. Prepare new server
apt-get update && apt-get install -y rstmdb

# 3. Stop any running instance
systemctl stop rstmdb 2>/dev/null || true

# 4. Restore data
mkdir -p /var/lib/rstmdb
tar -xzf "$LATEST_BACKUP" -C /var/lib/rstmdb --strip-components=1
chown -R rstmdb:rstmdb /var/lib/rstmdb

# 5. Start server
systemctl start rstmdb

# 6. Verify
rstmdb-cli info
rstmdb-cli --json wal-stats | jq '.current_offset'

echo "Disaster recovery complete"
```

## Verification

### Verify Backup Integrity

```bash
#!/bin/bash
# verify-backup.sh

BACKUP_FILE="$1"
TEMP_DIR=$(mktemp -d)

# Extract backup
tar -xzf "$BACKUP_FILE" -C "$TEMP_DIR"

# Check for required files
if [ ! -d "$TEMP_DIR/wal" ]; then
  echo "ERROR: Missing wal directory"
  exit 1
fi

if [ ! -d "$TEMP_DIR/snapshots" ]; then
  echo "WARNING: Missing snapshots directory"
fi

# Count WAL segments
WAL_COUNT=$(ls "$TEMP_DIR/wal"/*.wal 2>/dev/null | wc -l)
echo "WAL segments: $WAL_COUNT"

# Check backup metadata
if [ -f "$TEMP_DIR/backup.json" ]; then
  cat "$TEMP_DIR/backup.json"
fi

# Cleanup
rm -rf "$TEMP_DIR"

echo "Backup verification complete"
```

### Test Recovery

Regularly test recovery in a staging environment:

```bash
#!/bin/bash
# test-recovery.sh

BACKUP_FILE="$1"
TEST_PORT=7402

# Start test instance
docker run -d --name rstmdb-test \
  -p $TEST_PORT:7401 \
  -v $(mktemp -d):/data \
  rstmdb/rstmdb

# Restore backup to test instance
docker cp "$BACKUP_FILE" rstmdb-test:/tmp/backup.tar.gz
docker exec rstmdb-test sh -c "rm -rf /data/* && tar -xzf /tmp/backup.tar.gz -C /data --strip-components=1"
docker restart rstmdb-test

# Wait for startup
sleep 5

# Verify
rstmdb-cli -s localhost:$TEST_PORT info
INSTANCE_COUNT=$(rstmdb-cli -s localhost:$TEST_PORT --json list-instances | jq '.total')
echo "Recovered instances: $INSTANCE_COUNT"

# Cleanup
docker rm -f rstmdb-test

echo "Test recovery successful"
```

## Best Practices

1. **Regular backups** - At least daily, more frequently for critical data
2. **Test recoveries** - Monthly recovery tests in staging
3. **Offsite storage** - Store backups in different locations
4. **Encryption** - Encrypt backup files at rest
5. **Monitoring** - Alert on backup failures
6. **Retention policy** - Keep multiple backup versions
7. **Documentation** - Document recovery procedures

## Encryption

Encrypt backups with GPG:

```bash
# Encrypt
tar -czf - /var/lib/rstmdb | gpg --symmetric --cipher-algo AES256 -o backup.tar.gz.gpg

# Decrypt
gpg -d backup.tar.gz.gpg | tar -xzf - -C /var/lib/rstmdb
```

Or with age:

```bash
# Generate key
age-keygen -o key.txt

# Encrypt
tar -czf - /var/lib/rstmdb | age -r $(cat key.txt | grep 'public' | cut -d: -f2) > backup.tar.gz.age

# Decrypt
age -d -i key.txt backup.tar.gz.age | tar -xzf - -C /var/lib/rstmdb
```
