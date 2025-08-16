use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Error, anyhow};
use dashmap::DashMap;
use log::{error, info};
use tokio::{
    sync::{RwLock, broadcast},
    task::JoinHandle,
    time::sleep,
};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    config::{BroadcastMessage, BroadcastMessageType, Config},
    connection::{Connection, ConnectionError},
    hooks::ReadHooks,
    types::{ConnectionResult, HookType},
};

type ConnectionType = Connection<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct WSManager {
    pub conn: Arc<DashMap<String, Arc<RwLock<ConnectionType>>>>,
    restart_tx: broadcast::Sender<BroadcastMessage>,
}

impl Default for WSManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WSManager {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            restart_tx: self.restart_tx.clone(),
        }
    }
}

impl WSManager {
    pub fn new() -> Self {
        let (restart_tx, _) = broadcast::channel(16);
        Self {
            conn: Arc::new(DashMap::new()),
            restart_tx,
        }
    }

    pub async fn add_hook(&mut self, name: &str, hook: HookType) {
        if let Some(conn) = self.conn.get(name) {
            let mut conn_lock = conn.write().await;
            conn_lock.add_hook(hook);
        }
    }

    pub async fn add_text_hook<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        let hook = HookType::Text(Box::new(move |utf8_bytes| {
            handler(utf8_bytes.to_string());
        }));
        self.add_hook(name, hook).await;
    }

    pub async fn add_binary_hook<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        let hook = HookType::Binary(Box::new(move |bytes| {
            handler(bytes.to_vec());
        }));
        self.add_hook(name, hook).await;
    }

    pub async fn add_ping_hook<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        let hook = HookType::Ping(Box::new(move |bytes| {
            handler(bytes.to_vec());
        }));
        self.add_hook(name, hook).await;
    }

    pub async fn add_pong_hook<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(Vec<u8>) + Send + Sync + 'static,
    {
        let hook = HookType::Pong(Box::new(move |bytes| {
            handler(bytes.to_vec());
        }));
        self.add_hook(name, hook).await;
    }

    pub async fn add_close_hook<F>(&mut self, name: &str, handler: F)
    where
        F: Fn(Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>) + Send + Sync + 'static,
    {
        let hook = HookType::Close(Box::new(handler));
        self.add_hook(name, hook).await;
    }

    pub async fn new_conn(&mut self, name: &str, config: Config) {
        let conn = Connection::new(config, ReadHooks::new());
        self.conn
            .insert(name.to_string(), Arc::new(RwLock::new(conn)));
    }

    async fn wait_for_reconnect(conn: Arc<RwLock<ConnectionType>>, name: String) {
        let locked_conn = conn.read().await;
        let reconnect_timeout = locked_conn.config.reconnect_timeout;
        info!(
            "{}: Waiting for {} seconds before reconnecting",
            name, reconnect_timeout
        );
        drop(locked_conn);
        sleep(Duration::from_secs(reconnect_timeout)).await;
    }

    async fn attempt_connection(
        conn: Arc<RwLock<ConnectionType>>,
        name: String,
    ) -> Result<(JoinHandle<ConnectionResult>, JoinHandle<ConnectionResult>), Error> {
        let mut locked_conn = conn.write().await;
        info!(
            "{}: Attempting connection to {}",
            name, locked_conn.config.url
        );
        locked_conn.start_loop().await
    }

    async fn handle_connection_success(
        read_handle: JoinHandle<ConnectionResult>,
        ping_handle: JoinHandle<ConnectionResult>,
        tx: broadcast::Sender<BroadcastMessage>,
        name: String,
    ) {
        info!("{}: Connected", name);

        let rx_clone = tx.subscribe();
        let restart_handler = Self::create_restart_handler(rx_clone, name.clone());
        let read_abort = read_handle.abort_handle();
        let ping_abort = ping_handle.abort_handle();

        tokio::select! {
            _ = restart_handler => {
                read_abort.abort();
                ping_abort.abort();
            },
            maybe_read_result = read_handle => {
                if let Ok(Err(e)) = maybe_read_result {
                    error!("Read loop error: {:?}", e);
                }
                ping_abort.abort();
            },
            maybe_ping_result = ping_handle => {
                if let Ok(Err(e)) = maybe_ping_result {
                    error!("Ping loop error: {:?}", e);
                }
                read_abort.abort();
            }
        }
    }

    async fn run_connection_manager(
        conn: Arc<RwLock<ConnectionType>>,
        name: String,
        tx: broadcast::Sender<BroadcastMessage>,
    ) {
        loop {
            match Self::attempt_connection(conn.clone(), name.clone()).await {
                Ok((read_handle, ping_handle)) => {
                    Self::handle_connection_success(
                        read_handle,
                        ping_handle,
                        tx.clone(),
                        name.clone(),
                    )
                    .await;
                }
                Err(e) => {
                    error!("{}: Connection attempt failed with: {:?}", name, e);
                }
            }
            Self::wait_for_reconnect(conn.clone(), name.clone()).await;
        }
    }

    async fn create_restart_handler(mut rx: broadcast::Receiver<BroadcastMessage>, name: String) {
        while let Ok(msg) = rx.recv().await {
            if msg.target == name {
                if let Ok(msg_type) = msg.action.try_into() {
                    match msg_type {
                        BroadcastMessageType::Restart => {
                            error!("{}: Received abort message", name);
                            return;
                        }
                    }
                }
            }
        }
    }

    pub fn start(&self) -> HashMap<String, JoinHandle<()>> {
        let mut res = HashMap::with_capacity(self.conn.len());

        for entry in &*self.conn {
            let name = entry.key().clone();
            let conn = Arc::clone(entry.value());
            let tx = self.restart_tx.clone();

            let name_for_task = name.clone();
            let manager_handle = tokio::spawn(async move {
                Self::run_connection_manager(conn, name_for_task, tx).await;
            });

            res.insert(name, manager_handle);
        }
        res
    }

    pub fn restart(&self, name: &str) -> Result<(), Error> {
        let msg = BroadcastMessage {
            target: name.to_string(),
            action: 0, // BroadcastMessageType::Restart
        };
        self.restart_tx.send(msg).map_err(|e| anyhow!("Failed to send restart signal: {}", e))?;
        Ok(())
    }

    pub async fn write(&self, name: &str, msg: Message) -> ConnectionResult {
        if let Some(conn) = self.conn.get(name) {
            let msg_debug = format!("{:?}", msg);
            let mut locked_conn = conn.write().await;
            return locked_conn.write(msg).await.map_err(|e| {
                anyhow!("Failed to write message to connection '{}': {} (message: {})", name, e, msg_debug)
            });
        }
        Err(anyhow!(ConnectionError::ConnectionNotFound(
            format!("Connection '{}' not found when attempting to write message: {:?}", name, msg)
        )))
    }
}
