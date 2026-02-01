# rstmdb-cli

Command-line interface for [rstmdb](https://github.com/rstmdb/rstmdb).

## Overview

`rstmdb-cli` provides a full-featured command-line interface for interacting with rstmdb servers. It supports both interactive REPL mode and single-command execution for scripting.

## Features

- **Interactive REPL** - Tab completion, history, and syntax highlighting
- **Command mode** - Execute single commands for scripting
- **JSON output** - Machine-readable output for automation
- **TLS support** - Secure connections to rstmdb servers
- **Multiple output formats** - Table, JSON, and compact formats

## Installation

### From crates.io

```bash
cargo install rstmdb-cli
```

### From source

```bash
git clone https://github.com/rstmdb/rstmdb
cd rstmdb
cargo install --path rstmdb-cli
```

## Usage

### Interactive Mode

```bash
# Connect to local server
rstmdb-cli

# Connect to remote server with auth
rstmdb-cli --host db.example.com --port 7401 --user admin
```

### Command Mode

```bash
# Execute a single command
rstmdb-cli -c "SM.LIST"

# With JSON output
rstmdb-cli -c "INST.GET order-123" --json

# From a script
echo "SM.CREATE order {...}" | rstmdb-cli
```

## Commands

### Connection

```
PING                    # Health check
AUTH <user> <password>  # Authenticate
QUIT                    # Disconnect
```

### State Machines

```
SM.CREATE <name> <definition>    # Create state machine
SM.GET <id>                      # Get state machine details
SM.LIST                          # List all state machines
SM.DELETE <id>                   # Delete state machine
```

### Instances

```
INST.CREATE <sm_id> <data>       # Create instance
INST.GET <id>                    # Get instance details
INST.LIST [sm_id]                # List instances
INST.DELETE <id>                 # Delete instance
```

### Events

```
EVENT.APPLY <inst_id> <event> [payload]   # Apply event
HISTORY <inst_id>                          # Get event history
```

## Configuration

Create `~/.rstmdb/config.yaml`:

```yaml
default_host: localhost
default_port: 7401
default_user: admin

# TLS settings
tls:
  enabled: true
  ca_cert: ~/.rstmdb/ca.pem

# Output settings
output:
  format: table  # table, json, compact
  color: true
```

## Examples

### Create a State Machine

```bash
rstmdb-cli -c 'SM.CREATE order {
  "states": ["pending", "confirmed", "shipped", "delivered"],
  "initial": "pending",
  "transitions": [
    {"from": "pending", "to": "confirmed", "event": "confirm"},
    {"from": "confirmed", "to": "shipped", "event": "ship"},
    {"from": "shipped", "to": "delivered", "event": "deliver"}
  ]
}'
```

### Create and Progress an Instance

```bash
# Create instance
INST_ID=$(rstmdb-cli -c 'INST.CREATE sm-123 {"order_id": "ORD-456"}' --json | jq -r '.id')

# Apply events
rstmdb-cli -c "EVENT.APPLY $INST_ID confirm"
rstmdb-cli -c "EVENT.APPLY $INST_ID ship {\"tracking\": \"TRK-789\"}"

# Check state
rstmdb-cli -c "INST.GET $INST_ID"
```

## License

Licensed under the Business Source License 1.1. See [LICENSE](../LICENSE) for details.

## Part of rstmdb

This crate is part of the [rstmdb](https://github.com/rstmdb/rstmdb) state machine database. For full documentation and examples, visit the main repository.
