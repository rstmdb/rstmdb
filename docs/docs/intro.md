---
sidebar_position: 1
slug: /
---

# Introduction

**rstmdb** is a state machine database designed for building event-sourced, durable applications with explicit state management. It combines the reliability of Write-Ahead Logging (WAL) with the expressiveness of finite state machines.

## What is rstmdb?

rstmdb provides:

- **State Machine Engine** - Define state machines with states, transitions, and guard conditions
- **Durable Storage** - WAL-based persistence with crash recovery and snapshot compaction
- **Real-time Subscriptions** - Watch individual instances or subscribe to filtered event streams
- **Simple Protocol** - JSON-based RCP protocol over TCP with optional TLS

## Key Features

### State Machine Definitions
Define your state machines using a simple JSON DSL:

```json
{
  "states": ["pending", "approved", "rejected"],
  "initial": "pending",
  "transitions": [
    {"from": "pending", "event": "APPROVE", "to": "approved", "guard": "ctx.amount <= 1000"},
    {"from": "pending", "event": "REJECT", "to": "rejected"}
  ]
}
```

### Instance Management
Create instances of your state machines and apply events to transition between states:

```bash
# Create an instance
rstmdb-cli create-instance -m order -V 1 -i order-001 -c '{"amount": 500}'

# Apply an event
rstmdb-cli apply-event -i order-001 -e APPROVE -p '{"approver": "alice"}'
```

### Guard Expressions
Conditional transitions based on instance context:

```
ctx.amount > 100 && ctx.approved
```

### Real-time Subscriptions
Watch for state changes in real-time:

```bash
# Watch all shipped orders
rstmdb-cli watch-all --machines order --to-states shipped
```

## Use Cases

- **Order Processing** - Track order lifecycle from creation to fulfillment
- **Approval Workflows** - Multi-step approval processes with conditional transitions
- **IoT Device Management** - Track device states and handle events
- **Game State Management** - Manage game sessions and player states
- **Saga/Process Managers** - Coordinate long-running distributed processes

## Quick Example

```bash
# Start the server
./rstmdb

# In another terminal, create a state machine
rstmdb-cli put-machine -n order -v 1 '{
  "states": ["created", "paid", "shipped", "delivered"],
  "initial": "created",
  "transitions": [
    {"from": "created", "event": "PAY", "to": "paid"},
    {"from": "paid", "event": "SHIP", "to": "shipped"},
    {"from": "shipped", "event": "DELIVER", "to": "delivered"}
  ]
}'

# Create an order instance
rstmdb-cli create-instance -m order -V 1 -i order-123 -c '{"customer": "alice"}'

# Process the order
rstmdb-cli apply-event -i order-123 -e PAY -p '{"amount": 99.99}'
rstmdb-cli apply-event -i order-123 -e SHIP -p '{"carrier": "fedex"}'

# Check the current state
rstmdb-cli get-instance order-123
```

## Architecture Overview

```
┌─────────────┐     ┌─────────────────┐     ┌──────────────────┐
│   Client    │────▶│   TCP Server    │────▶│  Command Handler │
│  (CLI/SDK)  │◀────│   (TLS/Auth)    │◀────│                  │
└─────────────┘     └─────────────────┘     └────────┬─────────┘
                                                     │
                    ┌────────────────────────────────┼────────────────────────────────┐
                    │                                ▼                                │
                    │  ┌─────────────────┐     ┌──────────────┐     ┌─────────────┐  │
                    │  │ State Machine   │────▶│   Storage    │────▶│    WAL      │  │
                    │  │    Engine       │◀────│   (Memory)   │◀────│ (Segments)  │  │
                    │  └─────────────────┘     └──────────────┘     └─────────────┘  │
                    │                                                                 │
                    │                          rstmdb Server                          │
                    └─────────────────────────────────────────────────────────────────┘
```

## License

rstmdb is licensed under the Business Source License 1.1 (BSL-1.1). The license converts to Apache 2.0 after 4 years from each release.

## Next Steps

- [Getting Started](./getting-started) - Install and run rstmdb
- [Architecture](./architecture) - Understand how rstmdb works
- [CLI Reference](./cli) - Command-line interface documentation
