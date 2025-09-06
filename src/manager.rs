use std::{
    sync::{
        atomic::{AtomicBool, Ordering}, Arc, OnceLock
    }, time::Duration
};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use tokio::{task::JoinHandle, time::sleep};

use crate::{config::Config, connection::{Connection, ConnectionState}};

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
    FailedHandshake(String, i32),
    ConnectionStarting(String),
}

#[derive(Debug)]
pub enum ManagerError {
    ConnectionUnknown(String),
    ConnectionInactive(String),
    ConnectionRestarting(String),
}

impl std::fmt::Display for ManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManagerError::ConnectionUnknown(conn) => {
                write!(f, "Connection Unknown: {:?}", conn)
            }
            ManagerError::ConnectionInactive(conn) => {
                write!(f, "Connection Inactive: {:?}", conn)
            }
            ManagerError::ConnectionRestarting(conn) => {
                write!(f, "Connection Restarting: {:?}", conn)
            }
        }
    }
}

pub struct Manager {
    conn: DashMap<String, Connection>,
    global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
    loops_started: AtomicBool,
    pong_handle: OnceLock<JoinHandle<()>>,
    error_handle: OnceLock<JoinHandle<()>>,
}

impl Manager {
    pub fn new() -> Arc<Self> {
        let (global_send, _global_recv) = tokio::sync::broadcast::channel::<Message>(4_096);

        Arc::new(Self {
            conn: DashMap::new(),
            global_send: Arc::new(global_send),
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

    pub fn write(self: &Arc<Self>, name: &str, message: Message) -> Result<()> {
        if let Some(conn) = self.conn.get(name) {
            if let Ok(writer) = conn.write() {
                let _ = writer.send(message);
                return Ok(());
            }
            return Err(anyhow!(ManagerError::ConnectionInactive(name.to_string())));
        }
        Err(anyhow!(ManagerError::ConnectionUnknown(name.to_string())))
    }

    pub fn close_conn(self: &Arc<Self>, name: &str) -> Result<()> {
        if let Err(e) = self.write(name, Message::CloseMessage(name.to_string(), None)) {
            return Err(e);
        }
        self.conn.get_mut(name).unwrap().shutdown();
        Ok(())
    }

    pub fn new_conn(self: &Arc<Self>, name: &str, config: Config) {
        self.ensure_loops_started();

        if !self.conn.contains_key(name) {
            let conn = Connection::new(name, config.clone());
            self.conn.insert(name.to_string(), conn);
        }

        let name_owned = name.to_string();
        let global_send = Arc::clone(&self.global_send);
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let initial_back_off = 1.0;
            let mut attempt = 0;

            let _ = global_send.send(Message::ConnectionStarting(name_owned.clone()));
            loop {
                let back_off = initial_back_off * 2.0_f64.powi(attempt).min(60.0);
                if let Some(mut conn_ref) = manager.conn.get_mut(&name_owned) {
                    match conn_ref.try_connection(Arc::clone(&global_send)).await {
                        Ok(_) => {
                            let _ = global_send.send(Message::SuccessfulHandshake(name_owned.clone()));
                            break;
                        },
                        Err(_e) => {
                            let _ = global_send.send(Message::FailedHandshake(name_owned.clone(), attempt));
                            attempt += 1;
                            tokio::time::sleep(Duration::from_secs(back_off as u64)).await;
                        }
                    }
                } else {
                    // Connection was removed, exit the loop
                    break;
                }
            }
        });
    }

    pub async fn reconnect(self: &Arc<Self>, name: &str) -> Result<()> {
        if !self.conn.contains_key(name) {
            return Err(anyhow!(ManagerError::ConnectionUnknown(name.to_string())));
        }

        let conn = self.conn.get(name).expect("Connection should exist");
        let config = conn.config.clone(); 

        if let ConnectionState::Connecting = conn.connection_state {
            return Err(anyhow!(ManagerError::ConnectionRestarting(name.to_string())));
        }
        drop(conn);

        self.close_conn(name)?;
        sleep(Duration::from_secs(config.reconnect_timeout)).await;

        self.new_conn(name, config);
        Ok(())
    }

    pub async fn pong_loop(self: &Arc<Self>) {
        let mut global_recv = self.global_send.subscribe();

        loop {
            if let Ok(Message::PingMessage(name, val)) = global_recv.recv().await {
                if let Some(conn) = self.conn.get(&name) {
                    if let Ok(writer) = conn.write() {
                        let _ = writer.send(Message::PongMessage(name, val));
                    }
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
                        let _ = self.reconnect(&conn_name);
                    }
                    Message::WriteError(conn_name, _err_str) => {
                        let _ = self.reconnect(&conn_name);
                    }
                    Message::PongReceiveTimeoutError(conn_name) => {
                        let _ = self.reconnect(&conn_name);
                    }
                    _ => {}
                }
            }
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        for mut conn in self.conn.iter_mut() {
            conn.shutdown();
        }

        if let Some(pong_handle) = self.pong_handle.get() {
            pong_handle.abort();
        }
        if let Some(error_handle) = self.error_handle.get() {
            error_handle.abort();
        }
    }
}
