# Kharbranth Project Guide for Claude

## Overview

Kharbranth is a robust WebSocket connection management library written in Rust. It provides a high-level abstraction for managing multiple WebSocket connections with built-in reconnection logic, heartbeat mechanisms, and flexible message handling.

## Core Purpose

The library addresses common challenges in WebSocket client applications:

1. **Connection Reliability**: Automatic reconnection with configurable timeouts
2. **Connection Pooling**: Manage multiple named connections simultaneously  
3. **Heartbeat Management**: Built-in ping/pong mechanism to detect connection health
4. **Flexible Message Handling**: Hook-based system for processing different message types
5. **Error Resilience**: Comprehensive error handling and recovery mechanisms

## Use Cases

This is used to subscribe to crypto exchange websocket feeds.

## Architecture

### Two-Layer Design

#### Layer 1: Connection
- WebSocket connection wrapper with actor-based design
- Handles individual connection lifecycle
- Manages read/write splitting with separate async actors
- Implements ping/pong heartbeat protocol with timeout detection
- Converts WebSocket frames to typed Message enum variants

#### Layer 2: Manager
- Manages multiple named connections
- Provides connection pooling with thread-safe access
- Handles broadcasting for connection control
- Implements reconnection logic with configurable timeouts

### Key Design Decisions

#### Actor-Based Architecture
The library uses separate actors for read, write, and ping operations on each connection. This allows concurrent operations while maintaining thread safety and proper resource management.

#### Message-Based Communication
All WebSocket frames and system events are represented by a unified `Message` enum that flows through broadcast channels, enabling flexible message routing and handling.

#### Shared State Management
Uses `Arc<DashMap<>>` pattern for thread-safe shared state between async tasks, enabling concurrent read/write operations while maintaining memory safety and performance.

## Project Structure

This project is a Rust library crate with the following key components:

### Dependencies
- **tokio-tungstenite**: WebSocket client/server implementation
- **tokio**: Async runtime with full features
- **futures-util**: Stream and sink utilities
- **anyhow**: Error handling
- **thiserror**: Error derive macros
- **log**: Logging framework
- **env_logger**: Used with log
- **dashmap**: Concurrent HashMap implementation for thread-safe shared state

Log levels should be used to enable debugging. Key paths should be covered by logs.

### Core Types

#### `Connection`
WebSocket connection wrapper that manages:
- Read/write stream splitting with separate actors
- Ping/pong heartbeat mechanism
- Connection lifecycle management
- Integration with the Manager's message system

#### `Manager`
Connection manager that handles:
- Multiple named connections in a DashMap
- Connection pooling with Arc<DashMap<>>
- Automatic reconnection logic
- Broadcasting system for connection control
- Global message distribution across all connections

#### `Message`
Enum representing all possible WebSocket message types and system events:
- `PingMessage(String, String)`: Connection name + ping data
- `PongMessage(String, String)`: Connection name + pong data
- `TextMessage(String, String)`: Connection name + text content
- `BinaryMessage(String, Vec<u8>)`: Connection name + binary data
- `CloseMessage(String, Option<String>)`: Connection name + optional close reason
- `FrameMessage(String, String)`: Connection name + frame information
- `ReadError(String)`: Connection name with read error
- `WriteError(String)`: Connection name with write error
- `PongReceiveTimeoutError(String)`: Connection name with pong timeout

#### `ConnectionError`
Custom error types for WebSocket operations:
- `ConnectionInitFailed(String)`: Connection initialization failed with details

## Key Patterns

Manager is the key abstraction used by callers. This should hide the details of the underlying websocket connection.

### Usage Pattern
1. **Connection Management**: Users create named connections via `Manager::new_conn(name, config)`
2. **Message Reading**: Users call `Manager::read()` to get a broadcast receiver for all messages from all connections
3. **Message Writing**: Users call `Manager::write(name, message)` to send messages to specific connections
4. **Connection Control**: Users can reconnect (`Manager::reconnect()`) or close (`Manager::close_conn()`) connections

