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
    config: ConnectionConfig,
    next_id: AtomicUsize,
    stats: Arc<AtomicStats>,
}

impl ConnectionRouter {
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            config,
            next_id: AtomicUsize::new(0),
            stats: Arc::new(AtomicStats::new()),
        }
    }

    pub async fn get_stats(&self) -> QueueStats {
        self.stats.get_snapshot()
    }

}
