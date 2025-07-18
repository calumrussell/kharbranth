# Rust Language Guide for Kharbranth

## Project Structure

This project is a Rust library crate with the following key components:

### Logging
- **log**: Logging framework
- **env_logger**: Used with log 

Log levels should be used to enable debugging. Key paths should be covered by logs.

### Websockets
- **tokio-tungstenite**: WebSocket client/server implementation
- **tokio**: Async runtime with full features
- **futures-util**: Stream and sink utilities

### Errors
- **anyhow**: Error handling
- **thiserror**: Error derive macros

### Core Types

#### `Connection<S>`
Generic WebSocket connection wrapper that manages:
- Read/write stream splitting
- Ping/pong heartbeat mechanism
- Message hooks for different WebSocket frame types
- Connection lifecycle management

#### `WSManager`
Connection manager that handles:
- Multiple named connections in a HashMap
- Connection pooling with Arc<RwLock<>>
- Automatic reconnection logic
- Broadcasting system for connection control

#### `ReadHooks`
Callback system for handling different WebSocket message types:
- `on_text`: Text message handler
- `on_binary`: Binary message handler  
- `on_ping`/`on_pong`: Ping/pong handlers
- `on_close`: Close frame handler
- `on_frame`: Raw frame handler

#### `ConnectionError`
Custom error types for WebSocket operations:
- `PingFailed`: Ping operation failed
- `PongReceiveTimeout`: Pong response timeout
- `ReadError`/`WriteError`: I/O errors
- `ConnectionDropped`: Connection lost
- `ConnectionNotFound`: Named connection not found

## Key Patterns

WsManager is the key abstraction used by callers. This should hide the details of the underlying websocket connection. 

Users name a connection, WsManager supports read and write functions against the named connections.

Users interact through named connections and WsManager supports write functions.

Config options provide constant behaviour. ReadHooks provide a way to hook into read operations. Callers are able to restart connections through a tokio channel.

## Build Configuration
- Rust 2024 edition
- Library crate with `src/lib.rs` as entry point
- No binary targets defined
