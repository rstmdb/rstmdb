---
sidebar_position: 11
---

# Roadmap

Planned features and development phases for rstmdb.

## Current Version: 0.2.x

### Core Features (Complete)

- ✅ State machine engine with JSON DSL
- ✅ Guard expressions for conditional transitions
- ✅ Instance lifecycle management
- ✅ WAL-based durability with crash recovery
- ✅ Automatic compaction and cleanup
- ✅ Real-time subscriptions (WATCH_INSTANCE, WATCH_ALL)
- ✅ Bearer token authentication
- ✅ TLS/mTLS support
- ✅ Prometheus metrics
- ✅ CLI with REPL
- ✅ Rust client library

## Phase 1: Client Libraries

**Status:** Complete

Expand language support with official client libraries.

### Python Client ✅

- [x] Async client (asyncio)
- [x] Synchronous wrapper
- [x] Connection pooling
- [x] Full type hints
- [x] PyPI package

### TypeScript/Node.js Client ✅

- [x] Promise-based async API
- [x] Full TypeScript types
- [x] Streaming support
- [x] npm package

### Go Client ✅

- [x] Idiomatic Go API
- [x] Context support
- [x] Connection pooling

### C# / .NET Client ✅

- [x] Async API (Task-based)
- [x] Full type definitions
- [x] Streaming support
- [x] NuGet package

### Java Client ✅

- [x] CompletableFuture async API
- [x] Synchronous wrappers
- [x] Streaming support (Flow.Publisher)
- [x] Maven Central package

## Phase 2: High Availability

**Status:** In progress — core WAL streaming replication is shipped; failover tooling is next.

Enable read replicas for horizontal read scaling and improved availability.

### WAL Streaming Replication

- [x] WAL entry streaming to replicas
- [x] Configurable replication lag thresholds
- [x] Read-only replica mode
- [x] Automatic replica catch-up (offset-based)
- [x] Async and sync replication modes
- [x] Backpressure handling (slow-replica disconnect)
- [x] TLS for replication connections
- [x] Hashed auth tokens with rotation

### Health & Monitoring

- [x] Replica lag monitoring (entries and seconds)
- [x] Per-replica primary-side metrics
- [x] Replication metrics (Prometheus) + Grafana dashboard
- [ ] Health check endpoints

### Failover Support

- [ ] External failover orchestration hooks
- [ ] Graceful primary promotion
- [ ] Client-side failover support

## Phase 3: Consensus & Automatic Failover

**Status:** Research

Implement Raft consensus for automatic leader election and failover.

### Raft Integration

- [ ] Raft consensus protocol
- [ ] Automatic leader election
- [ ] Log replication via Raft
- [ ] Cluster membership changes

### Cluster Management

- [ ] Node discovery
- [ ] Quorum configuration
- [ ] Split-brain protection
- [ ] Rolling upgrades

## Phase 4: Operational Excellence

**Status:** Planned

Production-ready operational features.

### Security

- [ ] Encryption at rest (AES-256)
- [ ] Key rotation
- [ ] Audit logging improvements
- [ ] Role-based access control (RBAC)

### Recovery

- [ ] Point-in-time recovery
- [ ] Incremental backups
- [ ] Cross-region replication

### Observability

- [ ] OpenTelemetry integration
- [ ] Distributed tracing
- [ ] Enhanced metrics

### Operations

- [ ] Configuration hot-reload
- [ ] Admin HTTP API
- [ ] Graceful shutdown improvements

## Phase 5: Performance & Scale

**Status:** Future

Horizontal scaling and performance optimizations.

### Sharding

- [ ] Key-based sharding
- [ ] Automatic shard balancing
- [ ] Cross-shard queries

### Performance

- [ ] WAL compression
- [ ] Batch operation optimization
- [ ] Memory-mapped WAL reads
- [ ] Connection multiplexing

### Read Scaling

- [ ] Read replicas with routing
- [ ] Eventually consistent reads
- [ ] Stale reads for performance

## Community Requests

Features requested by the community:

| Feature                            | Votes  | Status      |
| ---------------------------------- | ------ | ----------- |
| HTTP/REST API                      | ⭐⭐⭐ | Planned     |
| gRPC interface                     | ⭐⭐   | Planned     |
| Webhooks on transitions            | ⭐⭐   | Considering |
| GraphQL API                        | ⭐     | Considering |
| Instance TTL/expiration            | ⭐     | Considering |
| State machine versioning migration | ⭐     | Research    |

## Contributing

We welcome contributions! Areas where help is especially appreciated:

1. **Client libraries** - additional languages and improvements
2. **Documentation** - Tutorials, examples, translations
3. **Testing** - Integration tests, chaos testing
4. **Benchmarks** - Performance comparisons, optimization

See [CONTRIBUTING.md](https://github.com/rstmdb/rstmdb/blob/main/CONTRIBUTING.md) for guidelines.

## Release Schedule

| Version | Target   | Focus                                             |
| ------- | -------- | ------------------------------------------------- |
| 0.2.0   | Released | Client libraries, FLUSH_ALL, instance re-creation |
| 0.3.0   | Released | WAL streaming replication                         |
| 0.4.0   | Q3 2026  | Failover tooling, Raft consensus                  |
| 1.0.0   | Q4 2026  | Production ready                                  |

_Dates are estimates and subject to change based on community feedback and contributions._

## Feedback

Have a feature request or suggestion?

- [GitHub Issues](https://github.com/rstmdb/rstmdb/issues)
- [GitHub Discussions](https://github.com/rstmdb/rstmdb/discussions)
