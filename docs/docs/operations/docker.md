---
sidebar_position: 2
---

# Docker Deployment

Deploy rstmdb using Docker and Docker Compose.

## Quick Start

```bash
# Run with defaults
docker run -p 7401:7401 rstmdb/rstmdb

# With persistent storage
docker run -p 7401:7401 -v rstmdb-data:/data rstmdb/rstmdb

# With authentication
docker run -p 7401:7401 \
  -e RSTMDB_AUTH_REQUIRED=true \
  -e RSTMDB_AUTH_TOKEN_HASH=<sha256-hash> \
  -v rstmdb-data:/data \
  rstmdb/rstmdb
```

## Docker Image

### Available Tags

| Tag | Description |
|-----|-------------|
| `latest` | Latest stable release |
| `x.y.z` | Specific version |
| `main` | Latest development build |

### Image Details

- Base: Debian Bookworm Slim
- User: `rstmdb` (non-root)
- Data directory: `/data`
- Config directory: `/etc/rstmdb`
- Default port: 7401
- Metrics port: 9090

## Docker Compose

### Basic Setup

```yaml
# docker-compose.yml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    ports:
      - "7401:7401"
      - "9090:9090"
    volumes:
      - rstmdb-data:/data
    environment:
      - RUST_LOG=info
    restart: unless-stopped

volumes:
  rstmdb-data:
```

### With Authentication

```yaml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    ports:
      - "7401:7401"
    volumes:
      - rstmdb-data:/data
      - ./tokens:/etc/rstmdb/tokens:ro
    environment:
      - RSTMDB_AUTH_REQUIRED=true
      - RSTMDB_AUTH_SECRETS_FILE=/etc/rstmdb/tokens
    restart: unless-stopped

volumes:
  rstmdb-data:
```

### With TLS

```yaml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    ports:
      - "7401:7401"
    volumes:
      - rstmdb-data:/data
      - ./certs:/etc/rstmdb/certs:ro
    environment:
      - RSTMDB_TLS_ENABLED=true
      - RSTMDB_TLS_CERT=/etc/rstmdb/certs/server.pem
      - RSTMDB_TLS_KEY=/etc/rstmdb/certs/server-key.pem
    restart: unless-stopped

volumes:
  rstmdb-data:
```

### With Custom Config

```yaml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    ports:
      - "7401:7401"
      - "9090:9090"
    volumes:
      - rstmdb-data:/data
      - ./config.yaml:/etc/rstmdb/config.yaml:ro
    environment:
      - RSTMDB_CONFIG=/etc/rstmdb/config.yaml
    restart: unless-stopped

volumes:
  rstmdb-data:
```

### Production Setup

```yaml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    ports:
      - "7401:7401"
    expose:
      - "9090"
    volumes:
      - rstmdb-data:/data
      - ./config:/etc/rstmdb:ro
    environment:
      - RSTMDB_CONFIG=/etc/rstmdb/config.yaml
    deploy:
      resources:
        limits:
          memory: 2G
        reservations:
          memory: 512M
    healthcheck:
      test: ["CMD", "rstmdb-cli", "-s", "localhost:7401", "ping"]
      interval: 10s
      timeout: 5s
      retries: 3
    restart: unless-stopped
    logging:
      driver: json-file
      options:
        max-size: "100m"
        max-file: "3"

volumes:
  rstmdb-data:
```

## Building the Image

### From Source

```dockerfile
# Dockerfile
FROM rust:1.75-bookworm as builder

WORKDIR /build
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim

RUN useradd -r -s /bin/false rstmdb && \
    mkdir -p /data /etc/rstmdb && \
    chown -R rstmdb:rstmdb /data /etc/rstmdb

COPY --from=builder /build/target/release/rstmdb /usr/local/bin/
COPY --from=builder /build/target/release/rstmdb-cli /usr/local/bin/

USER rstmdb
WORKDIR /data

ENV RSTMDB_BIND=0.0.0.0:7401
ENV RSTMDB_DATA=/data
ENV RUST_LOG=info

EXPOSE 7401 9090

CMD ["rstmdb"]
```

Build:
```bash
docker build -t rstmdb:local .
```

## Volume Management

### Named Volumes

```bash
# Create volume
docker volume create rstmdb-data

# Run with volume
docker run -v rstmdb-data:/data rstmdb/rstmdb

# Inspect volume
docker volume inspect rstmdb-data

# Backup volume
docker run --rm -v rstmdb-data:/data -v $(pwd):/backup alpine \
  tar czf /backup/rstmdb-backup.tar.gz -C /data .

# Restore volume
docker run --rm -v rstmdb-data:/data -v $(pwd):/backup alpine \
  sh -c "rm -rf /data/* && tar xzf /backup/rstmdb-backup.tar.gz -C /data"
```

### Bind Mounts

```bash
# Create local directory
mkdir -p ./rstmdb-data
chown 1000:1000 ./rstmdb-data  # Match container user

# Run with bind mount
docker run -v $(pwd)/rstmdb-data:/data rstmdb/rstmdb
```

## Environment Variables

All [configuration options](/configuration) are available as environment variables:

```bash
docker run \
  -e RSTMDB_BIND=0.0.0.0:7401 \
  -e RSTMDB_DATA=/data \
  -e RSTMDB_AUTH_REQUIRED=true \
  -e RSTMDB_AUTH_TOKEN_HASH=abc123... \
  -e RSTMDB_COMPACT_ENABLED=true \
  -e RSTMDB_COMPACT_EVENTS=10000 \
  -e RUST_LOG=info \
  rstmdb/rstmdb
```

## Health Checks

### Docker Health Check

```yaml
healthcheck:
  test: ["CMD", "rstmdb-cli", "-s", "localhost:7401", "ping"]
  interval: 10s
  timeout: 5s
  retries: 3
  start_period: 5s
```

### External Health Check

```bash
# Using docker exec
docker exec rstmdb rstmdb-cli ping

# Using TCP check
docker run --rm --network container:rstmdb alpine nc -zv localhost 7401
```

## Networking

### Bridge Network

```yaml
version: '3.8'

services:
  rstmdb:
    image: rstmdb/rstmdb:latest
    networks:
      - backend

  app:
    image: your-app:latest
    networks:
      - backend
    environment:
      - RSTMDB_SERVER=rstmdb:7401

networks:
  backend:
```

### Host Network

For maximum performance:

```bash
docker run --network host rstmdb/rstmdb
```

## Logging

### View Logs

```bash
# Follow logs
docker logs -f rstmdb

# Last 100 lines
docker logs --tail 100 rstmdb
```

### Log Drivers

```yaml
services:
  rstmdb:
    logging:
      driver: json-file
      options:
        max-size: "100m"
        max-file: "5"
```

Or use external logging:

```yaml
services:
  rstmdb:
    logging:
      driver: fluentd
      options:
        fluentd-address: localhost:24224
        tag: rstmdb
```

## Kubernetes

See [Kubernetes Deployment](/operations/deployment) for Kubernetes-specific configuration.

### Quick Kubernetes Example

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: rstmdb
spec:
  replicas: 1
  selector:
    matchLabels:
      app: rstmdb
  template:
    metadata:
      labels:
        app: rstmdb
    spec:
      containers:
      - name: rstmdb
        image: rstmdb/rstmdb:latest
        ports:
        - containerPort: 7401
        volumeMounts:
        - name: data
          mountPath: /data
      volumes:
      - name: data
        persistentVolumeClaim:
          claimName: rstmdb-data
```
