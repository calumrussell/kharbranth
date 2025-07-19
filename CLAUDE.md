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

#### Layer 1: Connection (`Connection<S>`)
- Generic WebSocket connection wrapper
- Handles individual connection lifecycle
- Manages read/write splitting and concurrent operations
- Implements ping/pong heartbeat protocol
- Provides hook system for message processing

#### Layer 2: Manager (`WSManager`)
- Manages multiple named connections
- Provides connection pooling with thread-safe access
- Handles broadcasting for connection control
- Implements reconnection logic with exponential backoff

### Key Design Decisions

#### Generic Stream Support
The `Connection<S>` type is generic over stream types, making it testable with mock streams while supporting real TLS WebSocket connections in production.

#### Stream/Sink Message Processing
The library provides a modern Stream/Sink API that integrates naturally with async Rust patterns. This replaces the previous callback-based hooks system, enabling async message handling and proper backpressure control.

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

#### `Connection<S>`
Generic WebSocket connection wrapper that manages:
- Read/write stream splitting
- Ping/pong heartbeat mechanism
- Message hooks for different WebSocket frame types
- Connection lifecycle management

#### `WSManager`
Connection manager that handles:
- Multiple named connections in a DashMap
- Connection pooling with Arc<DashMap<>>
- Automatic reconnection logic
- Broadcasting system for connection control

#### `WebSocketSink` and `WebSocketStream`
Modern Stream/Sink API for handling WebSocket messages:
- `WebSocketSink`: Implements `Sink<Message>` for sending messages with proper error handling
- `WebSocketStream`: Implements `Stream<Item = Result<Message, Error>>` for receiving messages
- `ConnectionState`: Enum tracking connection status (Connected, Disconnected, Error)
- Both provide `connection_state()` method for monitoring connection health

#### `Message`
Unified message enum for the Stream/Sink API:
- `Text(String)`: Text messages
- `Binary(Vec<u8>)`: Binary data
- `Ping(Vec<u8>)`: Ping frames
- `Pong(Vec<u8>)`: Pong frames  
- `Close(Option<CloseFrame>)`: Close frames

#### `ConnectionError`
Custom error types for WebSocket operations:
- `PingFailed`: Ping operation failed
- `PongReceiveTimeout`: Pong response timeout
- `ReadError`/`WriteError`: I/O errors
- `ConnectionDropped`: Connection lost
- `ConnectionNotFound`: Named connection not found

## Key Patterns

WSManager is the key abstraction used by callers. This should hide the details of the underlying websocket connection. 

### Stream/Sink API (Recommended)
Users call `connect_stream()` to get a `(WebSocketSink, WebSocketStream)` pair:
- `WebSocketSink` for sending messages with proper error handling and backpressure
- `WebSocketStream` for receiving messages as an async stream
- Both provide connection state monitoring

### Legacy Connection API
Users can still name a connection and use `new_conn()` with `write()` functions for backwards compatibility.

Config options provide constant behaviour. The Stream/Sink API provides modern async-native message handling. Callers are able to restart connections through a tokio channel.

### Example Usage

```rust
use kharbranth::{WSManager, Config, Message};
use futures_util::{SinkExt, StreamExt};

let manager = WSManager::new();
let config = Config {
    url: "wss://example.com/ws".to_string(),
    ping_duration: 30,
    ping_timeout: 10,
    reconnect_timeout: 5,
    write_on_init: Vec::new(),
};

// Connect and get stream/sink pair
let (mut sink, mut stream) = manager.connect_stream("my_connection", config).await?;

// Send messages
sink.send(Message::Text("Hello".to_string())).await?;

// Receive messages
while let Some(result) = stream.next().await {
    match result {
        Ok(Message::Text(text)) => println!("Received: {}", text),
        Ok(Message::Close(_)) => break,
        Err(e) => eprintln!("Stream error: {}", e),
        _ => {}
    }
}

// Check connection state
let state = sink.connection_state().await;
println!("Connection state: {:?}", state);
```

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

1. **Custom Stream Types**: Implement custom stream types for specialized transports
2. **Message Hooks**: Add custom processing logic for different message types
3. **Error Handling**: Extend `ConnectionError` for domain-specific error types
4. **Connection Strategies**: Customize reconnection and retry logic

## GitHub Actions

The repository includes a GitHub Action (`.github/workflows/claude.yml`) that triggers Claude Code when:
- A label containing "claude" is added to an issue
- The issue creator is "calumrussell" (security constraint for public repo)

This provides automated assistance while maintaining security for the public repository.
