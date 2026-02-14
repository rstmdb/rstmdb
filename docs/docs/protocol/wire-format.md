---
sidebar_position: 2
---

# Wire Format

This document describes the binary wire format used by RCP (rstmdb Command Protocol).

## Frame Structure

Every message is wrapped in a frame:

```
┌─────────────────────────────────────────────────────────────────┐
│                          Frame Header                            │
├──────────┬──────────┬──────────┬──────────┬─────────────────────┤
│  Magic   │  Flags   │ Reserved │  Length  │ CRC32C   │ Payload  │
│  4 bytes │  1 byte  │  3 bytes │  4 bytes │ 4 bytes  │ variable │
└──────────┴──────────┴──────────┴──────────┴──────────┴──────────┘
    0-3        4          5-7        8-11      12-15      16+
```

### Header Fields

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 4 | magic | `"RCP1"` (0x52435031) |
| 4 | 1 | flags | Frame flags |
| 5 | 3 | reserved | Must be 0x000000 |
| 8 | 4 | length | Payload length (big-endian) |
| 12 | 4 | crc32c | CRC32C of payload (big-endian) |
| 16 | var | payload | JSON message |

### Magic Bytes

The magic bytes identify the protocol:

```
R  C  P  1
52 43 50 31  (ASCII hex)
```

If a frame doesn't start with `RCP1`, the connection should be closed.

### Flags

```
Bit   Description
───────────────────
0     Compressed (reserved, must be 0)
1-7   Reserved (must be 0)
```

Currently no flags are defined. All bits must be 0.

### Length

The payload length is encoded as a 4-byte big-endian unsigned integer.

Maximum payload size: 16 MiB (16,777,216 bytes)

### CRC32C Checksum

The CRC32C (Castagnoli) checksum is computed over the payload bytes only.

```rust
let checksum = crc32c::crc32c(&payload);
```

On receive:
1. Read the header
2. Read `length` bytes of payload
3. Compute CRC32C of payload
4. Compare with header checksum
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
52 43 50 31     # Magic: "RCP1"
00              # Flags: none
00 00 00        # Reserved
00 00 00 26     # Length: 38 bytes
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

JSONL mode is negotiated during handshake or via configuration:

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
2. If magic != "RCP1", close connection
3. Read 12 bytes (rest of header)
4. Extract length from bytes 8-11
5. If length > 16 MiB, close connection
6. Read `length` bytes (payload)
7. Compute CRC32C of payload
8. If CRC != header CRC, close connection
9. Parse payload as JSON
10. Process message
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
- Read buffer: 64 KiB
- Write buffer: 64 KiB
- Max in-flight requests: 1000

### TCP Settings

Recommended TCP socket options:
- `TCP_NODELAY`: enabled (reduce latency)
- `SO_KEEPALIVE`: enabled (detect dead connections)
