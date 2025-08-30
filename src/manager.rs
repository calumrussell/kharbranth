use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use dashmap::DashMap;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    config::Config,
    connection::{Connection, ConnectionMessage},
};

pub struct Manager {
    conn: DashMap<String, Connection>,
    write_sends: DashMap<String, Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>>,
    read_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    read_tracker: DashMap<String, u64>,
}

impl Manager {
    pub fn new() -> Self {
        let (read_send, _read_recv) = tokio::sync::broadcast::channel::<ConnectionMessage>(1_028);

        Self {
            conn: DashMap::new(),
            write_sends: DashMap::new(),
            read_send: Arc::new(read_send),
            read_tracker: DashMap::new(),
        }
    }

    pub fn read(&self) -> tokio::sync::broadcast::Receiver<ConnectionMessage> {
        self.read_send.subscribe()
    }

    pub async fn write(&self, name: &str, message: ConnectionMessage) {
        if let Some(sender) = self.write_sends.get(name) {
            let _ = sender.send(message);
        }
    }

    pub async fn close_conn(&self, name: &str) {
        let writer = self.write_sends.get(name).unwrap();
        let _ = writer.send(ConnectionMessage::Message(
            name.to_string(),
            Message::Close(None),
        ));

        self.write_sends.remove(&name.to_string());
        self.conn.remove(&name.to_string());
    }

    pub async fn read_timeout_loop(&self) {
        let mut read_recv = self.read_send.subscribe();
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut previous_bytes = HashMap::new();

        loop {
            tokio::select! {
                msg_result = read_recv.recv() => {
                    if let Ok(msg) = msg_result {
                        match msg {
                            ConnectionMessage::Message(conn_name, msg_content) => {
                                let bytes_to_add = msg_content.len() as u64;
                                self.read_tracker
                                    .entry(conn_name)
                                    .and_modify(|bytes| *bytes += bytes_to_add)
                                    .or_insert(bytes_to_add);
                            }
                        }
                    }
                }
                _ = interval.tick() => {
                    // Collect current state in one pass to avoid deadlock
                    let current_state: HashMap<String, u64> = self.read_tracker
                        .iter()
                        .map(|entry| (entry.key().clone(), *entry.value()))
                        .collect();

                    for (conn_name, current_bytes) in &current_state {
                        if let Some(prev_bytes) = previous_bytes.get(conn_name) {
                            if current_bytes == prev_bytes {
                                self.close_conn(conn_name).await;
                            }
                        }
                    }
                    
                    // Update previous_bytes for next check
                    previous_bytes = current_state;
                }
            }
        }
    }

    pub async fn new_conn(&self, name: &str, config: Config) -> Result<()> {
        let reader_send = Arc::clone(&self.read_send);
        let wrapper = Connection::new(name, config, reader_send).await?;

        let writer_send_arc = Arc::clone(&wrapper.writer.sender);
        self.conn.insert(name.to_string(), wrapper);
        self.write_sends.insert(name.to_string(), writer_send_arc);
        Ok(())
    }
}
