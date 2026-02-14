---
sidebar_position: 3
---

# Python Client

The official Python client library for rstmdb.

**Repository:** [github.com/rstmdb/rstmdb-py](https://github.com/rstmdb/rstmdb-py)

## Installation

```bash
pip install rstmdb
```

For development:
```bash
pip install rstmdb[dev]
```

**Requirements:** Python 3.9+

## Features

- Async-first design using asyncio
- Full feature parity with the Rust client
- Complete type hints with Pydantic models
- TLS/mTLS support
- Event streaming via async iterators

## Quick Start

```python
import asyncio
from rstmdb import Client

async def main():
    # Connect to server
    client = Client("localhost", 7401, token="my-secret-token")
    await client.connect()

    # Define a state machine
    await client.put_machine("order", 1, {
        "states": ["pending", "paid", "shipped", "delivered"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "SHIP", "to": "shipped"},
            {"from": "shipped", "event": "DELIVER", "to": "delivered"}
        ]
    })

    # Create an instance
    instance = await client.create_instance(
        machine="order",
        version=1,
        instance_id="order-001",
        context={"customer": "alice", "total": 99.99}
    )
    print(f"Created: {instance.id} in state {instance.state}")

    # Apply events
    result = await client.apply_event(
        instance_id="order-001",
        event="PAY",
        payload={"payment_id": "pay-123"}
    )
    print(f"Transitioned: {result.previous_state} -> {result.current_state}")

    await client.close()

asyncio.run(main())
```

## Connection

### Basic Connection

```python
from rstmdb import Client

client = Client("localhost", 7401, token="my-secret-token")
await client.connect()
```

### TLS Connection

```python
client = Client(
    host="secure.example.com",
    port=7401,
    token="my-secret-token",
    tls=True,
    ca_cert="/path/to/ca.pem"
)
await client.connect()
```

### Mutual TLS (mTLS)

```python
client = Client(
    host="secure.example.com",
    port=7401,
    token="my-secret-token",
    tls=True,
    ca_cert="/path/to/ca.pem",
    client_cert="/path/to/client.pem",
    client_key="/path/to/client-key.pem"
)
await client.connect()
```

### Development Mode (Insecure)

```python
# Skip TLS verification - development only!
client = Client(
    host="localhost",
    port=7401,
    tls=True,
    insecure=True
)
```

## API Reference

### Machine Operations

#### put_machine

Register a state machine definition.

```python
await client.put_machine(
    name="order",
    version=1,
    definition={
        "states": ["pending", "paid", "shipped"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "SHIP", "to": "shipped"}
        ]
    }
)
```

#### get_machine

Retrieve a machine definition.

```python
machine = await client.get_machine("order", version=1)
print(machine.definition.states)
print(machine.definition.initial)
```

#### list_machines

List all machines.

```python
machines = await client.list_machines()
for m in machines:
    print(f"{m.name}: {m.versions}")
```

### Instance Operations

#### create_instance

Create a new instance.

```python
instance = await client.create_instance(
    machine="order",
    version=1,
    instance_id="order-001",
    context={"customer": "alice"}
)
```

#### get_instance

Get instance state and context.

```python
instance = await client.get_instance("order-001")
print(f"State: {instance.state}")
print(f"Context: {instance.context}")
```

#### delete_instance

Delete an instance.

```python
await client.delete_instance("order-001")
```

### Event Operations

#### apply_event

Apply an event to trigger a state transition.

```python
result = await client.apply_event(
    instance_id="order-001",
    event="PAY",
    payload={"amount": 99.99}
)

print(f"Previous: {result.previous_state}")
print(f"Current: {result.current_state}")
```

### Streaming

#### watch_all

Subscribe to events with filtering.

```python
async with client.watch_all(
    machines=["order"],
    to_states=["shipped", "delivered"]
) as stream:
    async for event in stream.events():
        print(f"{event.instance_id}: {event.event} -> {event.to_state}")
```

#### watch_instance

Watch a specific instance.

```python
async with client.watch_instance("order-001") as stream:
    async for event in stream.events():
        print(f"Event: {event.event}, New state: {event.to_state}")
```

### System Operations

#### ping

Health check.

```python
await client.ping()
```

#### info

Get server information.

```python
info = await client.info()
print(f"Version: {info.version}")
print(f"Instances: {info.stats.instances}")
```

## Error Handling

```python
from rstmdb import (
    Client,
    NotFoundError,
    InvalidTransitionError,
    AuthenticationError,
    ConnectionError
)

try:
    await client.apply_event("order-001", "PAY")
except NotFoundError:
    print("Instance not found")
except InvalidTransitionError as e:
    print(f"Cannot apply event from current state: {e}")
except AuthenticationError:
    print("Authentication failed")
except ConnectionError:
    print("Connection lost")
```

## Examples

### Order Processing

```python
import asyncio
from rstmdb import Client

async def process_order(client: Client, order_id: str):
    # Create order
    await client.create_instance(
        machine="order",
        version=1,
        instance_id=order_id,
        context={"items": ["item-1", "item-2"], "total": 149.99}
    )

    # Process payment
    await client.apply_event(order_id, "PAY", {"payment_id": "pay-123"})

    # Ship order
    await client.apply_event(order_id, "SHIP", {"tracking": "1Z999"})

    # Get final state
    order = await client.get_instance(order_id)
    print(f"Order {order_id} is now: {order.state}")

async def main():
    client = Client("localhost", 7401)
    await client.connect()
    await process_order(client, "order-001")
    await client.close()

asyncio.run(main())
```

### Event Consumer

```python
import asyncio
from rstmdb import Client

async def consume_events():
    client = Client("localhost", 7401)
    await client.connect()

    print("Listening for shipped orders...")

    async with client.watch_all(
        machines=["order"],
        to_states=["shipped"]
    ) as stream:
        async for event in stream.events():
            print(f"Order {event.instance_id} shipped!")
            # Send notification, update external system, etc.

asyncio.run(consume_events())
```

## Resources

- [GitHub Repository](https://github.com/rstmdb/rstmdb-py)
- [PyPI Package](https://pypi.org/project/rstmdb/)
