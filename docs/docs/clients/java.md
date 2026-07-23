---
sidebar_position: 7
---

# Java Client

The official Java client library for rstmdb.

**Repository:** [github.com/rstmdb/rstmdb-java](https://github.com/rstmdb/rstmdb-java)

## Installation

### Gradle (Kotlin DSL)

```kotlin
dependencies {
    implementation("com.rstmdb:rstmdb-client:0.1.0")
}
```

### Gradle (Groovy)

```groovy
dependencies {
    implementation 'com.rstmdb:rstmdb-client:0.1.0'
}
```

### Maven

```xml
<dependency>
    <groupId>com.rstmdb</groupId>
    <artifactId>rstmdb-client</artifactId>
    <version>0.1.0</version>
</dependency>
```

**Requirements:** Java 11+

## Features

- Full RCP protocol: RCPX binary framing with CRC32C Castagnoli checksums
- Both async (`CompletableFuture<T>`) and sync (`*Sync`) APIs for all operations
- All 22 operations and 16 error codes
- TLS/mTLS support via `SSLContext`
- Streaming subscriptions via `Iterable`
- Builder-pattern configuration with `RstmdbOptions`
- Testcontainers module for integration testing

## Quick Start

```java
import com.rstmdb.client.*;
import com.rstmdb.client.model.*;
import java.util.List;
import java.util.Map;

try (var client = RstmdbClient.connect("localhost", 7401)) {
    // Define a state machine
    client.putMachineSync(new PutMachineRequest(
        "order", 1,
        new MachineDefinition(
            List.of("pending", "paid", "shipped", "delivered"),
            "pending",
            List.of(
                new Transition(List.of("pending"), "PAY", "paid", null),
                new Transition(List.of("paid"), "SHIP", "shipped", null),
                new Transition(List.of("shipped"), "DELIVER", "delivered", null)
            ),
            null
        ),
        null
    ));

    // Create an instance
    var inst = client.createInstanceSync(new CreateInstanceRequest(
        "order-001", "order", 1,
        Map.of("customer", "alice", "total", 99.99),
        null
    ));
    System.out.println("Created: " + inst.getInstanceId() + " in state " + inst.getState());

    // Apply events
    var result = client.applyEventSync(new ApplyEventRequest(
        "order-001", "PAY",
        Map.of("payment_id", "pay-123"),
        null, null, null, null
    ));
    System.out.println(result.getFromState() + " -> " + result.getToState());
}
```

## Connection

### Basic Connection

```java
var client = RstmdbClient.connect("localhost", 7401);
```

### With Authentication

```java
var opts = RstmdbOptions.builder()
    .auth("my-secret-token")
    .build();

var client = RstmdbClient.connect("localhost", 7401, opts);
```

### TLS Connection

```java
var opts = RstmdbOptions.builder()
    .auth("my-secret-token")
    .sslContext(RstmdbOptions.createTlsContext(Path.of("ca.pem")))
    .build();

var client = RstmdbClient.connect("secure.example.com", 7401, opts);
```

### Development Mode (Insecure)

```java
// Skip TLS verification - development only!
var opts = RstmdbOptions.builder()
    .sslContext(RstmdbOptions.insecureTlsContext())
    .build();

var client = RstmdbClient.connect("localhost", 7401, opts);
```

## Configuration Options

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `auth` | `String` | `null` | Bearer token for authentication |
| `sslContext` | `SSLContext` | `null` | TLS configuration (null = plain TCP) |
| `connectTimeout` | `Duration` | `10s` | Connection dial timeout |
| `requestTimeout` | `Duration` | `30s` | Per-request timeout |
| `clientName` | `String` | `null` | Client name sent in HELLO handshake |

## API Reference

All operations are available as both async (`CompletableFuture<T>`) and sync (`*Sync`) methods.

### Machine Operations

#### PutMachine

Register a state machine definition.

```java
var result = client.putMachineSync(new PutMachineRequest(
    "order", 1,
    new MachineDefinition(
        List.of("pending", "paid", "shipped"),
        "pending",
        List.of(
            new Transition(List.of("pending"), "PAY", "paid", null),
            new Transition(List.of("paid"), "SHIP", "shipped", null)
        ),
        null
    ),
    null
));
```

#### GetMachine

Retrieve a machine definition.

```java
var machine = client.getMachineSync("order", 1);
System.out.println("Initial: " + machine.getDefinition().getInitial());
```

#### ListMachines

List all machines.

```java
var machines = client.listMachinesSync();
for (var m : machines.getItems()) {
    System.out.println(m.getMachine() + ": " + m.getVersions());
}
```

### Instance Operations

#### CreateInstance

Create a new instance.

```java
var inst = client.createInstanceSync(new CreateInstanceRequest(
    "order-001", "order", 1,
    Map.of("customer", "alice"),
    null
));
```

#### GetInstance

Get instance state and context.

```java
var inst = client.getInstanceSync("order-001");
System.out.println("State: " + inst.getState());
System.out.println("Context: " + inst.getCtx());
```

#### ListInstances

List instances with optional filters.

```java
var list = client.listInstancesSync(
    ListInstancesOptions.builder()
        .machine("order")
        .state("paid")
        .limit(50)
        .build()
);
for (var inst : list.getInstances()) {
    System.out.println(inst.getId() + ": " + inst.getState());
}
```

#### DeleteInstance

Delete an instance.

```java
var result = client.deleteInstanceSync("order-001");
System.out.println("Deleted: " + result.isDeleted());
```

### Event Operations

#### ApplyEvent

Apply an event to trigger a state transition.

```java
var result = client.applyEventSync(new ApplyEventRequest(
    "order-001", "PAY",
    Map.of("amount", 99.99),
    null, null, null, null
));

System.out.println("From: " + result.getFromState());
System.out.println("To: " + result.getToState());
```

With optimistic concurrency:

```java
var result = client.applyEventSync(new ApplyEventRequest(
    "order-001", "PAY",
    Map.of("amount", 99.99),
    "pending",  // expectedState
    null, null, null
));
```

#### Batch

Execute multiple operations in a single request.

```java
var results = client.batchSync(BatchMode.ATOMIC, List.of(
    BatchOperation.createInstance(new CreateInstanceRequest(
        "order-002", "order", 1, Map.of(), null
    )),
    BatchOperation.applyEvent(new ApplyEventRequest(
        "order-002", "PAY", Map.of(), null, null, null, null
    ))
));

for (var r : results) {
    System.out.println("status=" + r.getStatus());
}
```

### Streaming

#### WatchAll

Subscribe to events with filtering.

```java
var sub = client.watchAllSync(new WatchAllOptions(
    true, null, new String[]{"order"}, null, null, null
));

for (var event : sub.events()) {
    System.out.printf("%s: %s -> %s (event: %s)%n",
        event.getInstanceId(), event.getFromState(),
        event.getToState(), event.getEvent());
}
```

#### WatchInstance

Watch a specific instance.

```java
var sub = client.watchInstanceSync("order-001", true);

for (var event : sub.events()) {
    System.out.printf("Event: %s, New state: %s%n",
        event.getEvent(), event.getToState());
}
```

### System Operations

#### Ping

Health check.

```java
client.pingSync();
```

#### Info

Get server information.

```java
var info = client.getInfoSync();
System.out.println("Server: " + info.getServerName() + " " + info.getServerVersion());
```

### WAL Operations

#### WalRead

Read entries from the write-ahead log.

```java
var result = client.walReadSync(0, 100);
for (var record : result.getRecords()) {
    System.out.println("offset=" + record.getOffset() + " entry=" + record.getEntry());
}
```

#### WalStats

Get WAL statistics.

```java
var stats = client.walStatsSync();
System.out.println("Entries: " + stats.getEntryCount() + ", Size: " + stats.getTotalSizeBytes() + " bytes");
```

#### Compact

Trigger WAL compaction.

```java
var result = client.compactSync(false);
System.out.println("Reclaimed: " + result.getBytesReclaimed() + " bytes");
```

### Async Usage

All sync methods have async counterparts returning `CompletableFuture<T>`:

```java
client.ping()
    .thenCompose(v -> client.createInstance(request))
    .thenCompose(inst -> client.applyEvent(eventRequest))
    .thenAccept(result -> System.out.println(result.getToState()))
    .exceptionally(ex -> {
        if (ex.getCause() instanceof RstmdbException re) {
            System.err.println("Error: " + re.getErrorCode());
        }
        return null;
    })
    .join();
```

## Error Handling

```java
try {
    client.applyEventSync(request);
} catch (RstmdbException e) {
    if (RstmdbException.isInstanceNotFound(e)) {
        System.out.println("Instance not found");
    } else if (RstmdbException.isInvalidTransition(e)) {
        System.out.println("Cannot apply event from current state: " + e.getMessage());
    } else if (RstmdbException.isConflict(e)) {
        System.out.println("Optimistic concurrency conflict");
    } else if (e.isRetryable()) {
        System.out.println("Transient error, safe to retry");
    }
}
```

Error codes: `UNSUPPORTED_PROTOCOL`, `BAD_REQUEST`, `UNAUTHORIZED`, `AUTH_FAILED`, `NOT_FOUND`, `MACHINE_NOT_FOUND`, `MACHINE_VERSION_EXISTS`, `MACHINE_VERSION_LIMIT_EXCEEDED`, `INSTANCE_NOT_FOUND`, `INSTANCE_EXISTS`, `INVALID_TRANSITION`, `GUARD_FAILED`, `CONFLICT`, `WAL_IO_ERROR`, `INTERNAL_ERROR`, `RATE_LIMITED`.

## Examples

### Order Processing

```java
import com.rstmdb.client.*;
import com.rstmdb.client.model.*;
import java.util.List;
import java.util.Map;

public class OrderProcessing {
    static void processOrder(RstmdbClient client, String orderId) {
        // Create order
        client.createInstanceSync(new CreateInstanceRequest(
            orderId, "order", 1,
            Map.of("items", List.of("item-1", "item-2"), "total", 149.99),
            null
        ));

        // Process payment
        client.applyEventSync(new ApplyEventRequest(
            orderId, "PAY",
            Map.of("payment_id", "pay-123"),
            null, null, null, null
        ));

        // Ship order
        client.applyEventSync(new ApplyEventRequest(
            orderId, "SHIP",
            Map.of("tracking", "1Z999"),
            null, null, null, null
        ));

        // Get final state
        var order = client.getInstanceSync(orderId);
        System.out.println("Order " + orderId + " is now: " + order.getState());
    }

    public static void main(String[] args) throws Exception {
        try (var client = RstmdbClient.connect("localhost", 7401)) {
            processOrder(client, "order-001");
        }
    }
}
```

### Event Consumer

```java
import com.rstmdb.client.*;

public class EventConsumer {
    public static void main(String[] args) throws Exception {
        try (var client = RstmdbClient.connect("localhost", 7401)) {
            System.out.println("Listening for shipped orders...");

            var sub = client.watchAllSync(new WatchAllOptions(
                true, null, new String[]{"order"},
                null, new String[]{"shipped"}, null
            ));

            for (var event : sub.events()) {
                System.out.println("Order " + event.getInstanceId() + " shipped!");
                // Send notification, update external system, etc.
            }
        }
    }
}
```

## Testcontainers

The `rstmdb-testcontainer` module provides JUnit integration for testing:

```kotlin
// Gradle
testImplementation("com.rstmdb:rstmdb-testcontainer:0.1.0")
```

```java
@Testcontainers
class OrderServiceTest {
    @Container
    static RstmdbContainer rstmdb = new RstmdbContainer();

    @Test
    void testOrderWorkflow() throws Exception {
        try (var client = RstmdbClient.connect(
                rstmdb.getHost(), rstmdb.getPort())) {
            // test code here
        }
    }
}
```

## Resources

- [GitHub Repository](https://github.com/rstmdb/rstmdb-java)
- [Maven Central](https://central.sonatype.com/artifact/com.rstmdb/rstmdb-client)
