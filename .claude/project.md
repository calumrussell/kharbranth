# Kharbranth Project Purpose and Architecture

## Overview

Kharbranth is a robust WebSocket connection management library written in Rust. It provides a high-level abstraction for managing multiple WebSocket connections with built-in reconnection logic, heartbeat mechanisms, and flexible message handling.

## Core Purpose

The library addresses common challenges in WebSocket client applications:

1. **Connection Reliability**: Automatic reconnection with configurable timeouts
2. **Connection Pooling**: Manage multiple named connections simultaneously  
3. **Heartbeat Management**: Built-in ping/pong mechanism to detect connection health
4. **Flexible Message Handling**: Hook-based system for processing different message types
5. **Error Resilience**: Comprehensive error handling and recovery mechanisms

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

#### Hook-Based Message Processing
Instead of forcing a specific message processing pattern, the library provides hooks (`ReadHooks`) that allow users to define custom handlers for different WebSocket frame types.

#### Shared State Management
Uses `Arc<RwLock<>>` pattern for thread-safe shared state between async tasks, enabling concurrent read/write operations while maintaining memory safety.

## Use Cases

This is used to subscribe to crypto exchange websocket feeds.

## Extension Points

The library is designed for extensibility:

1. **Custom Stream Types**: Implement custom stream types for specialized transports
2. **Message Hooks**: Add custom processing logic for different message types
3. **Error Handling**: Extend `ConnectionError` for domain-specific error types
4. **Connection Strategies**: Customize reconnection and retry logic

## Performance Characteristics

No hard performance requirements but should reduce the number of copies, should be resilient to faults allowing callers to trigger restarts when feed moves into bad state, scalable to many websocket connections, and should be thread-safe.

