---
sidebar_position: 5
---

# Security

Security best practices for rstmdb deployments.

## Authentication

### Token-Based Authentication

rstmdb uses SHA-256 hashed tokens for authentication.

#### Generate Token Hash

```bash
# Using CLI
rstmdb-cli hash-token my-secret-token
# Output: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08

# Using openssl
echo -n "my-secret-token" | openssl dgst -sha256
```

#### Configure Server

```yaml
auth:
  required: true
  token_hashes:
    - "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
```

#### Use Secrets File

For easier rotation, use an external secrets file:

```bash
# /etc/rstmdb/tokens
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
a5f3c6c86f1a6d3b8c4e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2b1
```

```yaml
auth:
  required: true
  secrets_file: "/etc/rstmdb/tokens"
```

Secure the secrets file:
```bash
chmod 600 /etc/rstmdb/tokens
chown rstmdb:rstmdb /etc/rstmdb/tokens
```

### Token Best Practices

1. **Generate strong tokens** - Use cryptographic random generators
   ```bash
   openssl rand -hex 32
   ```

2. **Rotate regularly** - Add new token, update clients, remove old token

3. **Separate tokens** - Use different tokens for different services

4. **Never log tokens** - Ensure tokens don't appear in logs

## Transport Security (TLS)

### Enable TLS

```yaml
tls:
  enabled: true
  cert_path: "/etc/rstmdb/server.pem"
  key_path: "/etc/rstmdb/server-key.pem"
```

### Generate Certificates

Using OpenSSL:

```bash
# Generate CA
openssl genrsa -out ca-key.pem 4096
openssl req -new -x509 -days 365 -key ca-key.pem -out ca.pem \
  -subj "/CN=rstmdb-ca/O=MyOrg"

# Generate server key and CSR
openssl genrsa -out server-key.pem 4096
openssl req -new -key server-key.pem -out server.csr \
  -subj "/CN=rstmdb.example.com/O=MyOrg"

# Create server certificate
cat > server-ext.cnf << EOF
subjectAltName = DNS:rstmdb.example.com, DNS:localhost, IP:127.0.0.1
EOF

openssl x509 -req -days 365 -in server.csr \
  -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
  -out server.pem -extfile server-ext.cnf

# Verify certificate
openssl x509 -in server.pem -text -noout
```

### Mutual TLS (mTLS)

Require client certificates:

```yaml
tls:
  enabled: true
  cert_path: "/etc/rstmdb/server.pem"
  key_path: "/etc/rstmdb/server-key.pem"
  require_client_cert: true
  client_ca_path: "/etc/rstmdb/client-ca.pem"
```

Generate client certificate:

```bash
# Generate client key and CSR
openssl genrsa -out client-key.pem 4096
openssl req -new -key client-key.pem -out client.csr \
  -subj "/CN=my-service/O=MyOrg"

# Sign with CA
openssl x509 -req -days 365 -in client.csr \
  -CA ca.pem -CAkey ca-key.pem -CAcreateserial \
  -out client.pem

# Client connection
rstmdb-cli --tls \
  --ca-cert ca.pem \
  --client-cert client.pem \
  --client-key client-key.pem \
  -s rstmdb.example.com:7401 \
  ping
```

### Certificate Rotation

```bash
#!/bin/bash
# rotate-certs.sh

# Generate new certificate
./generate-cert.sh server-new.pem server-key-new.pem

# Swap certificates
mv /etc/rstmdb/server.pem /etc/rstmdb/server-old.pem
mv /etc/rstmdb/server-key.pem /etc/rstmdb/server-key-old.pem
mv server-new.pem /etc/rstmdb/server.pem
mv server-key-new.pem /etc/rstmdb/server-key.pem

# Reload server (requires SIGHUP support)
systemctl reload rstmdb

# Verify
rstmdb-cli --tls --ca-cert ca.pem ping
```

## Network Security

### Bind to Localhost

For local-only access:

