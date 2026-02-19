---
sidebar_position: 5
---

# Go Client

The official Go client library for rstmdb.

**Repository:** [github.com/rstmdb/rstmdb-go](https://github.com/rstmdb/rstmdb-go)

## Installation

```bash
go get github.com/rstmdb/rstmdb-go
```

**Requirements:** Go 1.21+

**Zero external dependencies** — standard library only.

## Features

- Full RCP protocol: RCPX binary framing with CRC32C checksums
- All 22 operations and 16 error codes
- Request/response multiplexing on a single TCP connection
- Async subscription streaming via channels
- TLS/mTLS support
- Context-based cancellation and timeouts
- Struct-based configuration (no functional options boilerplate)
- JSONL wire mode support

## Quick Start

```go
package main

import (
    "context"
    "fmt"
    "log"

    rstmdb "github.com/rstmdb/rstmdb-go"
)

func main() {
    ctx := context.Background()

    // Connect to server
    client, err := rstmdb.Connect(ctx, "localhost:7401", &rstmdb.Options{
        Auth: "my-secret-token",
    })
    if err != nil {
        log.Fatal(err)
    }
    defer client.Close()

    // Define a state machine
    _, err = client.PutMachine(ctx, rstmdb.PutMachineRequest{
        Machine: "order",
        Version: 1,
        Definition: rstmdb.MachineDefinition{
            States:  []string{"pending", "paid", "shipped", "delivered"},
            Initial: "pending",
            Transitions: []rstmdb.Transition{
                {From: rstmdb.StringOrSlice{"pending"}, Event: "PAY", To: "paid"},
                {From: rstmdb.StringOrSlice{"paid"}, Event: "SHIP", To: "shipped"},
                {From: rstmdb.StringOrSlice{"shipped"}, Event: "DELIVER", To: "delivered"},
            },
        },
    })
    if err != nil {
        log.Fatal(err)
    }

    // Create an instance
    inst, err := client.CreateInstance(ctx, rstmdb.CreateInstanceRequest{
        Machine:    "order",
        Version:    1,
        InstanceID: "order-001",
        InitialCtx: map[string]any{"customer": "alice", "total": 99.99},
    })
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("Created: %s in state %s\n", inst.InstanceID, inst.State)

    // Apply events
    result, err := client.ApplyEvent(ctx, rstmdb.ApplyEventRequest{
        InstanceID: "order-001",
        Event:      "PAY",
        Payload:    map[string]any{"payment_id": "pay-123"},
    })
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("Transitioned: %s -> %s\n", result.FromState, result.ToState)
}
```

## Connection

### Basic Connection

```go
client, err := rstmdb.Connect(ctx, "localhost:7401", nil)
```

### With Authentication

```go
client, err := rstmdb.Connect(ctx, "localhost:7401", &rstmdb.Options{
    Auth: "my-secret-token",
})
```

### TLS Connection

```go
tlsCfg, err := rstmdb.TLSFromFiles("ca.pem", "", "")
if err != nil {
    log.Fatal(err)
}

client, err := rstmdb.Connect(ctx, "secure.example.com:7401", &rstmdb.Options{
    Auth: "my-secret-token",
    TLS:  tlsCfg,
})
```

### Mutual TLS (mTLS)

```go
tlsCfg, err := rstmdb.TLSFromFiles("ca.pem", "client.pem", "client-key.pem")
if err != nil {
    log.Fatal(err)
}

client, err := rstmdb.Connect(ctx, "secure.example.com:7401", &rstmdb.Options{
    Auth: "my-secret-token",
    TLS:  tlsCfg,
})
```

### Development Mode (Insecure)

```go
// Skip TLS verification - development only!
client, err := rstmdb.Connect(ctx, "localhost:7401", &rstmdb.Options{
    TLS: rstmdb.InsecureTLS(),
})
```

## Configuration Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `Auth` | `string` | `""` | Bearer token for authentication |
| `TLS` | `*tls.Config` | `nil` | TLS configuration (nil = plain TCP) |
| `Timeout` | `time.Duration` | `10s` | Connection dial timeout |
| `RequestTimeout` | `time.Duration` | `30s` | Per-request timeout |
| `ClientName` | `string` | `""` | Client name sent in HELLO handshake |
| `WireMode` | `string` | `"binary_json"` | Wire mode: `"binary_json"` or `"jsonl"` |
| `Features` | `[]string` | `nil` | Feature negotiation hints |

## API Reference

### Machine Operations

#### PutMachine

Register a state machine definition.

```go
result, err := client.PutMachine(ctx, rstmdb.PutMachineRequest{
    Machine: "order",
    Version: 1,
    Definition: rstmdb.MachineDefinition{
        States:  []string{"pending", "paid", "shipped"},
        Initial: "pending",
        Transitions: []rstmdb.Transition{
            {From: rstmdb.StringOrSlice{"pending"}, Event: "PAY", To: "paid"},
            {From: rstmdb.StringOrSlice{"paid"}, Event: "SHIP", To: "shipped"},
        },
    },
})
```

#### GetMachine

Retrieve a machine definition.

```go
machine, err := client.GetMachine(ctx, "order", 1)
fmt.Println(machine.Definition.States)
fmt.Println(machine.Definition.Initial)
```

#### ListMachines

List all machines.

```go
machines, err := client.ListMachines(ctx)
for _, m := range machines {
    fmt.Printf("%s: %v\n", m.Machine, m.Versions)
}
```

### Instance Operations

#### CreateInstance

Create a new instance.

```go
inst, err := client.CreateInstance(ctx, rstmdb.CreateInstanceRequest{
    Machine:    "order",
    Version:    1,
    InstanceID: "order-001",
    InitialCtx: map[string]any{"customer": "alice"},
})
```

#### GetInstance

Get instance state and context.

```go
inst, err := client.GetInstance(ctx, "order-001")
fmt.Printf("State: %s\n", inst.State)
fmt.Printf("Context: %v\n", inst.Ctx)
```

#### ListInstances

List instances with optional filters.

```go
list, err := client.ListInstances(ctx,
    rstmdb.WithMachine("order"),
    rstmdb.WithState("paid"),
    rstmdb.WithLimit(50),
)
for _, inst := range list.Instances {
    fmt.Printf("%s: %s\n", inst.ID, inst.State)
}
```

#### DeleteInstance

Delete an instance.

```go
result, err := client.DeleteInstance(ctx, "order-001")
```

### Event Operations

#### ApplyEvent

Apply an event to trigger a state transition.

```go
result, err := client.ApplyEvent(ctx, rstmdb.ApplyEventRequest{
    InstanceID: "order-001",
    Event:      "PAY",
    Payload:    map[string]any{"amount": 99.99},
})

fmt.Printf("From: %s\n", result.FromState)
fmt.Printf("To: %s\n", result.ToState)
```

With optimistic concurrency:

```go
result, err := client.ApplyEvent(ctx, rstmdb.ApplyEventRequest{
    InstanceID:    "order-001",
    Event:         "PAY",
    ExpectedState: "pending",
})
```

#### Batch

Execute multiple operations in a single request.

```go
results, err := client.Batch(ctx, rstmdb.BatchAtomic, []rstmdb.BatchOperation{
    rstmdb.BatchCreateInstance(rstmdb.CreateInstanceRequest{
        Machine: "order", Version: 1, InstanceID: "order-002",
    }),
    rstmdb.BatchApplyEvent(rstmdb.ApplyEventRequest{
        InstanceID: "order-002", Event: "PAY",
    }),
})

for _, r := range results {
    fmt.Printf("status=%s\n", r.Status)
}
```

### Streaming

#### WatchAll

Subscribe to events with filtering.

```go
sub, err := client.WatchAll(ctx,
    rstmdb.WatchMachines("order"),
    rstmdb.WatchToStates("shipped", "delivered"),
)
if err != nil {
    log.Fatal(err)
}
defer sub.Close()

for {
    select {
    case event, ok := <-sub.Events:
        if !ok {
            return
        }
        fmt.Printf("%s: %s -> %s\n", event.InstanceID, event.EventName, event.ToState)
    case err, ok := <-sub.Errors:
        if !ok {
            return
        }
        log.Printf("watch error: %v", err)
    case <-ctx.Done():
        return
    }
}
```

#### WatchInstance

Watch a specific instance.

```go
sub, err := client.WatchInstance(ctx, rstmdb.WatchInstanceRequest{
    InstanceID: "order-001",
    IncludeCtx: true,
})
if err != nil {
    log.Fatal(err)
}
defer sub.Close()

for event := range sub.Events {
    fmt.Printf("Event: %s, New state: %s\n", event.EventName, event.ToState)
}
```

### System Operations

#### Ping

Health check.

```go
err := client.Ping(ctx)
```

#### Info

Get server information.

```go
info, err := client.Info(ctx)
fmt.Printf("Server: %s %s\n", info.ServerName, info.ServerVersion)
fmt.Printf("Features: %v\n", info.Features)
```

### WAL Operations

#### WALRead

Read entries from the write-ahead log.

```go
result, err := client.WALRead(ctx, 0, rstmdb.WALLimit(100))
for _, record := range result.Records {
    fmt.Printf("offset=%d entry=%v\n", record.Offset, record.Entry)
}
```

#### WALStats

Get WAL statistics.

```go
stats, err := client.WALStats(ctx)
fmt.Printf("Entries: %d, Size: %d bytes\n", stats.EntryCount, stats.TotalSize)
```

#### Compact

Trigger WAL compaction.

```go
result, err := client.Compact(ctx, false)
fmt.Printf("Reclaimed: %d bytes\n", result.BytesReclaimed)
```

#### SnapshotInstance

Create a point-in-time snapshot.

```go
snap, err := client.SnapshotInstance(ctx, "order-001")
fmt.Printf("Snapshot: %s at offset %d\n", snap.SnapshotID, snap.WALOffset)
```

## Error Handling

```go
import "errors"

result, err := client.ApplyEvent(ctx, req)
if err != nil {
    var rstmdbErr *rstmdb.Error
    if errors.As(err, &rstmdbErr) {
        switch {
        case rstmdb.IsInstanceNotFound(err):
            fmt.Println("Instance not found")
        case rstmdb.IsInvalidTransition(err):
            fmt.Printf("Cannot apply event from current state: %s\n", rstmdbErr.Message)
        case rstmdb.IsConflict(err):
            fmt.Println("Optimistic concurrency conflict")
        case rstmdb.IsRetryable(err):
            fmt.Println("Transient error, safe to retry")
        }
    }
    log.Fatal(err)
}
```

Error codes: `UNSUPPORTED_PROTOCOL`, `BAD_REQUEST`, `UNAUTHORIZED`, `AUTH_FAILED`, `NOT_FOUND`, `MACHINE_NOT_FOUND`, `MACHINE_VERSION_EXISTS`, `MACHINE_VERSION_LIMIT_EXCEEDED`, `INSTANCE_NOT_FOUND`, `INSTANCE_EXISTS`, `INVALID_TRANSITION`, `GUARD_FAILED`, `CONFLICT`, `WAL_IO_ERROR`, `INTERNAL_ERROR`, `RATE_LIMITED`.

## Examples

### Order Processing

```go
package main

import (
    "context"
    "fmt"
    "log"

    rstmdb "github.com/rstmdb/rstmdb-go"
)

func processOrder(ctx context.Context, client *rstmdb.Client, orderID string) {
    // Create order
    client.CreateInstance(ctx, rstmdb.CreateInstanceRequest{
        Machine:    "order",
        Version:    1,
        InstanceID: orderID,
        InitialCtx: map[string]any{"items": []string{"item-1", "item-2"}, "total": 149.99},
    })

    // Process payment
    client.ApplyEvent(ctx, rstmdb.ApplyEventRequest{
        InstanceID: orderID,
        Event:      "PAY",
        Payload:    map[string]any{"payment_id": "pay-123"},
    })

    // Ship order
    client.ApplyEvent(ctx, rstmdb.ApplyEventRequest{
        InstanceID: orderID,
        Event:      "SHIP",
        Payload:    map[string]any{"tracking": "1Z999"},
    })

    // Get final state
    order, _ := client.GetInstance(ctx, orderID)
    fmt.Printf("Order %s is now: %s\n", orderID, order.State)
}

func main() {
    ctx := context.Background()
    client, err := rstmdb.Connect(ctx, "localhost:7401", nil)
    if err != nil {
        log.Fatal(err)
    }
    defer client.Close()

    processOrder(ctx, client, "order-001")
}
```

### Event Consumer

```go
package main

import (
    "context"
    "fmt"
    "log"
    "os"
    "os/signal"

    rstmdb "github.com/rstmdb/rstmdb-go"
)

func main() {
    ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
    defer cancel()

    client, err := rstmdb.Connect(ctx, "localhost:7401", nil)
    if err != nil {
        log.Fatal(err)
    }
    defer client.Close()

    fmt.Println("Listening for shipped orders...")

    sub, err := client.WatchAll(ctx,
        rstmdb.WatchMachines("order"),
        rstmdb.WatchToStates("shipped"),
    )
    if err != nil {
        log.Fatal(err)
    }
    defer sub.Close()

    for {
        select {
        case event, ok := <-sub.Events:
            if !ok {
                return
            }
            fmt.Printf("Order %s shipped!\n", event.InstanceID)
            // Send notification, update external system, etc.
        case err, ok := <-sub.Errors:
            if !ok {
                return
            }
            log.Printf("error: %v", err)
        case <-ctx.Done():
            return
        }
    }
}
```

## Resources

- [GitHub Repository](https://github.com/rstmdb/rstmdb-go)
- [pkg.go.dev Reference](https://pkg.go.dev/github.com/rstmdb/rstmdb-go)
