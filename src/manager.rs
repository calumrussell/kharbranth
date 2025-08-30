use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use dashmap::DashMap;
use tokio_tungstenite::tungstenite::Message;

use crate::{
    config::Config, connection::{Connection, ConnectionMessage},
};

#[derive(Clone)]
pub struct Manager {
    conn: DashMap<String, Connection>,
    write_sends: DashMap<String, Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>>,
    read_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    read_tracker: DashMap<String, u64>,

}

impl Manager {
    pub fn new() -> Self {
        let (read_send, _read_recv) = tokio::sync::broadcast::channel::<ConnectionMessage>(8);

        Self {
            conn: DashMap::new(),
            write_sends: DashMap::new(),
            read_send: Arc::new(read_send),
            read_tracker: DashMap::new(),
        }
    }

    pub async fn read(&self) {
        let mut read_recv = self.read_send.subscribe();
        while let Ok(msg) = read_recv.recv().await {
            match msg {
                ConnectionMessage::Message(name, msg) => {
                    let bytes_len = msg.len();

                    if let Some(mut bytes) = self.read_tracker.get_mut(&name) {
                        *bytes += bytes_len as u64;
                    } else {
                        self.read_tracker.insert(name.clone(), bytes_len as u64);
                    }

                    println!("{:?}", msg);
                }
            }
        }
    }

    pub async fn write(&self, name: &str, message: ConnectionMessage) {
        if let Some(sender) = self.write_sends.get(name) {
            let _ = sender.send(message);
        }
    }

    pub async fn close_conn(&self, name: &str) {
        let writer = self.write_sends.get(name).unwrap();
        let _ = writer.send(ConnectionMessage::Message(name.to_string(), Message::Close(None)));

        self.write_sends.remove(&name.to_string());
        self.conn.remove(&name.to_string());
    }

    pub async fn read_timeout_loop(&self) {
        loop {
            let mut checker = HashMap::new();

            for conn in self.read_tracker.iter() {
                let name = conn.key().clone();
                let bytes_recv_now = conn.value().clone();
                if !checker.contains_key(&name) {
                    checker.insert(name, bytes_recv_now);
                } else {
                    let bytes_recv_last = checker.get(&name).unwrap();
                    if bytes_recv_now == *bytes_recv_last {
                        self.close_conn(&name).await;
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    pub async fn new_conn(&mut self, name: &str, config: Config) -> Result<()> {
        let reader_send = Arc::clone(&self.read_send);
        let wrapper = Connection::new(name, config, reader_send).await?;

        let writer_send_arc = Arc::clone(&wrapper.writer.sender);
        self.conn.insert(name.to_string(), wrapper);
        self.write_sends.insert(name.to_string(), writer_send_arc);
        Ok(())
    }
}
