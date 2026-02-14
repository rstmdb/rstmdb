---
sidebar_position: 4
---

# TypeScript/Node.js Client

The official TypeScript/Node.js client library for rstmdb.

**Repository:** [github.com/rstmdb/rstmdb-js](https://github.com/rstmdb/rstmdb-js)

## Installation

```bash
npm install @rstmdb/client
```

Or with other package managers:
```bash
pnpm add @rstmdb/client
yarn add @rstmdb/client
```

**Requirements:** Node.js 18.0.0+

## Features

- Full TypeScript support with complete type definitions
- Promise-based async API
- Streaming support via AsyncIterator and EventEmitter
- Automatic reconnection
- TLS/mTLS support
- Connection pooling

## Quick Start

```typescript
import { Client } from '@rstmdb/client';

async function main() {
  // Connect to server
  const client = await Client.connect('localhost', 7401, {
    auth: 'my-secret-token'
  });

  // Define a state machine
  await client.putMachine('order', 1, {
    states: ['pending', 'paid', 'shipped', 'delivered'],
    initial: 'pending',
    transitions: [
      { from: 'pending', event: 'PAY', to: 'paid' },
      { from: 'paid', event: 'SHIP', to: 'shipped' },
      { from: 'shipped', event: 'DELIVER', to: 'delivered' }
    ]
  });

  // Create an instance
  const instance = await client.createInstance({
    machine: 'order',
    version: 1,
    id: 'order-001',
    context: { customer: 'alice', total: 99.99 }
  });
  console.log(`Created: ${instance.id} in state ${instance.state}`);

  // Apply events
  const result = await client.applyEvent({
    instanceId: 'order-001',
    event: 'PAY',
    payload: { paymentId: 'pay-123' }
  });
  console.log(`Transitioned: ${result.previousState} -> ${result.currentState}`);

  await client.close();
}

main();
```

## Connection

### Static Factory (Recommended)

```typescript
import { Client } from '@rstmdb/client';

const client = await Client.connect('localhost', 7401, {
  auth: 'my-secret-token'
});
```

### Builder Pattern

```typescript
import { Client, ClientOptions } from '@rstmdb/client';

const config = ClientOptions
  .create('localhost')
  .port(7401)
  .auth('my-secret-token')
  .connectionTimeout(10000)
  .requestTimeout(30000)
  .build();

const client = new Client(config);
await client.connect();
```

### TLS Connection

```typescript
const client = await Client.connect('secure.example.com', 7401, {
  auth: 'my-secret-token',
  tls: {
    ca: fs.readFileSync('/path/to/ca.pem')
  }
});
```

### Mutual TLS (mTLS)

```typescript
const client = await Client.connect('secure.example.com', 7401, {
  auth: 'my-secret-token',
  tls: {
    ca: fs.readFileSync('/path/to/ca.pem'),
    cert: fs.readFileSync('/path/to/client.pem'),
    key: fs.readFileSync('/path/to/client-key.pem')
  }
});
```

### Auto-Reconnection

```typescript
const client = await Client.connect('localhost', 7401, {
  reconnect: {
    enabled: true,
    interval: 1000,      // 1 second between attempts
    maxAttempts: 10      // Give up after 10 attempts
  }
});
```

## Configuration Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `auth` | `string` | - | Authentication token |
| `connectionTimeout` | `number` | `10000` | Connection timeout (ms) |
| `requestTimeout` | `number` | `30000` | Request timeout (ms) |
| `tls` | `TlsOptions` | - | TLS configuration |
| `reconnect.enabled` | `boolean` | `true` | Enable auto-reconnect |
| `reconnect.interval` | `number` | `1000` | Reconnect interval (ms) |
| `reconnect.maxAttempts` | `number` | `10` | Max reconnect attempts |
| `clientName` | `string` | - | Client identifier |

## API Reference

### Machine Operations

#### putMachine

Register a state machine definition.

```typescript
await client.putMachine('order', 1, {
  states: ['pending', 'paid', 'shipped'],
  initial: 'pending',
  transitions: [
    { from: 'pending', event: 'PAY', to: 'paid' },
    { from: 'paid', event: 'SHIP', to: 'shipped' }
  ]
});
```

#### getMachine

Retrieve a machine definition.

```typescript
const machine = await client.getMachine('order', 1);
console.log(machine.definition.states);
console.log(machine.definition.initial);
```

#### listMachines

List all machines.

```typescript
const machines = await client.listMachines();
for (const m of machines) {
  console.log(`${m.name}: ${m.versions.join(', ')}`);
}
```

### Instance Operations

#### createInstance

Create a new instance.

```typescript
const instance = await client.createInstance({
  machine: 'order',
  version: 1,
  id: 'order-001',
  context: { customer: 'alice' }
});
```

#### getInstance

Get instance state and context.

```typescript
const instance = await client.getInstance('order-001');
console.log(`State: ${instance.state}`);
console.log(`Context:`, instance.context);
```

#### deleteInstance

Delete an instance.

```typescript
await client.deleteInstance('order-001');
```

### Event Operations

#### applyEvent

Apply an event to trigger a state transition.

```typescript
const result = await client.applyEvent({
  instanceId: 'order-001',
  event: 'PAY',
  payload: { amount: 99.99 }
});

console.log(`Previous: ${result.previousState}`);
console.log(`Current: ${result.currentState}`);
```

#### applyEvent with expectedState

Optimistic concurrency control.

```typescript
const result = await client.applyEvent({
  instanceId: 'order-001',
  event: 'SHIP',
  expectedState: 'paid'  // Fails if not in 'paid' state
});
```

#### batch

Execute multiple operations atomically.

```typescript
const results = await client.batch({
  mode: 'atomic',
  operations: [
    { op: 'applyEvent', params: { instanceId: 'order-001', event: 'PAY' } },
    { op: 'applyEvent', params: { instanceId: 'order-002', event: 'PAY' } }
  ]
});
```

### Streaming

#### watchInstance (AsyncIterator)

```typescript
const stream = await client.watchInstance('order-001');

for await (const event of stream) {
  console.log(`Event: ${event.event}, New state: ${event.toState}`);
}
```

#### watchInstance (EventEmitter)

```typescript
const stream = await client.watchInstance('order-001');

stream.on('event', (event) => {
  console.log(`Event: ${event.event}, New state: ${event.toState}`);
});

stream.on('error', (error) => {
  console.error('Stream error:', error);
});

// Later: stop watching
await stream.close();
```

#### watchAll

Subscribe to events with filtering.

```typescript
const stream = await client.watchAll({
  machines: ['order'],
  toStates: ['shipped', 'delivered']
});

for await (const event of stream) {
  console.log(`${event.instanceId}: ${event.event} -> ${event.toState}`);
}
```

### System Operations

#### ping

Health check.

```typescript
await client.ping();
```

#### info

Get server information.

```typescript
const info = await client.info();
console.log(`Version: ${info.version}`);
console.log(`Instances: ${info.stats.instances}`);
```

## Error Handling

```typescript
import {
  Client,
  NotFoundError,
  ConflictError,
  InvalidTransitionError,
  AuthenticationError,
  ConnectionError,
  TimeoutError
} from '@rstmdb/client';

try {
  await client.applyEvent({ instanceId: 'order-001', event: 'PAY' });
} catch (error) {
  if (error instanceof NotFoundError) {
    console.log('Instance not found');
  } else if (error instanceof InvalidTransitionError) {
    console.log('Cannot apply event from current state');
  } else if (error instanceof ConflictError) {
    console.log('Concurrent modification - retry');
  } else if (error instanceof AuthenticationError) {
    console.log('Authentication failed');
  } else if (error instanceof ConnectionError) {
    console.log('Connection lost');
  } else if (error instanceof TimeoutError) {
    console.log('Request timed out');
  }

  // Check if error is retryable
  if (error.retryable) {
    // Safe to retry
  }
}
```

## TypeScript Types

```typescript
import type {
  Client,
  Machine,
  MachineDefinition,
  Instance,
  ApplyEventResult,
  WatchStream,
  StreamEvent,
  ServerInfo
} from '@rstmdb/client';
```

## Examples

### Order Processing

```typescript
import { Client } from '@rstmdb/client';

async function processOrder(client: Client, orderId: string) {
  // Create order
  await client.createInstance({
    machine: 'order',
    version: 1,
    id: orderId,
    context: { items: ['item-1', 'item-2'], total: 149.99 }
  });

  // Process payment
  await client.applyEvent({
    instanceId: orderId,
    event: 'PAY',
    payload: { paymentId: 'pay-123' }
  });

  // Ship order
  await client.applyEvent({
    instanceId: orderId,
    event: 'SHIP',
    payload: { tracking: '1Z999' }
  });

  // Get final state
  const order = await client.getInstance(orderId);
  console.log(`Order ${orderId} is now: ${order.state}`);
}

async function main() {
  const client = await Client.connect('localhost', 7401);
  await processOrder(client, 'order-001');
  await client.close();
}

main();
```

### Event Consumer

```typescript
import { Client } from '@rstmdb/client';

async function consumeEvents() {
  const client = await Client.connect('localhost', 7401);

  console.log('Listening for shipped orders...');

  const stream = await client.watchAll({
    machines: ['order'],
    toStates: ['shipped']
  });

  for await (const event of stream) {
    console.log(`Order ${event.instanceId} shipped!`);
    // Send notification, update external system, etc.
  }
}

consumeEvents();
```

### Retry with Backoff

```typescript
import { Client, TimeoutError, ConnectionError } from '@rstmdb/client';

async function applyWithRetry(
  client: Client,
  instanceId: string,
  event: string,
  maxRetries = 3
) {
  for (let attempt = 0; attempt < maxRetries; attempt++) {
    try {
      return await client.applyEvent({ instanceId, event });
    } catch (error) {
      if (!error.retryable || attempt === maxRetries - 1) {
        throw error;
      }
      const delay = 100 * Math.pow(2, attempt);
      await new Promise(resolve => setTimeout(resolve, delay));
    }
  }
}
```

## Resources

- [GitHub Repository](https://github.com/rstmdb/rstmdb-js)
- [npm Package](https://www.npmjs.com/package/@rstmdb/client)