### Message Flow
- All incoming WebSocket frames are converted to `Message` enum variants and broadcast globally
- Users receive typed messages indicating both the connection name and message content
- Outgoing messages are sent to specific connections via the connection name

Config options provide constant behavior. The Manager handles automatic reconnection when errors occur or pong timeouts happen.

## Performance Characteristics

No hard performance requirements but should reduce the number of copies, should be resilient to faults allowing callers to trigger restarts when feed moves into bad state, scalable to many websocket connections, and should be thread-safe.

## Development Guidelines

### Branch Structure

#### Main Branch
- `main`: Primary development branch
- All PRs should target `main`
- Protected branch requiring PR reviews

#### Feature Branches
- Create feature branches from `main`
- Use descriptive names: `fix-wsmanager-mut-self`, `add-connection-pooling`
- Keep branches focused on single features/fixes
- Should be tagged to an issue
- Should increment the version in `Cargo.toml` for every change with semver formatting (vX.X.X)
- Increment the major versions for breaking changes, minor otherwise

### Commit Guidelines

#### Commit Message Format
Follow the existing pattern seen in recent commits:
- Use imperative mood: "Fix WSManager::write() requiring &mut self unnecessarily"
- Be descriptive about the change
- Try not to commit all changes as one big commit but split it up into parts that make sense

### Pull Request Process

#### Before Creating PR
1. Ensure all tests pass: `cargo test`
2. Check code compiles: `cargo build`
3. Run clippy for linting: `cargo clippy`
4. Format code: `cargo fmt`

#### PR Creation
1. Create descriptive PR title
2. Include summary of changes
3. Reference related issues
4. Add test plan if applicable

### Common Issue Patterns to Look For

#### Connection Management Issues
- Memory leaks in connection pooling
- Improper cleanup of WebSocket connections
- Race conditions in concurrent connection handling

#### Error Handling
- Unwrap() calls that should use proper error handling
- Missing error propagation in async contexts
- Inconsistent error types across modules

#### Performance Issues
- Unnecessary cloning of large data structures
- Inefficient HashMap/DashMap operations
- Blocking operations in async contexts

#### Code Quality
- Missing documentation for public APIs
- Inconsistent naming conventions
- Dead code or unused imports

### Testing Strategy

#### Unit Tests
- Mock WebSocket connections using `tokio-test`
- Test error conditions and edge cases
- Verify connection lifecycle management

#### Integration Tests
- Test full WebSocket connection flow
- Verify reconnection logic
- Test concurrent connection handling

### Code Review Checklist

- [ ] Code compiles without warnings
- [ ] All tests pass
- [ ] No clippy warnings
- [ ] Proper error handling
- [ ] Memory safety considerations
- [ ] Performance implications reviewed
- [ ] Documentation updated if needed
- [ ] Make sure PR does not contain unneeded files
- [ ] Check for unused code created by this change
- [ ] Version updated in Cargo.toml

## Build Configuration
- Rust 2024 edition
- Library crate with `src/lib.rs` as entry point
- No binary targets defined

## Comments
- Do not add superfluous comments that only describe what something does
- Tests should have a comment at the top explaining in plain terms what the test does

## Formatting
- Use spaces, not tabs
- No trailing whitespace on any lines
- Empty lines should be completely empty (no spaces)
- Always run `cargo fmt` before committing

## Extension Points

The library is designed for extensibility:

1. **Message Processing**: Handle different `Message` enum variants in your application logic
2. **Error Handling**: Extend `ConnectionError` for domain-specific error types  
3. **Connection Strategies**: Customize reconnection and retry logic through Config
4. **Protocol Support**: The actor-based design supports extending to different WebSocket protocols

## GitHub Actions

The repository includes a GitHub Action (`.github/workflows/claude.yml`) that triggers Claude Code when:
- A label containing "claude" is added to an issue
- The issue creator is "calumrussell" (security constraint for public repo)

This provides automated assistance while maintaining security for the public repository.
