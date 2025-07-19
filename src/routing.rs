use std::{
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
    future::Future,
    pin::Pin,
};

use anyhow::Result;
use dashmap::DashMap;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Unique identifier for a subscription
pub type SubscriptionId = usize;

/// Types of messages that can be subscribed to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscriptionType {
    TextMessages,
    BinaryMessages,
    ConnectionEvents,
    ErrorEvents,
}

/// Connection state events that subscribers can receive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionEvent {
    Connected,
    Disconnected,
    Reconnecting,
    Error(String),
}

/// Error events for connection issues
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorEvent {
    PingTimeout,
    ReadError(String),
    WriteError(String),
    ConnectionDropped,
}

/// Simple message handler trait - takes just a string for simplicity
/// This maintains the abstraction by hiding WebSocket Message types completely
pub trait MessageHandler: Send + Sync {
    /// Handle a text message - this is the main use case for crypto exchanges
    fn handle_text(&self, message: &str) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// Convenience handler for simple async functions  
pub struct AsyncHandler<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    handler: F,
}

impl<F, Fut> AsyncHandler<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    pub fn new(handler: F) -> Self {
        Self { handler }
    }
}

impl<F, Fut> MessageHandler for AsyncHandler<F, Fut>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    fn handle_text(&self, message: &str) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin((self.handler)(message.to_string()))
    }
}

/// Backpressure policies for handling message queue overflow
#[derive(Debug, Clone, Copy)]
pub enum BackpressurePolicy {
    /// Drop the oldest messages when queue is full
    DropOldest,
    /// Drop the newest message when queue is full
    DropNewest,
    /// Fail fast with an error when queue is full
    FailFast,
}

/// Statistics about message processing
#[derive(Debug, Clone)]
pub struct QueueStats {
    pub pending: usize,
    pub total_received: u64,
    pub total_dropped: u64,
    pub total_processed: u64,
}

/// Configuration for connection-specific message handling
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub backpressure_policy: BackpressurePolicy,
    pub max_pending_messages: usize,
    pub warning_threshold: usize,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            backpressure_policy: BackpressurePolicy::DropOldest,
            max_pending_messages: 1000,
            warning_threshold: 800,
        }
    }
}

/// A single subscription entry
struct Subscription {
    id: SubscriptionId,
    handler: Arc<dyn MessageHandler>,
}

/// Per-connection message router - handles subscriptions for a single connection
pub struct ConnectionRouter {
    text_subscriptions: Arc<DashMap<SubscriptionId, Subscription>>,
    connection_event_subscriptions: Arc<DashMap<SubscriptionId, Subscription>>,
    config: ConnectionConfig,
    next_id: AtomicUsize,
    stats: Arc<RwLock<QueueStats>>,
}

impl ConnectionRouter {
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            text_subscriptions: Arc::new(DashMap::new()),
            connection_event_subscriptions: Arc::new(DashMap::new()),
            config,
            next_id: AtomicUsize::new(0),
            stats: Arc::new(RwLock::new(QueueStats {
                pending: 0,
                total_received: 0,
                total_dropped: 0,
                total_processed: 0,
            })),
        }
    }

    /// Subscribe to text messages from this connection
    pub async fn subscribe_text<H>(&self, handler: H) -> Result<SubscriptionId>
    where
        H: MessageHandler + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let subscription = Subscription {
            id,
            handler: Arc::new(handler),
        };
        self.text_subscriptions.insert(id, subscription);
        debug!("Added text subscription {}", id);
        Ok(id)
    }

    /// Subscribe to connection events from this connection
    pub async fn subscribe_connection_events<H>(&self, handler: H) -> Result<SubscriptionId>
    where
        H: MessageHandler + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let subscription = Subscription {
            id,
            handler: Arc::new(handler),
        };
        self.connection_event_subscriptions.insert(id, subscription);
        debug!("Added connection event subscription {}", id);
        Ok(id)
    }

    /// Unsubscribe from a specific subscription
    pub fn unsubscribe(&self, subscription_id: SubscriptionId) -> bool {
        let removed_text = self.text_subscriptions.remove(&subscription_id).is_some();
        let removed_connection = self.connection_event_subscriptions.remove(&subscription_id).is_some();
        removed_text || removed_connection
    }

    /// Route a text message to all text subscribers
    pub async fn route_text_message(&self, text: &str) -> Result<()> {
        let mut stats_guard = self.stats.write().await;
        stats_guard.total_received += 1;

        // Check backpressure
        if stats_guard.pending >= self.config.max_pending_messages {
            match self.config.backpressure_policy {
                BackpressurePolicy::DropOldest | BackpressurePolicy::DropNewest => {
                    warn!("Message queue full, dropping message");
                    stats_guard.total_dropped += 1;
                    return Ok(());
                }
                BackpressurePolicy::FailFast => {
                    return Err(anyhow::anyhow!("Message queue full, failing fast"));
                }
            }
        }

        // Send to all text subscribers
        let mut futures = Vec::new();
        for entry in self.text_subscriptions.iter() {
            let handler = Arc::clone(&entry.value().handler);
            let text_copy = text.to_string();
            let future = async move {
                handler.handle_text(&text_copy).await;
            };
            futures.push(future);
        }

        let futures_count = futures.len();
        stats_guard.pending += futures_count;
        drop(stats_guard);

        // Execute all handlers concurrently
        if !futures.is_empty() {
            futures_util::future::join_all(futures).await;
            
            // Update processed count
            let mut stats_guard = self.stats.write().await;
            stats_guard.total_processed += futures_count as u64;
            stats_guard.pending = stats_guard.pending.saturating_sub(futures_count);
        }

        Ok(())
    }

    /// Route a connection event to all connection event subscribers
    pub async fn route_connection_event(&self, event: ConnectionEvent) -> Result<()> {
        // For now, connection events can be represented as JSON strings
        let event_json = serde_json::to_string(&event)
            .map_err(|e| anyhow::anyhow!("Failed to serialize connection event: {}", e))?;

        let mut futures = Vec::new();
        for entry in self.connection_event_subscriptions.iter() {
            let handler = Arc::clone(&entry.value().handler);
            let event_copy = event_json.clone();
            let future = async move {
                handler.handle_text(&event_copy).await;
            };
            futures.push(future);
        }

        // Execute all handlers concurrently
        if !futures.is_empty() {
            futures_util::future::join_all(futures).await;
        }

        Ok(())
    }

    /// Get current queue statistics
    pub async fn get_stats(&self) -> QueueStats {
        self.stats.read().await.clone()
    }

    /// Get the number of active subscriptions
    pub fn subscription_count(&self) -> usize {
        self.text_subscriptions.len() + self.connection_event_subscriptions.len()
    }
}