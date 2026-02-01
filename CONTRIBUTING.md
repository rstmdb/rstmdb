# Contributing to rstmdb

Thank you for your interest in contributing to rstmdb! This document provides guidelines and information for contributors.

## Getting Started

### Prerequisites

- Rust 1.75+ (stable)
- Git
- Docker (optional, for container builds)

### Setup

```bash
# Clone the repository
git clone https://github.com/rstmdb/rstmdb.git
cd rstmdb

# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace

# Run the server
cargo run
```

## Development Workflow

### Code Style

We use standard Rust formatting and linting:

```bash
# Format code (required before committing)
cargo fmt --all

# Run clippy (must pass with no warnings)
cargo clippy --all-targets --all-features -- -D warnings
```

### Running Tests

```bash
# Run all tests
cargo test --workspace --all-features

# Run tests for a specific crate
cargo test -p rstmdb-core

# Run a specific test
cargo test -p rstmdb-wal test_recovery

# Run tests with output
cargo test --workspace -- --nocapture
```

### Building Documentation

```bash
# Build and open documentation
cargo doc --workspace --no-deps --open
```

### Benchmarks

```bash
# Run all benchmarks
cargo bench -p rstmdb-bench

# Run specific benchmark
cargo bench -p rstmdb-bench -- wal
```

## Project Structure

| Crate             | Description          | When to modify                                      |
| ----------------- | -------------------- | --------------------------------------------------- |
| `rstmdb-protocol` | Wire protocol (RCP)  | Adding protocol operations, changing message format |
| `rstmdb-wal`      | Write-Ahead Log      | WAL performance, durability, recovery               |
| `rstmdb-core`     | State machine engine | Machine definitions, guards, transitions            |
| `rstmdb-storage`  | Snapshots            | Snapshot format, compaction logic                   |
| `rstmdb-server`   | TCP server           | Connection handling, session management, handlers   |
| `rstmdb-client`   | Rust client          | Client API, connection pooling                      |
| `rstmdb-cli`      | CLI tool             | Commands, REPL features                             |
| `rstmdb-bench`    | Benchmarks           | Performance testing                                 |

## Making Changes

### Branch Naming

- `feature/description` - New features
- `fix/description` - Bug fixes
- `docs/description` - Documentation
- `refactor/description` - Code refactoring
- `test/description` - Test improvements

### Commit Messages

Use clear, descriptive commit messages:

```
feat(core): add support for wildcard transitions

Add ability to define transitions that match any source state
using "*" as the from field. This simplifies machine definitions
for events that can occur from multiple states.
```

Format: `type(scope): description`

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`

### Pull Request Process

1. **Fork and branch** - Create a feature branch from `main`
2. **Make changes** - Keep commits focused and atomic
3. **Test locally** - Ensure all tests pass
4. **Format and lint** - Run `cargo fmt` and `cargo clippy`
5. **Update docs** - Update relevant documentation
6. **Open PR** - Provide clear description of changes

### PR Checklist

- [ ] Code compiles without warnings
- [ ] All tests pass (`cargo test --workspace`)
- [ ] Code is formatted (`cargo fmt --all -- --check`)
- [ ] Clippy passes (`cargo clippy --all-targets --all-features -- -D warnings`)
- [ ] New code has tests where appropriate
- [ ] Documentation updated if needed
- [ ] Commit messages follow conventions

## Areas for Contribution

### High Priority

- **Client libraries** - Python, Go, TypeScript implementations
- **Documentation** - Tutorials, examples, API guides
- **Testing** - Integration tests, chaos testing, fuzzing

### Good First Issues

Look for issues labeled `good-first-issue` in the issue tracker. These are typically:

- Documentation improvements
- Small bug fixes
- Test coverage improvements
- Error message improvements

### Adding a New Protocol Operation

1. Define the operation in `rstmdb-protocol/src/message.rs`
2. Add handler in `rstmdb-server/src/handler.rs`
3. Update client in `rstmdb-client/src/client.rs`
4. Add CLI command if user-facing
5. Update `docs/protocol.md` and `docs/RCP_SPECIFICATION.md`
6. Add integration tests

### Adding a Client Library

See `docs/python-client-spec.md` for the Python client specification as a reference. Key requirements:

- Implement full RCP protocol support
- Async-first design
- Connection management and reconnection
- TLS support
- Comprehensive error handling
- Documentation and examples

## Testing Guidelines

### Unit Tests

Place unit tests in the same file as the code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // ...
    }
}
```

### Integration Tests

Place integration tests in `tests/` directories:

```
rstmdb-server/
  tests/
    integration_test.rs
```

### Property-Based Tests

We use `proptest` for property-based testing:

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_roundtrip(input in any::<String>()) {
        let encoded = encode(&input);
        let decoded = decode(&encoded)?;
        assert_eq!(input, decoded);
    }
}
```

## CI Pipeline

All PRs must pass the CI pipeline:

| Job    | Description                                                |
| ------ | ---------------------------------------------------------- |
| Format | `cargo fmt --all -- --check`                               |
| Clippy | `cargo clippy --all-targets --all-features -- -D warnings` |
| Build  | Build on Linux, macOS, Windows                             |
| Test   | Run tests on Linux, macOS, Windows                         |
| Doc    | Build documentation without warnings                       |
| Docker | Build Docker image                                         |

## Code Review

### What We Look For

- **Correctness** - Does the code do what it claims?
- **Tests** - Are edge cases covered?
- **Performance** - Any obvious inefficiencies?
- **Safety** - No panics in library code, proper error handling
- **Style** - Consistent with existing code
- **Documentation** - Public APIs documented

### Response Time

Maintainers aim to review PRs within a few days. Complex changes may take longer.

## Questions?

- Open an issue for bugs or feature requests
- Start a discussion for questions or ideas
- Check existing issues before creating new ones

Thank you for contributing!
