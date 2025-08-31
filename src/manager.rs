use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use chrono::Local;
use dashmap::DashMap;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    connection::{Connection, ConnectionMessage},
};

pub struct Manager {
    conn: DashMap<String, Connection>,
    write_sends: DashMap<String, Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>>,
    global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    read_tracker: DashMap<String, u64>,
    pong_tracker: DashMap<String, i64>,
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    pub fn new() -> Self {
        let (global_send, _global_recv) =
            tokio::sync::broadcast::channel::<ConnectionMessage>(1_028);

        Self {
            conn: DashMap::new(),
            write_sends: DashMap::new(),
            global_send: Arc::new(global_send),
            read_tracker: DashMap::new(),
            pong_tracker: DashMap::new(),
        }
    }

    pub fn read(&self) -> tokio::sync::broadcast::Receiver<ConnectionMessage> {
        self.global_send.subscribe()
    }

    pub async fn write(&self, name: &str, message: ConnectionMessage) {
        if let Some(sender) = self.write_sends.get(name) {
            let _ = sender.send(message);
        }
    }

    pub async fn close_conn(&self, name: &str) {
        if let Some(writer) = self.write_sends.get(name) {
            let _ = writer.send(ConnectionMessage::Message(
                name.to_string(),
                Message::Close(None),
            ));
        }

        if let Some(conn) = self.conn.get(name) {
            conn.cancel_token.cancel();
        }

        self.write_sends.remove(&name.to_string());
        self.conn.remove(&name.to_string());
        self.read_tracker.remove(&name.to_string());
    }

    pub async fn read_timeout_loop(&self) {
        let mut global_recv = self.global_send.subscribe();
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut previous_bytes = HashMap::new();

        loop {
            tokio::select! {
                msg_result = global_recv.recv() => {
                    if let Ok(msg) = msg_result {
                        match msg {
                            ConnectionMessage::Message(conn_name, msg_content) => {
                                let bytes_to_add = msg_content.len() as u64;
                                self.read_tracker
                                    .entry(conn_name)
                                    .and_modify(|bytes| *bytes += bytes_to_add)
                                    .or_insert(bytes_to_add);
                            },
                            _ => (),
                        }
                    }
                }
                _ = interval.tick() => {
                    let current_state: HashMap<String, u64> = self.read_tracker
                        .iter()
                        .map(|entry| (entry.key().clone(), *entry.value()))
                        .collect();

                    for (conn_name, current_bytes) in &current_state {
                        if let Some(prev_bytes) = previous_bytes.get(conn_name)
                            && current_bytes == prev_bytes {
                                self.close_conn(conn_name).await;
                            }
                    }

                    previous_bytes = current_state;
                }
            }
        }
    }

    pub async fn ping_loop(&self, name: &str, config: Config, cancel_token: CancellationToken) {
        let mut ping_duration = tokio::time::interval(Duration::from_secs(config.ping_duration));
        let mut ping_timeout = tokio::time::interval(Duration::from_secs(config.ping_timeout));
        let ping_msg_bytes: tokio_tungstenite::tungstenite::Bytes =
            config.ping_message.clone().into();
        let mut global_recv = self.global_send.subscribe();
        loop {
            tokio::select! {
                _ = ping_duration.tick() => {
                    if let Some(writer) = self.write_sends.get(name) {
                        let _ = writer.send(ConnectionMessage::Message(name.to_string(), Message::Ping(ping_msg_bytes.clone())));
                    }
                },
                _ = cancel_token.cancelled() => break,
                msg_result = global_recv.recv() => {
                    if let Ok(msg) = msg_result {
                        if let ConnectionMessage::Message(name, conn_msg) = msg {
                            if let Message::Pong(val) = conn_msg {
                                let now = Local::now().timestamp();
                                if let Some(mut last_pong) = self.pong_tracker.get_mut(&name) {
                                    *last_pong = now;
                                } else {
                                    self.pong_tracker.insert(name, now);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn new_conn(&self, name: &str, config: Config) -> Result<()> {
        let global_send = Arc::clone(&self.global_send);
        let wrapper = Connection::new(name, config, global_send).await?;

        let writer_send_arc = Arc::clone(&wrapper.writer.sender);
        self.conn.insert(name.to_string(), wrapper);
        self.write_sends.insert(name.to_string(), writer_send_arc);
        Ok(())
    }

    pub async fn reconnect(&self, name: &str) -> Result<()> {
        let config = if let Some(conn) = self.conn.get(name) {
            conn.config.clone()
        } else {
            return Err(anyhow::anyhow!(
                "Connection '{}' not found for reconnect",
                name
            ));
        };

        self.close_conn(name).await;

        tokio::time::sleep(Duration::from_secs(config.reconnect_timeout)).await;

        self.new_conn(name, config).await
    }

    pub async fn pong_loop(&self) {
        let mut global_recv = self.global_send.subscribe();

        loop {
            if let Ok(msg) = global_recv.recv().await {
                match msg {
                    ConnectionMessage::Message(name, msg) => match msg {
                        Message::Ping(val) => {
                            if let Some(conn_writer) = self.write_sends.get(&name) {
                                let _ = conn_writer
                                    .send(ConnectionMessage::Message(name, Message::Pong(val)));
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    pub async fn error_handling_loop(&self) {
        let mut global_recv = self.global_send.subscribe();

        loop {
            if let Ok(msg) = global_recv.recv().await {
                match msg {
                    ConnectionMessage::ReadError(conn_name) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    ConnectionMessage::WriteError(conn_name) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    _ => {}
                }
            }
        }
    }
}
