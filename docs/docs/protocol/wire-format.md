---
sidebar_position: 2
---

# Wire Format

This document describes the binary wire format used by RCP (rstmdb Command Protocol).

## Frame Structure

Every message is wrapped in a frame with an **18-byte fixed header**:

```
┌──────────────────────────────────────────────────────────────────────────┐
│                             Frame Header (18 bytes)                       │
├──────────┬──────────┬──────────┬────────────┬─────────────┬─────────────┤
│  Magic   │ Version  │  Flags   │ Header Len │ Payload Len │   CRC32C    │
│  4 bytes │  2 bytes │  2 bytes │   2 bytes  │   4 bytes   │   4 bytes   │
├──────────┴──────────┴──────────┴────────────┴─────────────┴─────────────┤
│ [Header Extension]  │  Payload (JSON)                                    │
│   header_len bytes  │  payload_len bytes                                 │
└─────────────────────┴────────────────────────────────────────────────────┘
     0-3      4-5        6-7         8-9          10-13         14-17
```

### Header Fields

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | magic | `"RCPX"` (0x52435058) |
| 4 | 2 | version | Protocol version, big-endian (current: `1`) |
| 6 | 2 | flags | Frame flags, big-endian (see below) |
| 8 | 2 | header_len | Header extension length, big-endian |
| 10 | 4 | payload_len | Payload length, big-endian |
| 14 | 4 | crc32c | CRC32C of payload, big-endian |
| 18 | var | header_ext | Reserved for future use (header_len bytes) |
| 18+header_len | var | payload | UTF-8 JSON message |

### Magic Bytes

The magic bytes identify the protocol:

```
R  C  P  X
52 43 50 58  (ASCII hex)
```

If a frame doesn't start with `RCPX`, the connection should be closed.

### Version

Protocol version as a 2-byte big-endian unsigned integer. Current version: `1` (`0x0001`).

### Flags

2-byte big-endian bitfield:

```
Bit   Mask     Name           Description
──────────────────────────────────────────────
0     0x0001   CRC_PRESENT    CRC32C checksum is present and must be validated
1     0x0002   COMPRESSED     Payload is compressed (reserved, not yet used)
2     0x0004   STREAM         Frame is part of a stream
3     0x0008   END_STREAM     Final frame of a stream
4-15  —        Reserved       Must be zero for protocol v1
```

Valid flag mask for v1: `0x000F`. Frames with bits outside this mask set must be rejected.

### Header Extension

The `header_len` field specifies the length of an optional header extension area between the fixed header and the payload. Currently unused (should be 0 in v1). Clients must skip `header_len` bytes before reading the payload.

### Payload Length

The payload length is encoded as a 4-byte big-endian unsigned integer.

Maximum payload size: 16 MiB (16,777,216 bytes)

### CRC32C Checksum

The CRC32C (Castagnoli) checksum is computed over the payload bytes only.

```rust
let checksum = crc32c::crc32c(&payload);
```

The checksum is only validated when the `CRC_PRESENT` flag (bit 0) is set.

On receive:
1. Read the fixed header (18 bytes)
2. Skip `header_len` bytes (header extension)
3. Read `payload_len` bytes of payload
4. If `CRC_PRESENT` flag is set, compute CRC32C of payload and compare with header checksum
5. Reject frame if mismatch

## Byte Order

All multi-byte integers are **big-endian** (network byte order).

## Example Frame

Request to ping the server:

```json
{"type":"request","id":"1","op":"PING"}
```

Wire format (hex):

```
52 43 50 58     # Magic: "RCPX"
00 01           # Version: 1
00 01           # Flags: CRC_PRESENT
00 00           # Header extension length: 0
00 00 00 26     # Payload length: 38 bytes
A1 B2 C3 D4     # CRC32C (example)
7B 22 74 79...  # Payload: JSON bytes
```

## JSONL Mode

For debugging, JSONL (JSON Lines) mode uses newline-delimited JSON:

```
{"type":"request","id":"1","op":"PING"}\n
{"type":"response","id":"1","status":"ok"}\n
```

### JSONL Rules

- Each message is a single line of JSON
- Lines are terminated with `\n` (0x0A)
- No framing header or CRC
- Max line length: 16 MiB

### Switching to JSONL

JSONL mode is negotiated during the HELLO handshake. The client sends preferred `wire_modes` and the server picks the first supported mode:

```json
{
  "op": "HELLO",
  "params": {
    "protocol_version": 1,
    "wire_modes": ["binary_json", "jsonl"]
  }
}
```

**Server config:**
```yaml
network:
  wire_mode: jsonl
```

**CLI flag:**
```bash
rstmdb-cli --wire-mode jsonl ping
```

## Parsing Algorithm

### Binary Mode

```
1. Read 4 bytes (magic)
2. If magic != "RCPX", close connection
3. Read 14 bytes (rest of fixed header)
4. Extract version from bytes 4-5
5. If version != 1, close connection (UNSUPPORTED_PROTOCOL)
6. Extract flags from bytes 6-7
7. If flags & ~0x000F != 0, close connection (invalid flags)
8. Extract header_len from bytes 8-9
9. Extract payload_len from bytes 10-13
10. If payload_len > 16 MiB, close connection
11. Skip header_len bytes (header extension)
12. Read payload_len bytes (payload)
13. If flags & CRC_PRESENT:
    a. Compute CRC32C of payload
    b. If CRC != header CRC, close connection
14. Parse payload as UTF-8 JSON
15. Process message
```

### JSONL Mode

```
1. Read until newline
2. If line length > 16 MiB, close connection
3. Parse line as JSON
4. Process message
```

## Error Handling

### Invalid Magic

Close the connection immediately. This indicates either:
- Wrong protocol
- Connection to wrong port
- Data corruption

### Unsupported Version

Close the connection with `UNSUPPORTED_PROTOCOL` error.

### Invalid Flags

Close the connection. Unknown flag bits may indicate a newer protocol version.

### CRC Mismatch

Close the connection. The data is corrupted and cannot be trusted.

### Oversized Frame

Close the connection. This protects against memory exhaustion attacks.

### Invalid JSON

Send an error response if possible, then close:

```json
{
  "type": "response",
  "id": null,
  "status": "error",
  "error": {
    "code": "BAD_REQUEST",
    "message": "Invalid JSON in request"
  }
}
```

## Performance Considerations

### CRC32C Hardware Acceleration

CRC32C (Castagnoli polynomial) is hardware-accelerated on:
- x86_64 with SSE4.2
- ARM with CRC instructions

The `crc32c` crate automatically uses hardware acceleration when available.

### Buffer Sizes

Recommended buffer sizes:
- Read buffer: 8 KiB (default)
- Write buffer: 64 KiB
- Max in-flight requests: 1000

### TCP Settings

Recommended TCP socket options:
- `TCP_NODELAY`: enabled (reduce latency)
- `SO_KEEPALIVE`: enabled (detect dead connections)
