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

pub type SubscriptionId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscriptionType {
    TextMessages,
    BinaryMessages,
    ConnectionEvents,
    ErrorEvents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConnectionEvent {
    Connected,
    Disconnected,
    Reconnecting,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorEvent {
    PingTimeout,
    ReadError(String),
    WriteError(String),
    ConnectionDropped,
}

pub trait MessageHandler: Send + Sync {
    fn handle_text(&self, message: &str) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}  
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

#[derive(Debug, Clone, Copy)]
pub enum BackpressurePolicy {
    DropOldest,
    DropNewest,
    FailFast,
}

#[derive(Debug, Clone)]
pub struct QueueStats {
    pub pending: usize,
    pub total_received: u64,
    pub total_dropped: u64,
    pub total_processed: u64,
}

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

struct Subscription {
    id: SubscriptionId,
    handler: Arc<dyn MessageHandler>,
}

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

    pub fn unsubscribe(&self, subscription_id: SubscriptionId) -> bool {
        let removed_text = self.text_subscriptions.remove(&subscription_id).is_some();
        let removed_connection = self.connection_event_subscriptions.remove(&subscription_id).is_some();
        removed_text || removed_connection
    }

    pub async fn route_text_message(&self, text: &str) -> Result<()> {
        let mut stats_guard = self.stats.write().await;
        stats_guard.total_received += 1;

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

        if !futures.is_empty() {
            futures_util::future::join_all(futures).await;
            
            let mut stats_guard = self.stats.write().await;
            stats_guard.total_processed += futures_count as u64;
            stats_guard.pending = stats_guard.pending.saturating_sub(futures_count);
        }

        Ok(())
    }

    pub async fn route_connection_event(&self, event: ConnectionEvent) -> Result<()> {
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

        if !futures.is_empty() {
            futures_util::future::join_all(futures).await;
        }

        Ok(())
    }

    pub async fn get_stats(&self) -> QueueStats {
        self.stats.read().await.clone()
    }

    pub fn subscription_count(&self) -> usize {
        self.text_subscriptions.len() + self.connection_event_subscriptions.len()
    }
}