```yaml
network:
  bind_addr: "127.0.0.1:7401"
```

### Firewall Rules

```bash
# Allow only from specific IPs
iptables -A INPUT -p tcp --dport 7401 -s 10.0.0.0/8 -j ACCEPT
iptables -A INPUT -p tcp --dport 7401 -j DROP

# Or with ufw
ufw allow from 10.0.0.0/8 to any port 7401
```

### Network Policies (Kubernetes)

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: rstmdb-policy
spec:
  podSelector:
    matchLabels:
      app: rstmdb
  policyTypes:
    - Ingress
  ingress:
    - from:
        - podSelector:
            matchLabels:
              access: rstmdb
      ports:
        - protocol: TCP
          port: 7401
```

## Data Security

### File Permissions

```bash
# Data directory
chmod 700 /var/lib/rstmdb
chown rstmdb:rstmdb /var/lib/rstmdb

# Configuration
chmod 600 /etc/rstmdb/*
chown rstmdb:rstmdb /etc/rstmdb/*

# Secrets
chmod 600 /etc/rstmdb/tokens
```

### Encryption at Rest

Currently, rstmdb doesn't provide built-in encryption at rest. Options:

1. **Filesystem encryption** - Use dm-crypt/LUKS

   ```bash
   # Create encrypted volume
   cryptsetup luksFormat /dev/sdb
   cryptsetup luksOpen /dev/sdb rstmdb-data
   mkfs.ext4 /dev/mapper/rstmdb-data
   mount /dev/mapper/rstmdb-data /var/lib/rstmdb
   ```

2. **Cloud encryption** - Use encrypted EBS/GCE persistent disks

3. **Application-level** - Encrypt sensitive context data before storing

### Sensitive Data in Context

Avoid storing sensitive data directly in instance context:

```json
// Bad - storing secrets
{
  "customer": "alice",
  "credit_card": "4111111111111111",
  "ssn": "123-45-6789"
}

// Good - store references
{
  "customer": "alice",
  "payment_method_id": "pm_123abc",
  "identity_verified": true
}
```

## Process Security

### Run as Non-Root

```bash
# Create dedicated user
useradd -r -s /bin/false rstmdb

# Run as non-root
su -s /bin/sh rstmdb -c "/usr/local/bin/rstmdb"
```

### Systemd Hardening

```ini
[Service]
User=rstmdb
Group=rstmdb
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
MemoryDenyWriteExecute=true
LockPersonality=true
ReadWritePaths=/var/lib/rstmdb
```

### Container Security

```dockerfile
# Run as non-root
USER rstmdb

# Read-only root filesystem
# (mount /data as volume)
```

```yaml
# Kubernetes
securityContext:
  runAsNonRoot: true
  runAsUser: 1000
  readOnlyRootFilesystem: true
  allowPrivilegeEscalation: false
  capabilities:
    drop:
      - ALL
```

## Audit Logging

Enable detailed logging for audit:

```yaml
logging:
  level: "info"
  format: "json"
```

Log entries include:
- Client IP address
- Operation performed
- Instance ID (for data operations)
- Timestamp
- Duration

Example audit log entry:
```json
{
  "timestamp": "2024-01-15T10:30:00Z",
  "level": "INFO",
  "client_addr": "10.0.1.50:45678",
  "op": "APPLY_EVENT",
  "instance_id": "order-001",
  "event": "PAY",
  "duration_ms": 5
}
```

## Security Checklist

- [ ] Authentication enabled (`auth.required: true`)
- [ ] Strong tokens (32+ bytes, cryptographic random)
- [ ] TLS enabled for production
- [ ] mTLS for service-to-service communication
- [ ] Firewall configured
- [ ] Running as non-root user
- [ ] File permissions restricted
- [ ] Filesystem encryption for sensitive data
- [ ] Regular token rotation
- [ ] Audit logging enabled
- [ ] Backup encryption
- [ ] Network policies in place (Kubernetes)
