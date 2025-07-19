use std::{
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
    future::Future,
    pin::Pin,
};

use anyhow::Result;
use dashmap::DashMap;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, mpsc};

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
    sender: mpsc::Sender<String>,
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
        let handler = Arc::new(handler);
        
        let (sender, mut receiver) = mpsc::channel(self.config.max_pending_messages);
        
        let subscription = Subscription {
            id,
            handler: handler.clone(),
            sender,
        };
        
        self.text_subscriptions.insert(id, subscription);
        
        let stats = Arc::clone(&self.stats);
        tokio::spawn(async move {
            while let Some(text) = receiver.recv().await {
                handler.handle_text(&text).await;
                
                let mut stats_guard = stats.write().await;
                stats_guard.total_processed += 1;
                stats_guard.pending = stats_guard.pending.saturating_sub(1);
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
        
        let (sender, mut receiver) = mpsc::channel(100);
        
        let subscription = Subscription {
            id,
            handler: handler.clone(),
            sender,
        };
        
        self.connection_event_subscriptions.insert(id, subscription);
        
        tokio::spawn(async move {
            while let Some(event_json) = receiver.recv().await {
                handler.handle_text(&event_json).await;
            }
            debug!("Connection event subscription {} handler task completed", id);
        });
        
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
        drop(stats_guard);

        let mut _messages_sent = 0;
        let mut messages_dropped = 0;

        for entry in self.text_subscriptions.iter() {
            let subscription = entry.value();
            
            match subscription.sender.try_send(text.to_string()) {
                Ok(_) => {
                    _messages_sent += 1;
                    let mut stats_guard = self.stats.write().await;
                    stats_guard.pending += 1;
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    match self.config.backpressure_policy {
                        BackpressurePolicy::DropOldest => {
                            warn!("Message queue full for subscription {}, dropping newest message", subscription.id);
                            messages_dropped += 1;
                        }
                        BackpressurePolicy::DropNewest => {
                            warn!("Message queue full for subscription {}, dropping newest message", subscription.id);
                            messages_dropped += 1;
                        }
                        BackpressurePolicy::FailFast => {
                            return Err(anyhow::anyhow!("Message queue full for subscription {}, failing fast", subscription.id));
                        }
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("Subscription {} channel closed, removing", subscription.id);
                    self.text_subscriptions.remove(&subscription.id);
                }
            }
        }

        if messages_dropped > 0 {
            let mut stats_guard = self.stats.write().await;
            stats_guard.total_dropped += messages_dropped;
        }

        Ok(())
    }

    pub async fn route_connection_event(&self, event: ConnectionEvent) -> Result<()> {
        let event_json = serde_json::to_string(&event)
            .map_err(|e| anyhow::anyhow!("Failed to serialize connection event: {}", e))?;

        for entry in self.connection_event_subscriptions.iter() {
            let subscription = entry.value();
            
            match subscription.sender.try_send(event_json.clone()) {
                Ok(_) => {
                    debug!("Sent connection event to subscription {}", subscription.id);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("Connection event queue full for subscription {}, dropping event", subscription.id);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    debug!("Connection event subscription {} channel closed, removing", subscription.id);
                    self.connection_event_subscriptions.remove(&subscription.id);
                }
            }
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