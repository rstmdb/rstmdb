---
sidebar_position: 1
---

# Client Libraries

Overview of rstmdb client libraries and protocol integration options.

## Official Clients

### Rust Client

**Status:** ✅ Available

```toml
[dependencies]
rstmdb-client = "0.3"
```

[Full documentation →](./rust)

### Python Client

**Status:** ✅ Available

```bash
pip install rstmdb
```

Features:
- Async support (asyncio)
- Sync wrapper
- Full type hints
- Connection pooling

[PyPI Package →](https://pypi.org/project/rstmdb/)

[Full documentation →](./python)

### Node.js/TypeScript Client

**Status:** ✅ Available

```bash
npm install @rstmdb/client
```

Features:
- Full TypeScript support
- Promise-based API
- Streaming support
- Connection pooling

[npm Package →](https://www.npmjs.com/package/@rstmdb/client)

[Full documentation →](./typescript)

### Go Client

**Status:** ✅ Available

```bash
go get github.com/rstmdb/rstmdb-go
```

Features:
- Zero external dependencies (standard library only)
- Full RCP protocol with RCPX binary framing
- Channel-based event streaming
- Context-based cancellation and timeouts

[pkg.go.dev Reference →](https://pkg.go.dev/github.com/rstmdb/rstmdb-go)

[Full documentation →](./go)

### C# / .NET Client

**Status:** ✅ Available

```bash
dotnet add package Rstmdb.Client
```

Features:
- Zero external dependencies
- Full RCP protocol with RCPX binary framing
- Async streaming via `System.Threading.Channels` and `IAsyncEnumerable`
- `async/await` with `CancellationToken` throughout

[NuGet Package →](https://www.nuget.org/packages/Rstmdb.Client)

[Full documentation →](./csharp)

### Java Client

**Status:** ✅ Available

```kotlin
// Gradle Kotlin DSL
implementation("com.rstmdb:rstmdb-client:0.1.0")
```

```xml
<!-- Maven -->
<dependency>
    <groupId>com.rstmdb</groupId>
    <artifactId>rstmdb-client</artifactId>
    <version>0.1.0</version>
</dependency>
```

Features:
- Java 11+ support
- Both async (`CompletableFuture`) and sync (`*Sync`) APIs
- Full RCP protocol with RCPX binary framing
- TLS/mTLS support via `SSLContext`
- Streaming subscriptions via `Iterable`

[Maven Central →](https://central.sonatype.com/artifact/com.rstmdb/rstmdb-client)

[Full documentation →](./java)

## Server–Client Compatibility Matrix

All official SDKs speak RCP `protocol_version = 1`. Upgrading an SDK to a current release is recommended when connecting to rstmdb `0.3.x` or `1.0+`:

| Client | Package | Compatible With 0.2.x | Compatible With 0.3.x | Forward-Compatible With 1.0+ |
|--------|---------|------------------------|------------------------|-------------------------------|
| Rust (`rstmdb-client`) | Crates / workspace | `0.1.x` – `0.3.x` | `0.3.x` | Open `ErrorCode::Unknown`, `applied: false`, `idempotency_key` |
| Python (`rstmdb`) | `rstmdb` (PyPI) | `0.2.x` | `> 0.2.1` | `ErrorCode \| str` fallback, v0.3+ replication & backup support |
| Node.js / TS (`@rstmdb/client`) | `@rstmdb/client` (npm) | `0.2.x` | `> 0.2.1` | Open `ErrorCode` fallback (`ServerError`), v0.3+ & v1.0 error codes |
| Go (`rstmdb-go`) | `github.com/rstmdb/rstmdb-go` | `0.2.x` | `> 0.2.1` | Open `ErrorCode string`, `NegotiatedFeatures`, hot backup & replication |
| Java (`rstmdb-client`) | `com.rstmdb:rstmdb-client` | `0.1.0` | `> 0.1.0` | Open `String errorCode`, v0.3+ & v1.0 `ErrorCodes` constants & helpers |
| C# / .NET (`Rstmdb.Client`) | `Rstmdb.Client` (NuGet) | `0.2.x` | `> 0.2.1` | Open `string ErrorCode`, v0.3+ & v1.0 constants, backup & replication |

> **Closed-enum note for older Rust (`<= 0.3.0`) and Python (`<= 0.2.1`) releases:** Earlier Rust (`ErrorCode` without `#[serde(other)]`) and Python (`ErrorCode(str, Enum)` Pydantic field) releases fail deserialization when a newer server emits a newly introduced `error.code` such as `READ_ONLY_MODE`, `REPLICATION_TIMEOUT`, `REPLICATION_ERROR`, `IDEMPOTENCY_CONFLICT`, or `GUARD_UNKNOWN`. Upgrade to a forward-compatible release before deploying against `1.0` servers.

### Server Behavior Changes Since 0.3.0

Clients integrating with `0.3.x` and `1.0+` servers should account for the following protocol and error semantics:

- **Idempotent retries (`applied`):** Retrying `APPLY_EVENT` or `CREATE_INSTANCE` with the same `idempotency_key` returns the original result with `applied: false`.
- **Validation & `initial_ctx` errors:** Validation failures (including non-object `initial_ctx` payloads and exceeded per-instance idempotency key limits) return non-retryable `BAD_REQUEST`.
- **Replica writes:** Followers/replicas run read-only and reject mutating operations with `READ_ONLY_MODE` (also `REPLICATION_TIMEOUT` and `REPLICATION_ERROR` for replication failures).
- **Upcoming 1.0 parity:** Reusing an `idempotency_key` with a different request fails with `IDEMPOTENCY_CONFLICT`, strict guard checks return `GUARD_UNKNOWN`, `BATCH` executes atomically, and `HELLO` advertises `durable_idempotency`.

## Protocol Integration

If an official client isn't available for your language, you can integrate directly with the RCP protocol.

### Protocol Overview

- **Transport:** TCP (port 7401)
- **Wire format:** Framed binary with JSON payload
- **TLS:** Optional
- **Authentication:** Bearer token

See [Protocol Documentation](/protocol/overview) for details.

### Implementation Steps

1. **Establish TCP connection**
2. **Send HELLO handshake**
3. **Authenticate (if required)**
4. **Send requests, receive responses**
5. **Handle subscriptions (if needed)**

### Frame Format

```
┌────────┬────────┬──────────┬──────────────────┐
│ Magic  │ Flags  │ CRC32C   │ JSON Payload     │
│ "RCP1" │ 4 bytes│ 4 bytes  │ variable         │
└────────┴────────┴──────────┴──────────────────┘
```

See [Wire Format](/protocol/wire-format) for complete specification.

### Example: Minimal Python Client

```python
import socket
import json
import struct
import crc32c

class RstmdbClient:
    MAGIC = b'RCP1'

    def __init__(self, host='127.0.0.1', port=7401):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.connect((host, port))
        self.request_id = 0
        self._handshake()

    def _handshake(self):
        response = self._send_request('HELLO', {
            'protocol_version': 1,
            'client_name': 'minimal-python',
            'client_version': '0.1.0'
        })
        if response['status'] != 'ok':
            raise Exception('Handshake failed')

    def _send_request(self, op, params=None):
        self.request_id += 1
        message = {
            'type': 'request',
            'id': str(self.request_id),
            'op': op,
        }
        if params:
            message['params'] = params

        payload = json.dumps(message).encode('utf-8')

        # Build frame
        flags = b'\x00\x00\x00\x00'
        length = struct.pack('>I', len(payload))
        checksum = struct.pack('>I', crc32c.crc32c(payload))

        frame = self.MAGIC + flags + length + checksum + payload
        self.sock.sendall(frame)

        return self._recv_response()

    def _recv_response(self):
        # Read header (16 bytes)
        header = self._recv_exact(16)
        magic = header[0:4]
        if magic != self.MAGIC:
            raise Exception('Invalid magic')

        length = struct.unpack('>I', header[8:12])[0]
        expected_crc = struct.unpack('>I', header[12:16])[0]

        # Read payload
        payload = self._recv_exact(length)
        actual_crc = crc32c.crc32c(payload)
        if actual_crc != expected_crc:
            raise Exception('CRC mismatch')

        return json.loads(payload)

    def _recv_exact(self, n):
        data = b''
        while len(data) < n:
            chunk = self.sock.recv(n - len(data))
            if not chunk:
                raise Exception('Connection closed')
            data += chunk
        return data

    def ping(self):
        return self._send_request('PING')

    def create_instance(self, machine, version, instance_id, context):
        return self._send_request('CREATE_INSTANCE', {
            'machine': machine,
            'version': version,
            'id': instance_id,
            'context': context
        })

    def apply_event(self, instance_id, event, payload=None):
        params = {
            'instance_id': instance_id,
            'event': event
        }
        if payload:
            params['payload'] = payload
        return self._send_request('APPLY_EVENT', params)

    def get_instance(self, instance_id):
        return self._send_request('GET_INSTANCE', {'id': instance_id})

# Usage
client = RstmdbClient()
print(client.ping())
client.create_instance('order', 1, 'order-001', {'customer': 'alice'})
client.apply_event('order-001', 'PAY', {'amount': 99.99})
print(client.get_instance('order-001'))
```

## HTTP Gateway (Coming Soon)

A REST/HTTP gateway is planned for environments where direct TCP isn't suitable:

```bash
# REST endpoint
curl -X POST http://localhost:8080/instances \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer my-token" \
  -d '{"machine": "order", "version": 1, "id": "order-001"}'

# Apply event
curl -X POST http://localhost:8080/instances/order-001/events \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer my-token" \
  -d '{"event": "PAY", "payload": {"amount": 99.99}}'
```

## gRPC (Planned)

A gRPC interface is planned for polyglot environments with strong typing requirements.

## Contributing

Interested in creating a client for your language? See the [Protocol Documentation](/protocol/overview) and reach out on GitHub.

Client requirements:
- Full protocol compliance
- Connection pooling
- TLS support
- Error handling
- Comprehensive tests
- Documentation
