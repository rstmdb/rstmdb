---
sidebar_position: 6
---

# C# / .NET Client

The official C# client library for rstmdb.

**Repository:** [github.com/rstmdb/rstmdb-dotnet](https://github.com/rstmdb/rstmdb-dotnet)

## Installation

```bash
dotnet add package Rstmdb.Client
```

**Requirements:** .NET 7.0+

**Zero external dependencies** — System.Text.Json (built-in) and a built-in CRC32C implementation.

## Features

- Full RCP protocol: RCPX binary framing with CRC32C Castagnoli checksums
- All 22 operations and 16 error codes
- Request/response multiplexing on a single TCP connection
- Async subscription streaming via `System.Threading.Channels` and `IAsyncEnumerable`
- TLS/mTLS support via `SslStream`
- `async/await` with `CancellationToken` throughout
- `IAsyncDisposable` lifecycle management (`await using`)
- JSONL wire mode support

## Quick Start

```csharp
using Rstmdb.Client;

// Connect to server
await using var client = await RstmdbClient.ConnectAsync("localhost", 7401, new RstmdbOptions
{
    Auth = "my-secret-token",
});

// Define a state machine
await client.PutMachineAsync(new PutMachineRequest
{
    Machine = "order",
    Version = 1,
    Definition = new MachineDefinition
    {
        States = new[] { "pending", "paid", "shipped", "delivered" },
        Initial = "pending",
        Transitions = new[]
        {
            new Transition { From = new[] { "pending" }, Event = "PAY", To = "paid" },
            new Transition { From = new[] { "paid" }, Event = "SHIP", To = "shipped" },
            new Transition { From = new[] { "shipped" }, Event = "DELIVER", To = "delivered" },
        },
    },
});

// Create an instance
var inst = await client.CreateInstanceAsync(new CreateInstanceRequest
{
    Machine = "order",
    Version = 1,
    InstanceId = "order-001",
    InitialCtx = new Dictionary<string, object> { ["customer"] = "alice", ["total"] = 99.99 },
});
Console.WriteLine($"Created: {inst.InstanceId} in state {inst.State}");

// Apply events
var result = await client.ApplyEventAsync(new ApplyEventRequest
{
    InstanceId = "order-001",
    Event = "PAY",
    Payload = new Dictionary<string, object> { ["payment_id"] = "pay-123" },
});
Console.WriteLine($"Transitioned: {result.FromState} -> {result.ToState}");
```

## Connection

### Basic Connection

```csharp
await using var client = await RstmdbClient.ConnectAsync("localhost", 7401);
```

### With Authentication

```csharp
await using var client = await RstmdbClient.ConnectAsync("localhost", 7401, new RstmdbOptions
{
    Auth = "my-secret-token",
});
```

### TLS Connection

```csharp
await using var client = await RstmdbClient.ConnectAsync("secure.example.com", 7401, new RstmdbOptions
{
    Auth = "my-secret-token",
    Tls = RstmdbOptions.CreateTls(caFile: "ca.pem"),
});
```

### Mutual TLS (mTLS)

```csharp
await using var client = await RstmdbClient.ConnectAsync("secure.example.com", 7401, new RstmdbOptions
{
    Auth = "my-secret-token",
    Tls = RstmdbOptions.CreateTls(caFile: "ca.pem", certFile: "client.pem", keyFile: "client-key.pem"),
});
```

### Development Mode (Insecure)

```csharp
// Skip TLS verification - development only!
await using var client = await RstmdbClient.ConnectAsync("localhost", 7401, new RstmdbOptions
{
    Tls = RstmdbOptions.InsecureTls(),
});
```

## Configuration Options

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `Auth` | `string?` | `null` | Bearer token for authentication |
| `Tls` | `SslClientAuthenticationOptions?` | `null` | TLS configuration (null = plain TCP) |
| `ConnectTimeout` | `TimeSpan` | `10s` | Connection dial timeout |
| `RequestTimeout` | `TimeSpan` | `30s` | Per-request timeout |
| `ClientName` | `string?` | `null` | Client name sent in HELLO handshake |
| `WireMode` | `string` | `"binary_json"` | Wire mode: `"binary_json"` or `"jsonl"` |
| `Features` | `string[]?` | `null` | Feature negotiation hints |

## API Reference

### Machine Operations

#### PutMachine

Register a state machine definition.

```csharp
var result = await client.PutMachineAsync(new PutMachineRequest
{
    Machine = "order",
    Version = 1,
    Definition = new MachineDefinition
    {
        States = new[] { "pending", "paid", "shipped" },
        Initial = "pending",
        Transitions = new[]
        {
            new Transition { From = new[] { "pending" }, Event = "PAY", To = "paid" },
            new Transition { From = new[] { "paid" }, Event = "SHIP", To = "shipped" },
        },
    },
});
```

#### GetMachine

Retrieve a machine definition.

```csharp
var machine = await client.GetMachineAsync("order", 1);
Console.WriteLine(string.Join(", ", machine.Definition.States));
Console.WriteLine(machine.Definition.Initial);
```

#### ListMachines

List all machines.

```csharp
var machines = await client.ListMachinesAsync();
foreach (var m in machines)
{
    Console.WriteLine($"{m.Machine}: [{string.Join(", ", m.Versions)}]");
}
```

### Instance Operations

#### CreateInstance

Create a new instance.

```csharp
var inst = await client.CreateInstanceAsync(new CreateInstanceRequest
{
    Machine = "order",
    Version = 1,
    InstanceId = "order-001",
    InitialCtx = new Dictionary<string, object> { ["customer"] = "alice" },
});
```

#### GetInstance

Get instance state and context.

```csharp
var inst = await client.GetInstanceAsync("order-001");
Console.WriteLine($"State: {inst.State}");
Console.WriteLine($"Context: {inst.Ctx}");
```

#### ListInstances

List instances with optional filters.

```csharp
var list = await client.ListInstancesAsync(new ListInstancesOptions
{
    Machine = "order",
    State = "paid",
    Limit = 50,
});
foreach (var inst in list.Instances)
{
    Console.WriteLine($"{inst.Id}: {inst.State}");
}
```

#### DeleteInstance

Delete an instance.

```csharp
var result = await client.DeleteInstanceAsync("order-001");
```

### Event Operations

#### ApplyEvent

Apply an event to trigger a state transition.

```csharp
var result = await client.ApplyEventAsync(new ApplyEventRequest
{
    InstanceId = "order-001",
    Event = "PAY",
    Payload = new Dictionary<string, object> { ["amount"] = 99.99 },
});

Console.WriteLine($"From: {result.FromState}");
Console.WriteLine($"To: {result.ToState}");
```

With optimistic concurrency:

```csharp
var result = await client.ApplyEventAsync(new ApplyEventRequest
{
    InstanceId = "order-001",
    Event = "PAY",
    ExpectedState = "pending",
});
```

#### Batch

Execute multiple operations in a single request.

```csharp
var results = await client.BatchAsync(BatchMode.Atomic, new[]
{
    BatchOperation.CreateInstance(new CreateInstanceRequest
    {
        Machine = "order", Version = 1, InstanceId = "order-002",
    }),
    BatchOperation.ApplyEvent(new ApplyEventRequest
    {
        InstanceId = "order-002", Event = "PAY",
    }),
});

foreach (var r in results)
{
    Console.WriteLine($"status={r.Status}");
}
```

### Streaming

#### WatchAll

Subscribe to events with filtering.

```csharp
await using var sub = await client.WatchAllAsync(new WatchAllOptions
{
    Machines = new[] { "order" },
    ToStates = new[] { "shipped", "delivered" },
});

await foreach (var evt in sub.ReadAllAsync(cancellationToken))
{
    Console.WriteLine($"{evt.InstanceId}: {evt.Event} -> {evt.ToState}");
}
```

Or read from the channel directly:

```csharp
while (await sub.Events.WaitToReadAsync(cancellationToken))
{
    while (sub.Events.TryRead(out var evt))
    {
        Console.WriteLine($"{evt.InstanceId}: {evt.Event} -> {evt.ToState}");
    }
}
```

#### WatchInstance

Watch a specific instance.

```csharp
await using var sub = await client.WatchInstanceAsync(new WatchInstanceRequest
{
    InstanceId = "order-001",
    IncludeCtx = true,
});

await foreach (var evt in sub.ReadAllAsync(cancellationToken))
{
    Console.WriteLine($"Event: {evt.Event}, New state: {evt.ToState}");
}
```

### System Operations

#### Ping

Health check.

```csharp
await client.PingAsync();
```

#### Info

Get server information.

```csharp
var info = await client.GetInfoAsync();
Console.WriteLine($"Server: {info.ServerName} {info.ServerVersion}");
Console.WriteLine($"Features: {string.Join(", ", info.Features ?? Array.Empty<string>())}");
```

### WAL Operations

#### WalRead

Read entries from the write-ahead log.

```csharp
var result = await client.WalReadAsync(0, limit: 100);
foreach (var record in result.Records)
{
    Console.WriteLine($"offset={record.Offset} entry={record.Entry}");
}
```

#### WalStats

Get WAL statistics.

```csharp
var stats = await client.WalStatsAsync();
Console.WriteLine($"Entries: {stats.EntryCount}, Size: {stats.TotalSizeBytes} bytes");
```

#### Compact

Trigger WAL compaction.

```csharp
var result = await client.CompactAsync(forceSnapshot: false);
Console.WriteLine($"Reclaimed: {result.BytesReclaimed} bytes");
```

#### SnapshotInstance

Create a point-in-time snapshot.

```csharp
var snap = await client.SnapshotInstanceAsync("order-001");
Console.WriteLine($"Snapshot: {snap.SnapshotId} at offset {snap.WalOffset}");
```

## Error Handling

```csharp
try
{
    var result = await client.ApplyEventAsync(req);
}
catch (RstmdbException ex) when (RstmdbException.IsInstanceNotFound(ex))
{
    Console.WriteLine("Instance not found");
}
catch (RstmdbException ex) when (RstmdbException.IsInvalidTransition(ex))
{
    Console.WriteLine($"Cannot apply event from current state: {ex.Message}");
}
catch (RstmdbException ex) when (RstmdbException.IsConflict(ex))
{
    Console.WriteLine("Optimistic concurrency conflict");
}
catch (RstmdbException ex) when (ex.IsRetryable)
{
    Console.WriteLine("Transient error, safe to retry");
}
```

Error codes: `UNSUPPORTED_PROTOCOL`, `BAD_REQUEST`, `UNAUTHORIZED`, `AUTH_FAILED`, `NOT_FOUND`, `MACHINE_NOT_FOUND`, `MACHINE_VERSION_EXISTS`, `MACHINE_VERSION_LIMIT_EXCEEDED`, `INSTANCE_NOT_FOUND`, `INSTANCE_EXISTS`, `INVALID_TRANSITION`, `GUARD_FAILED`, `CONFLICT`, `WAL_IO_ERROR`, `INTERNAL_ERROR`, `RATE_LIMITED`.

## Examples

### Order Processing

```csharp
using Rstmdb.Client;

async Task ProcessOrder(RstmdbClient client, string orderId)
{
    // Create order
    await client.CreateInstanceAsync(new CreateInstanceRequest
    {
        Machine = "order",
        Version = 1,
        InstanceId = orderId,
        InitialCtx = new Dictionary<string, object>
        {
            ["items"] = new[] { "item-1", "item-2" },
            ["total"] = 149.99,
        },
    });

    // Process payment
    await client.ApplyEventAsync(new ApplyEventRequest
    {
        InstanceId = orderId,
        Event = "PAY",
        Payload = new Dictionary<string, object> { ["payment_id"] = "pay-123" },
    });

    // Ship order
    await client.ApplyEventAsync(new ApplyEventRequest
    {
        InstanceId = orderId,
        Event = "SHIP",
        Payload = new Dictionary<string, object> { ["tracking"] = "1Z999" },
    });

    // Get final state
    var order = await client.GetInstanceAsync(orderId);
    Console.WriteLine($"Order {orderId} is now: {order.State}");
}

await using var client = await RstmdbClient.ConnectAsync("localhost", 7401);
await ProcessOrder(client, "order-001");
```

### Event Consumer

```csharp
using Rstmdb.Client;

// Graceful shutdown via Ctrl+C
using var cts = new CancellationTokenSource();
Console.CancelKeyPress += (_, e) => { e.Cancel = true; cts.Cancel(); };

await using var client = await RstmdbClient.ConnectAsync("localhost", 7401);
Console.WriteLine("Listening for shipped orders...");

await using var sub = await client.WatchAllAsync(new WatchAllOptions
{
    Machines = new[] { "order" },
    ToStates = new[] { "shipped" },
});

try
{
    await foreach (var evt in sub.ReadAllAsync(cts.Token))
    {
        Console.WriteLine($"Order {evt.InstanceId} shipped!");
        // Send notification, update external system, etc.
    }
}
catch (OperationCanceledException)
{
    Console.WriteLine("Stopped.");
}
```

## Resources

- [GitHub Repository](https://github.com/rstmdb/rstmdb-dotnet)
- [NuGet Package](https://www.nuget.org/packages/Rstmdb.Client)
