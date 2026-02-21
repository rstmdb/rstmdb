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
rstmdb-client = "0.1"
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
