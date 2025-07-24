use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use dashmap::DashMap;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

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
    fn handle_text(&self, message: Arc<str>) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
pub struct AsyncHandler<F, Fut>
where
    F: Fn(Arc<str>) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    handler: F,
}

impl<F, Fut> AsyncHandler<F, Fut>
where
    F: Fn(Arc<str>) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    pub fn new(handler: F) -> Self {
        Self { handler }
    }
}

impl<F, Fut> MessageHandler for AsyncHandler<F, Fut>
where
    F: Fn(Arc<str>) -> Fut + Send + Sync,
    Fut: Future<Output = ()> + Send,
{
    fn handle_text(&self, message: Arc<str>) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin((self.handler)(message))
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

struct AtomicStats {
    pending: AtomicUsize,
    total_received: AtomicU64,
    total_dropped: AtomicU64,
    total_processed: AtomicU64,
}

impl AtomicStats {
    fn new() -> Self {
        Self {
            pending: AtomicUsize::new(0),
            total_received: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
            total_processed: AtomicU64::new(0),
        }
    }

    fn get_snapshot(&self) -> QueueStats {
        QueueStats {
            pending: self.pending.load(Ordering::Relaxed),
            total_received: self.total_received.load(Ordering::Relaxed),
            total_dropped: self.total_dropped.load(Ordering::Relaxed),
            total_processed: self.total_processed.load(Ordering::Relaxed),
        }
    }
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

struct BackpressureQueue {
    queue: Arc<Mutex<VecDeque<Arc<str>>>>,
    notify: Arc<Notify>,
    capacity: usize,
    policy: BackpressurePolicy,
}

impl BackpressureQueue {
    fn new(capacity: usize, policy: BackpressurePolicy) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
            capacity,
            policy,
        }
    }

    async fn send(&self, message: Arc<str>) -> Result<bool> {
        let mut queue = self.queue.lock().await;

        if queue.len() >= self.capacity {
            match self.policy {
                BackpressurePolicy::DropOldest => {
                    queue.pop_front(); // Remove oldest
                    queue.push_back(message);
                    self.notify.notify_one();
                    Ok(false) // Indicates a message was dropped
                }
                BackpressurePolicy::DropNewest => {
                    // Don't add the new message
                    Ok(false) // Indicates message was dropped
                }
                BackpressurePolicy::FailFast => {
                    return Err(anyhow::anyhow!("Queue is full"));
                }
            }
        } else {
            queue.push_back(message);
            self.notify.notify_one();
            Ok(true) // Message was successfully queued
        }
    }

    async fn recv(&self) -> Option<Arc<str>> {
        loop {
            {
                let mut queue = self.queue.lock().await;
                if let Some(message) = queue.pop_front() {
                    return Some(message);
                }
            }
            self.notify.notified().await;
        }
    }
}

struct Subscription {
    id: SubscriptionId,
    handler: Arc<dyn MessageHandler>,
    queue: Arc<BackpressureQueue>,
}

pub struct ConnectionRouter {
    text_subscriptions: Arc<DashMap<SubscriptionId, Subscription>>,
    connection_event_subscriptions: Arc<DashMap<SubscriptionId, Subscription>>,
    config: ConnectionConfig,
    next_id: AtomicUsize,
    stats: Arc<AtomicStats>,
}

impl ConnectionRouter {
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            text_subscriptions: Arc::new(DashMap::new()),
            connection_event_subscriptions: Arc::new(DashMap::new()),
            config,
            next_id: AtomicUsize::new(0),
            stats: Arc::new(AtomicStats::new()),
        }
    }

    pub async fn subscribe_text<H>(&self, handler: H) -> Result<SubscriptionId>
    where
        H: MessageHandler + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handler = Arc::new(handler);

        let queue = Arc::new(BackpressureQueue::new(
            self.config.max_pending_messages,
            self.config.backpressure_policy,
        ));

        let subscription = Subscription {
            id,
            handler: handler.clone(),
            queue: Arc::clone(&queue),
        };

        self.text_subscriptions.insert(id, subscription);

        let stats = Arc::clone(&self.stats);
        tokio::spawn(async move {
            loop {
                if let Some(text) = queue.recv().await {
                    handler.handle_text(text).await;

                    stats.total_processed.fetch_add(1, Ordering::Relaxed);
                    stats.pending.fetch_sub(1, Ordering::Relaxed);
                } else {
                    break;
                }
            }
            debug!("Text subscription {} handler task completed", id);
        });

        debug!("Added text subscription {}", id);
        Ok(id)
    }

    pub async fn subscribe_connection_events<H>(&self, handler: H) -> Result<SubscriptionId>
    where
        H: MessageHandler + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handler = Arc::new(handler);

        let queue = Arc::new(BackpressureQueue::new(
            100, // Connection events use smaller queue
            self.config.backpressure_policy,
        ));

        let subscription = Subscription {
            id,
            handler: handler.clone(),
            queue: Arc::clone(&queue),
        };

        self.connection_event_subscriptions.insert(id, subscription);

        tokio::spawn(async move {
            loop {
                if let Some(event_json) = queue.recv().await {
                    handler.handle_text(event_json).await;
                } else {
                    break;
                }
            }
            debug!(
                "Connection event subscription {} handler task completed",
                id
            );
        });

        debug!("Added connection event subscription {}", id);
        Ok(id)
    }

    pub fn unsubscribe(&self, subscription_id: SubscriptionId) -> bool {
        let removed_text = self.text_subscriptions.remove(&subscription_id).is_some();
        let removed_connection = self
            .connection_event_subscriptions
            .remove(&subscription_id)
            .is_some();
        removed_text || removed_connection
    }

    pub async fn route_text_message(&self, text: &str) -> Result<()> {
        self.stats.total_received.fetch_add(1, Ordering::Relaxed);

        let mut _messages_sent = 0;
        let mut messages_dropped = 0;
        let shared_text: Arc<str> = text.into();

        for entry in self.text_subscriptions.iter() {
            let subscription = entry.value();

            match subscription.queue.send(Arc::clone(&shared_text)).await {
                Ok(true) => {
                    _messages_sent += 1;
                    self.stats.pending.fetch_add(1, Ordering::Relaxed);
                }
                Ok(false) => {
                    // Message was dropped due to backpressure policy
                    messages_dropped += 1;
                    warn!(
                        "Message dropped for subscription {} due to backpressure",
                        subscription.id
                    );
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        if messages_dropped > 0 {
            self.stats
                .total_dropped
                .fetch_add(messages_dropped, Ordering::Relaxed);
        }

        Ok(())
    }

    pub async fn route_connection_event(&self, event: ConnectionEvent) -> Result<()> {
        let event_json = serde_json::to_string(&event)
            .map_err(|e| anyhow::anyhow!("Failed to serialize connection event: {}", e))?;
        let shared_event: Arc<str> = event_json.into();

        for entry in self.connection_event_subscriptions.iter() {
            let subscription = entry.value();

            match subscription.queue.send(Arc::clone(&shared_event)).await {
                Ok(true) => {
                    debug!("Sent connection event to subscription {}", subscription.id);
                }
                Ok(false) => {
                    warn!(
                        "Connection event dropped for subscription {} due to backpressure",
                        subscription.id
                    );
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    pub async fn get_stats(&self) -> QueueStats {
        self.stats.get_snapshot()
    }

    pub fn subscription_count(&self) -> usize {
        self.text_subscriptions.len() + self.connection_event_subscriptions.len()
    }
}
