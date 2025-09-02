use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use dashmap::DashMap;
use log::error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{config::Config, connection::Connection};

#[derive(Clone, Debug)]
pub enum Message {
    PingMessage(String, String),
    PongMessage(String, String),
    TextMessage(String, String),
    BinaryMessage(String, Vec<u8>),
    CloseMessage(String, Option<String>),
    FrameMessage(String, String),
    ReadError(String, String),
    WriteError(String, String),
    PongReceiveTimeoutError(String),
    SuccessfulHandshake(String),
}

pub struct Manager {
    conn: DashMap<String, Connection>,
    write_sends: DashMap<String, Arc<tokio::sync::broadcast::Sender<Message>>>,
    global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
    cancel_tokens: DashMap<String, CancellationToken>,
    loops_started: AtomicBool,
    pong_handle: OnceLock<JoinHandle<()>>,
    error_handle: OnceLock<JoinHandle<()>>,
}

impl Manager {
    pub fn new() -> Arc<Self> {
        let (global_send, _global_recv) = tokio::sync::broadcast::channel::<Message>(4_096);

        Arc::new(Self {
            conn: DashMap::new(),
            write_sends: DashMap::new(),
            global_send: Arc::new(global_send),
            cancel_tokens: DashMap::new(),
            loops_started: AtomicBool::new(false),
            pong_handle: OnceLock::new(),
            error_handle: OnceLock::new(),
        })
    }

    fn ensure_loops_started(self: &Arc<Self>) {
        if self
            .loops_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let manager_clone1 = Arc::clone(self);
            let _pong_handle = tokio::spawn(async move {
                manager_clone1.pong_loop().await;
            });

            let manager_clone2 = Arc::clone(self);
            let _error_handle = tokio::spawn(async move {
                manager_clone2.error_handling_loop().await;
            });

            let _ = self.pong_handle.set(_pong_handle);
            let _ = self.error_handle.set(_error_handle);
        }
    }

    pub fn read(self: &Arc<Self>) -> tokio::sync::broadcast::Receiver<Message> {
        self.global_send.subscribe()
    }

    pub async fn write(self: &Arc<Self>, name: &str, message: Message) {
        println!("Write called: {:?}, {:?}", name, message);
        if let Some(sender) = self.write_sends.get(name) {
            println!("Through the lock");
            if let Err(e) = sender.try_send(message) {
                println!("Write send failed: {:?}", e);
            }
        }
    }

    pub async fn close_conn(self: &Arc<Self>, name: &str) {
        if let Some(writer) = self.write_sends.get(name) {
            let _ = writer.send(Message::CloseMessage(name.to_string(), None));
        }

        if let Some(cancel_token) = self.cancel_tokens.get(name) {
            cancel_token.cancel();
        }

        self.write_sends.remove(&name.to_string());
        self.conn.remove(&name.to_string());
        self.cancel_tokens.remove(&name.to_string());
    }

    pub async fn new_conn(self: &Arc<Self>, name: &str, config: Config) {
        self.ensure_loops_started();
        let global_send = Arc::clone(&self.global_send);
        let cancel_token = CancellationToken::new();

        match Connection::new(name, config.clone(), global_send, cancel_token.clone()).await {
            Ok(wrapper) => {
                let writer_send_arc = Arc::clone(&wrapper.writer.sender);
                self.conn.insert(name.to_string(), wrapper);
                self.write_sends.insert(name.to_string(), writer_send_arc);
                self.cancel_tokens
                    .insert(name.to_string(), cancel_token.clone());

                let _ = self.global_send.send(Message::SuccessfulHandshake(name.to_string()));
            }
            Err(e) => {
                error!("Failed to create connection '{}': {}", name, e);
            }
        }
    }

    pub async fn reconnect(self: &Arc<Self>, name: &str) -> Result<()> {
        let config = match self.conn.get(name) {
            Some(conn) => conn.config.clone(),
            None => {
                return Err(anyhow::anyhow!(
                    "Connection '{}' not found for reconnect",
                    name
                ));
            }
        };

        self.close_conn(name).await;
        tokio::time::sleep(Duration::from_secs(config.reconnect_timeout)).await;

        self.new_conn(name, config).await;
        Ok(())
    }

    pub async fn pong_loop(self: &Arc<Self>) {
        let mut global_recv = self.global_send.subscribe();

        loop {
            if let Ok(Message::PingMessage(name, val)) = global_recv.recv().await {
                if let Some(conn_writer) = self.write_sends.get(&name) {
                    let _ = conn_writer.send(Message::PongMessage(name, val));
                }
            }
        }
    }

    pub async fn error_handling_loop(self: &Arc<Self>) {
        let mut global_recv = self.global_send.subscribe();

        loop {
            if let Ok(msg) = global_recv.recv().await {
                match msg {
                    Message::ReadError(conn_name, _err_str) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    Message::WriteError(conn_name, _err_str) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    Message::PongReceiveTimeoutError(conn_name) => {
                        let _ = self.reconnect(&conn_name).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        for token in self.cancel_tokens.iter() {
            token.cancel();
        }

        for entry in self.write_sends.iter() {
            let name = entry.key().clone();
            let sender = entry.value();
            let _ = sender.send(Message::CloseMessage(name, None));
        }

        if let Some(pong_handle) = self.pong_handle.get() {
            pong_handle.abort();
        }
        if let Some(error_handle) = self.error_handle.get() {
            error_handle.abort();
        }
    }
}
