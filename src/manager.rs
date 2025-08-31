use std::{sync::Arc, time::Duration};

use anyhow::Result;
use dashmap::DashMap;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    connection::{Connection, ConnectionMessage},
};

pub struct Manager {
    conn: DashMap<String, Connection>,
    write_sends: DashMap<String, Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>>,
    global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    cancel_tokens: DashMap<String, CancellationToken>,
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
            cancel_tokens: DashMap::new(),
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

        if let Some(cancel_token) = self.cancel_tokens.get(name) {
            cancel_token.cancel();
        }

        self.write_sends.remove(&name.to_string());
        self.conn.remove(&name.to_string());
        self.cancel_tokens.remove(&name.to_string());
    }

    pub async fn new_conn(&self, name: &str, config: Config) -> Result<()> {
        let global_send = Arc::clone(&self.global_send);
        let cancel_token = CancellationToken::new();
        let wrapper = Connection::new(name, config.clone(), global_send, cancel_token.clone()).await?;

        let writer_send_arc = Arc::clone(&wrapper.writer.sender);
        self.conn.insert(name.to_string(), wrapper);
        self.write_sends.insert(name.to_string(), writer_send_arc);
        self.cancel_tokens.insert(name.to_string(), cancel_token.clone());
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
            if let Ok(ConnectionMessage::Message(name, Message::Ping(val))) = global_recv.recv().await
                && let Some(conn_writer) = self.write_sends.get(&name) {
                let _ = conn_writer
                    .send(ConnectionMessage::Message(name, Message::Pong(val)));
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
                    ConnectionMessage::PongReceiveTimeoutError(conn_name) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    _ => {}
                }
            }
        }
    }
}
