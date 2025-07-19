# Migration Guide: ReadHooks to Enhanced Message Routing

This guide helps you migrate from the deprecated `ReadHooks` system to the new subscription-based message routing API introduced in issue #22.

## Overview

The new enhanced message routing system provides several advantages over ReadHooks:

1. **Maintains Abstraction**: Hides WebSocket complexity behind a simple subscription API
2. **Built-in Backpressure**: Configurable policies for handling high-throughput scenarios  
3. **Connection Transparency**: Reconnections happen invisibly to user code
4. **Event-Driven**: React to connection state changes without polling
5. **Zero Boilerplate**: No message loops, error handling, or reconnection logic needed

## Key Changes

### Before (ReadHooks)
```rust
use kharbranth::{WSManager, Config, ReadHooks};

let manager = WSManager::new();
let mut hooks = ReadHooks::new();

// Manual callback setup
hooks.on_text = Arc::new(Some(Box::new(|text| {
    handle_trade_data(text.to_string());
})));

hooks.on_close = Arc::new(Some(Box::new(|_frame| {
    log::info!("Connection closed");
})));

manager.new_conn("exchange", config, hooks).await;
```

### After (Enhanced Routing)
```rust
use kharbranth::{WSManager, Config, ReadHooks, AsyncHandler};

let manager = WSManager::new();
let hooks = ReadHooks::new(); // Still needed for backward compatibility

manager.new_conn("exchange", config, hooks).await;

// Simple subscription-based API
let subscription_id = manager.subscribe("exchange", AsyncHandler::new(|text| async move {
    handle_trade_data(text);
})).await?;

let _event_subscription = manager.subscribe_connection_events("exchange", AsyncHandler::new(|event| async move {
    log::info!("Connection event: {}", event);
})).await?;
```

## Migration Steps

### Step 1: Update Dependencies

The new API is backward compatible, so no dependency changes are needed.

### Step 2: Replace ReadHooks with Subscriptions

#### For Text Message Handling

**Old:**
```rust
let mut hooks = ReadHooks::new();
hooks.on_text = Arc::new(Some(Box::new(|text| {
    process_message(text.to_string());
})));
manager.new_conn("conn", config, hooks).await;
```

**New:**
```rust
let hooks = ReadHooks::new(); // Empty hooks for backward compatibility
manager.new_conn("conn", config, hooks).await;

// Subscribe to text messages
let handler = AsyncHandler::new(|text| async move {
    process_message(text);
});
let subscription_id = manager.subscribe("conn", handler).await?;
```

#### For Connection Events

**Old:**
```rust
hooks.on_close = Arc::new(Some(Box::new(|_frame| {
    log::warn!("Connection closed");
})));
hooks.on_ping = Arc::new(Some(Box::new(|_payload| {
    log::debug!("Ping received");
})));
```

**New:**
```rust
// Connection events are automatically handled and can be subscribed to
let event_handler = AsyncHandler::new(|event_json| async move {
    // Events are delivered as JSON strings
    if let Ok(event) = serde_json::from_str::<ConnectionEvent>(&event_json) {
        match event {
            ConnectionEvent::Connected => log::info!("Connected"),
            ConnectionEvent::Disconnected => log::warn!("Connection closed"),
            ConnectionEvent::Error(err) => log::error!("Connection error: {}", err),
            _ => {}
        }
    }
});
manager.subscribe_connection_events("conn", event_handler).await?;
```

### Step 3: Configure Backpressure (Optional)

The new system supports configurable backpressure management:

```rust
use kharbranth::{ConnectionConfig, BackpressurePolicy};

let router_config = ConnectionConfig {
    backpressure_policy: BackpressurePolicy::DropOldest,
    max_pending_messages: 1000,
    warning_threshold: 800,
};

let hooks = ReadHooks::new();
manager.new_conn_with_router_config("high_throughput", config, hooks, router_config).await;
```

### Step 4: Monitor and Manage Subscriptions

```rust
// Get subscription statistics
let stats = manager.get_connection_stats("conn").await?;
println!("Processed: {}, Dropped: {}", stats.total_processed, stats.total_dropped);

// Unsubscribe when no longer needed
manager.unsubscribe("conn", subscription_id)?;
```

## Full Example Migration

### Before (ReadHooks)
```rust
use std::{sync::Arc, time::Duration};
use kharbranth::{WSManager, Config, ReadHooks};
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let manager = WSManager::new();
    let mut hooks = ReadHooks::new();
    
    // Setup callbacks
    hooks.on_text = Arc::new(Some(Box::new(|text| {
        println!("Trade data: {}", text);
    })));
    
    hooks.on_close = Arc::new(Some(Box::new(|_| {
        println!("Connection closed");
    })));
    
    let config = Config {
        url: "wss://api.exchange.com/ws".to_string(),
        ping_duration: 30,
        ping_timeout: 10,
        reconnect_timeout: 5,
        write_on_init: vec![],
    };
    
    manager.new_conn("exchange", config, hooks).await;
    
    let (tx, _rx) = broadcast::channel(16);
    let _handles = manager.start(tx);
    
    // Keep alive
    tokio::time::sleep(Duration::from_secs(3600)).await;
    Ok(())
}
```

### After (Enhanced Routing)
```rust
use std::time::Duration;
use kharbranth::{WSManager, Config, ReadHooks, AsyncHandler, ConnectionEvent};
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let manager = WSManager::new();
    let hooks = ReadHooks::new(); // Empty for backward compatibility
    
    let config = Config {
        url: "wss://api.exchange.com/ws".to_string(),
        ping_duration: 30,
        ping_timeout: 10,
        reconnect_timeout: 5,
        write_on_init: vec![],
    };
    
    manager.new_conn("exchange", config, hooks).await;
    
    // Subscribe to trade data
    let _trade_subscription = manager.subscribe("exchange", AsyncHandler::new(|text| async move {
        println!("Trade data: {}", text);
    })).await?;
    
    // Subscribe to connection events
    let _event_subscription = manager.subscribe_connection_events("exchange", AsyncHandler::new(|event| async move {
        println!("Connection event: {}", event);
    })).await?;
    
    let (tx, _rx) = broadcast::channel(16);
    let _handles = manager.start(tx);
    
    // Monitor statistics
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Ok(stats) = manager.get_connection_stats("exchange").await {
                println!("Stats - Processed: {}, Dropped: {}", 
                         stats.total_processed, stats.total_dropped);
            }
        }
    });
    
    // Keep alive
    tokio::time::sleep(Duration::from_secs(3600)).await;
    Ok(())
}
```

## Benefits of Migration

1. **Cleaner Code**: No more Arc<Option<Box<dyn Fn>>> wrappers
2. **Async Native**: Handlers can be async functions
3. **Better Error Handling**: Subscription methods return Results
4. **Observability**: Built-in statistics and monitoring
5. **Backpressure Control**: Handle high-throughput scenarios gracefully
6. **Type Safety**: Strongly typed events instead of raw WebSocket frames

## Backward Compatibility

The old ReadHooks system remains functional to ensure smooth migration. You can:

1. Migrate gradually by keeping ReadHooks while adding subscriptions
2. Run both systems simultaneously during transition
3. Remove ReadHooks once fully migrated to the new API

The ReadHooks system will be removed in a future major version release.

## Common Patterns

### Multiple Subscribers
```rust
// Multiple handlers can subscribe to the same connection
let parser1 = manager.subscribe("feed", AsyncHandler::new(|text| async move {
    parse_trades(text).await;
})).await?;

let logger = manager.subscribe("feed", AsyncHandler::new(|text| async move {
    log_message(text).await;
})).await?;
```

### Error Handling
```rust
let handler = AsyncHandler::new(|text| async move {
    if let Err(e) = process_message(text).await {
        log::error!("Failed to process message: {}", e);
    }
});
```

### Resource Cleanup
```rust
// Always unsubscribe when done
let subscription_id = manager.subscribe("conn", handler).await?;

// Later...
manager.unsubscribe("conn", subscription_id)?;
```

For more examples, see the test cases in `src/lib.rs` under the `test` module.