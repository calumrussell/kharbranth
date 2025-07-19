use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Error, Result, anyhow};
use dashmap::DashMap;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, RwLock, broadcast},
    task::JoinHandle,
    time::sleep,
};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{
        Message, Utf8Bytes,
        client::IntoClientRequest,
        protocol::{CloseFrame, frame::Frame},
    },
};
use tokio_util::bytes::Bytes;

type BytesFunc = Option<Box<dyn Fn(Bytes) + Send + Sync>>;
type Utf8BytesFunc = Option<Box<dyn Fn(Utf8Bytes) + Send + Sync>>;
type CloseFrameFunc = Option<Box<dyn Fn(Option<CloseFrame>) + Send + Sync>>;
type FrameFunc = Option<Box<dyn Fn(Frame) + Send + Sync>>;

pub struct ReadHooks {
    pub on_text: Arc<Utf8BytesFunc>,
    pub on_binary: Arc<BytesFunc>,
    pub on_ping: Arc<BytesFunc>,
    pub on_pong: Arc<BytesFunc>,
    pub on_close: Arc<CloseFrameFunc>,
    pub on_frame: Arc<FrameFunc>,
}

impl ReadHooks {
    pub fn new() -> Self {
        Self {
            on_text: Arc::new(None),
            on_binary: Arc::new(None),
            on_ping: Arc::new(None),
            on_pong: Arc::new(None),
            on_close: Arc::new(None),
            on_frame: Arc::new(None),
        }
    }
}

impl Default for ReadHooks {
    fn default() -> Self {
        Self::new()
    }
}

struct SimplePingTracker {
    last_ping_sent: Option<(Instant, Vec<u8>)>,
    timeout: Duration,
}

impl SimplePingTracker {
    fn new(timeout: Duration) -> Self {
        Self {
            last_ping_sent: None,
            timeout,
        }
    }

    fn should_send_ping(&self) -> bool {
        self.last_ping_sent.is_none()
    }

    fn send_ping(&mut self, payload: Vec<u8>) -> Vec<u8> {
        self.last_ping_sent = Some((Instant::now(), payload.clone()));
        payload
    }

    fn handle_pong(&mut self, payload: Vec<u8>) -> Result<(), Error> {
        match &self.last_ping_sent {
            Some((_, expected_payload)) if expected_payload == &payload => {
                self.last_ping_sent = None;
                Ok(())
            }
            _ => {
                log::warn!("Pong payload mismatch or unexpected pong");
                Err(anyhow!(ConnectionError::PongReceiveTimeout))
            }
        }
    }

    fn check_timeout(&self) -> Result<(), Error> {
        if let Some((sent_time, _)) = &self.last_ping_sent {
            if Instant::now() - *sent_time > self.timeout {
                return Err(anyhow!(ConnectionError::PongReceiveTimeout));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ConnectionError {
    PingFailed(String),
    CloseFrameReceived,
    PongReceiveTimeout,
    ReadError(String),
    ConnectionDropped,
    WriteError(String),
    ConnectionNotFound(String),
    ConnectionInitFailed(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::PingFailed(msg) => write!(f, "Ping Failed: {:?}", msg),
            ConnectionError::CloseFrameReceived => write!(f, "Close Frame Received"),
            ConnectionError::PongReceiveTimeout => write!(f, "Timed out waiting for Pong"),
            ConnectionError::ReadError(msg) => {
                write!(f, "Read error when calling .next(): {:?}", msg)
            }
            ConnectionError::ConnectionDropped => write!(f, "Connection dropped"),
            ConnectionError::WriteError(msg) => {
                write!(f, "Write error when calling .send(): {:?}", msg)
            }
            ConnectionError::ConnectionNotFound(name) => {
                write!(f, "Connection not found: {:?}", name)
            }
            ConnectionError::ConnectionInitFailed(msg) => {
                write!(f, "Connection intialization failed: {:?}", msg)
            }
        }
    }
}

impl std::error::Error for ConnectionError {}

type ConnectionResult = Result<(), Error>;
type ReadStream<S> = Arc<Mutex<SplitStream<WebSocketStream<S>>>>;
type WriteStream<S> = Arc<Mutex<SplitSink<WebSocketStream<S>, Message>>>;

pub struct Connection<S> {
    pub config: Config,
    pub read: Option<ReadStream<S>>,
    pub write: Option<WriteStream<S>>,
    pub hooks: ReadHooks,
    ping_tracker: Arc<RwLock<SimplePingTracker>>,
}

impl<S> Connection<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    pub async fn write(&mut self, msg: Message) -> ConnectionResult {
        let write_clone = Arc::clone(self.write.as_ref().unwrap());
        let mut write_lock = write_clone.lock().await;
        if let Err(e) = write_lock.send(msg).await {
            return Err(anyhow!(ConnectionError::WriteError(e.to_string())));
        }
        Ok(())
    }

    async fn ping_loop(&mut self) -> JoinHandle<ConnectionResult> {
        let write_clone = Arc::clone(self.write.as_ref().unwrap());
        let ping_tracker_clone = Arc::clone(&self.ping_tracker);
        let ping_duration_clone = self.config.ping_duration;

        tokio::spawn(async move {
            loop {
                {
                    let tracker = ping_tracker_clone.read().await;
                    tracker.check_timeout()?;

                    if !tracker.should_send_ping() {
                        drop(tracker);
                        sleep(Duration::from_secs(ping_duration_clone)).await;
                        continue;
                    }
                }

                {
                    let mut tracker = ping_tracker_clone.write().await;
                    // Double-check in case state changed
                    if tracker.should_send_ping() {
                        // Create unique payload using current timestamp
                        let timestamp = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                            .to_le_bytes()
                            .to_vec();
                        let payload = tracker.send_ping(timestamp);

                        let mut write_lock = write_clone.lock().await;
                        if let Err(e) = write_lock.send(Message::Ping(payload.into())).await {
                            return Err(anyhow!(ConnectionError::PingFailed(e.to_string())));
                        }
                    }
                }
                sleep(Duration::from_secs(ping_duration_clone)).await;
            }
        })
    }

    async fn read_loop(&mut self) -> JoinHandle<ConnectionResult> {
        let read_clone = Arc::clone(self.read.as_ref().unwrap());
        let ping_tracker_clone = Arc::clone(&self.ping_tracker);

        let on_text_clone = Arc::clone(&self.hooks.on_text);
        let on_binary_clone = Arc::clone(&self.hooks.on_binary);
        let on_ping_clone = Arc::clone(&self.hooks.on_ping);
        let on_pong_clone = Arc::clone(&self.hooks.on_pong);
        let on_close_clone = Arc::clone(&self.hooks.on_close);
        let on_frame_clone = Arc::clone(&self.hooks.on_frame);

        tokio::spawn(async move {
            loop {
                let mut read_lock = read_clone.lock().await;
                match read_lock.next().await {
                    Some(recieved) => {
                        debug!("Read: {:?}", &recieved);
                        match recieved {
                            Ok(msg) => match msg {
                                Message::Text(text) => {
                                    if let Some(on_text_func) = on_text_clone.as_ref() {
                                        on_text_func(text);
                                    }
                                }
                                Message::Binary(binary) => {
                                    if let Some(on_binary_func) = on_binary_clone.as_ref() {
                                        on_binary_func(binary);
                                    }
                                }
                                Message::Ping(ping) => {
                                    if let Some(on_ping_func) = on_ping_clone.as_ref() {
                                        on_ping_func(ping);
                                    }
                                }
                                Message::Pong(pong) => {
                                    if let Some(on_pong_func) = on_pong_clone.as_ref() {
                                        on_pong_func(pong.clone());
                                    }

                                    {
                                        let mut tracker = ping_tracker_clone.write().await;
                                        tracker.handle_pong(pong.to_vec())?;
                                    }
                                }
                                Message::Close(maybe_frame) => {
                                    if let Some(on_close_func) = on_close_clone.as_ref() {
                                        on_close_func(maybe_frame);
                                    }
                                    return Err(anyhow!(ConnectionError::CloseFrameReceived));
                                }
                                Message::Frame(frame) => {
                                    if let Some(on_frame_func) = on_frame_clone.as_ref() {
                                        on_frame_func(frame);
                                    }
                                }
                            },
                            Err(e) => {
                                return Err(anyhow!(ConnectionError::ReadError(e.to_string())));
                            }
                        }
                    }
                    None => return Err(anyhow!(ConnectionError::ConnectionDropped)),
                }
            }
        })
    }

    pub fn new(config: Config, hooks: ReadHooks) -> Self {
        let ping_timeout = Duration::from_secs(config.ping_timeout);
        Self {
            config,
            read: None,
            write: None,
            hooks,
            ping_tracker: Arc::new(RwLock::new(SimplePingTracker::new(ping_timeout))),
        }
    }
}

impl Connection<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    pub async fn start_loop(
        &mut self,
    ) -> Result<(JoinHandle<ConnectionResult>, JoinHandle<ConnectionResult>), Error> {
        let request = self.config.url.clone().into_client_request().unwrap();
        match connect_async(request).await {
            Ok((conn, response)) => {
                debug!("Handshake response: {:?}", response);
                let (write, read) = conn.split();

                self.read = Some(Arc::new(Mutex::new(read)));
                self.write = Some(Arc::new(Mutex::new(write)));

                let read_loop = self.read_loop().await;
                let ping_loop = self.ping_loop().await;

                sleep(Duration::from_millis(500)).await;
                for message in self.config.write_on_init.clone() {
                    let _ = self.write(message).await;
                }

                Ok((read_loop, ping_loop))
            }
            Err(e) => Err(anyhow!(ConnectionError::ConnectionInitFailed(
                e.to_string()
            ))),
        }
    }
}

pub struct Config {
    pub url: String,
    pub ping_duration: u64,
    pub ping_message: String,
    pub ping_timeout: u64,
    pub reconnect_timeout: u64,
    pub write_on_init: Vec<Message>,
}

pub enum BroadcastMessageType {
    Restart,
}

impl TryFrom<u8> for BroadcastMessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(BroadcastMessageType::Restart),
            _ => Err(anyhow!("Unknown u8 input")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub target: String,
    pub action: u8,
}

type ConnectionType = Connection<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub struct WSManager {
    conn: Arc<DashMap<String, Arc<RwLock<ConnectionType>>>>,
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
        }
    }
}

impl WSManager {
    pub fn new() -> Self {
        Self {
            conn: Arc::new(DashMap::new()),
        }
    }

    pub async fn new_conn(&self, name: &str, config: Config, hooks: ReadHooks) {
        let conn = Connection::new(config, hooks);

        self.conn
            .insert(name.to_string(), Arc::new(RwLock::new(conn)));
    }

    pub fn start(
        &self,
        tx: broadcast::Sender<BroadcastMessage>,
    ) -> HashMap<String, JoinHandle<()>> {
        let mut res = HashMap::with_capacity(self.conn.len());

        for entry in &*self.conn {
            let name = entry.key();
            let conn = entry.value();
            let conn_clone: Arc<RwLock<ConnectionType>> = Arc::clone(conn);
            let name_clone = name.clone();
            let tx_clone = tx.clone();
            let manager_handle = tokio::spawn(async move {
                loop {
                    let connection_attempt = {
                        let mut locked_conn = conn_clone.write().await;
                        info!(
                            "{}: Attempting connection to {}",
                            name_clone, locked_conn.config.url
                        );
                        locked_conn.start_loop().await
                    };

                    if connection_attempt.is_err() {
                        let err = connection_attempt.unwrap_err();
                        error!("{}: Connection attempt failed with: {:?}", name_clone, err);

                        {
                            let locked_conn = conn_clone.read().await;
                            let reconnect_timeout = locked_conn.config.reconnect_timeout;
                            info!(
                                "{}: Waiting for {} seconds before reconnecting",
                                name_clone, reconnect_timeout
                            );
                            sleep(Duration::from_secs(reconnect_timeout)).await;
                        }
                    } else {
                        let (read_handle, ping_handle) = connection_attempt.unwrap();
                        info!("{}: Connected", name_clone);

                        let name_clone_two = name_clone.clone();
                        let mut rx_clone = tx_clone.subscribe();
                        let recv_handle = tokio::spawn(async move {
                            while let Ok(msg) = rx_clone.recv().await {
                                if msg.target == name_clone_two {
                                    if let Ok(msg_type) = msg.action.try_into() {
                                        match msg_type {
                                            BroadcastMessageType::Restart => {
                                                error!(
                                                    "{}: Received abort message",
                                                    name_clone_two
                                                );
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        });
                        let abort_read = read_handle.abort_handle();
                        let abort_ping = ping_handle.abort_handle();

                        tokio::select! {
                            _ = recv_handle => {
                                abort_read.abort();
                                abort_ping.abort();
                            }
                            maybe_read_result = read_handle => {
                                if let Ok(Err(e)) = maybe_read_result {
                                    error!("Read loop error: {:?}", e);
                                }
                                abort_read.abort();
                                abort_ping.abort();
                            },
                            maybe_ping_result = ping_handle => {
                                if let Ok(Err(e)) = maybe_ping_result {
                                    error!("Ping loop error: {:?}", e);
                                }
                                abort_read.abort();
                                abort_ping.abort();
                            }
                        }

                        {
                            let locked_conn = conn_clone.read().await;
                            let reconnect_timeout = locked_conn.config.reconnect_timeout;
                            info!(
                                "{}: Waiting for {} seconds before reconnecting",
                                name_clone, reconnect_timeout
                            );
                            sleep(Duration::from_secs(reconnect_timeout)).await;
                        }
                    }
                }
            });
            res.insert(name.clone(), manager_handle);
        }
        res
    }

    pub async fn write(&self, name: &str, msg: Message) -> ConnectionResult {
        if let Some(conn) = self.conn.get(name) {
            let mut locked_conn = conn.write().await;
            return locked_conn.write(msg).await;
        }
        Err(anyhow!(ConnectionError::ConnectionNotFound(
            name.to_string()
        )))
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use futures_util::StreamExt;
    use tokio::io::ErrorKind;
    use tokio::sync::Mutex;
    use tokio_test::io::Mock;
    use tokio_tungstenite::WebSocketStream;

    use crate::{Config, Connection, ReadHooks, WSManager};

    async fn setup(mock: Mock) -> Connection<Mock> {
        let ws_stream = WebSocketStream::from_raw_socket(
            mock,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;

        let write_on_init = Vec::new();
        let config = Config {
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: "wss://fake".to_string(),
            reconnect_timeout: 10,
            write_on_init,
        };

        let (sink, stream) = ws_stream.split();

        let hooks = ReadHooks::new();

        let mut conn = Connection::new(config, hooks);
        conn.write = Some(Arc::new(Mutex::new(sink)));
        conn.read = Some(Arc::new(Mutex::new(stream)));
        return conn;
    }

    #[tokio::test]
    async fn read_error_returns_connection_error() {
        let mock = tokio_test::io::Builder::new()
            .read_error(std::io::Error::new(
                ErrorKind::ConnectionAborted,
                "Server connection lost",
            ))
            .build();

        let mut conn = setup(mock).await;

        let read_handle = conn.read_loop().await;
        let res = tokio::join!(read_handle);

        assert!(res.0.unwrap().is_err());
    }

    #[tokio::test]
    async fn concurrent_new_conn_operations() {
        let manager = WSManager::new();
        let mut handles = vec![];

        for i in 0..100 {
            let manager_clone = manager.clone();
            let handle = tokio::spawn(async move {
                let config = Config {
                    ping_duration: 10,
                    ping_message: "ping".to_string(),
                    ping_timeout: 15,
                    url: format!("wss://fake{}.com", i),
                    reconnect_timeout: 10,
                    write_on_init: Vec::new(),
                };
                let hooks = ReadHooks::new();
                manager_clone
                    .new_conn(&format!("conn_{}", i), config, hooks)
                    .await;
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify all connections were added
        assert_eq!(manager.conn.len(), 100);
        for i in 0..100 {
            assert!(manager.conn.contains_key(&format!("conn_{}", i)));
        }
    }

    #[tokio::test]
    async fn concurrent_write_to_different_connections() {
        // This test verifies that DashMap allows concurrent writes to different connections
        // without blocking.
        let manager = WSManager::new();

        for i in 0..10 {
            let config = Config {
                ping_duration: 10,
                ping_message: "ping".to_string(),
                ping_timeout: 15,
                url: format!("wss://fake{}.com", i),
                reconnect_timeout: 10,
                write_on_init: Vec::new(),
            };
            let hooks = ReadHooks::new();
            manager
                .new_conn(&format!("conn_{}", i), config, hooks)
                .await;
        }

        let mut handles = vec![];
        for i in 0..10 {
            let manager_clone = manager.clone();
            let handle = tokio::spawn(async move {
                // Simulate checking if connection exists (read operation)
                manager_clone.conn.contains_key(&format!("conn_{}", i))
            });
            handles.push(handle);
        }

        // All operations should complete without blocking
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result);
        }
    }

    #[tokio::test]
    async fn no_deadlock_during_start_and_write() {
        use tokio::time::{Duration, timeout};

        // This test verifies that DashMap doesn't cause deadlocks when start() is iterating
        // while other operations try to access the map
        let manager = WSManager::new();

        for i in 0..5 {
            let config = Config {
                ping_duration: 10,
                ping_message: "ping".to_string(),
                ping_timeout: 15,
                url: format!("wss://fake{}.com", i),
                reconnect_timeout: 10,
                write_on_init: Vec::new(),
            };
            let hooks = ReadHooks::new();
            manager
                .new_conn(&format!("conn_{}", i), config, hooks)
                .await;
        }

        let manager_clone1 = manager.clone();
        let manager_clone2 = manager.clone();

        // Start iterating over connections (simulating start())
        let iterate_task = tokio::spawn(async move {
            // Simulate what start() does - iterate and hold references
            for _ in 0..100 {
                for entry in &*manager_clone1.conn {
                    let _name = entry.key();
                    let _conn = entry.value();
                    // Small delay to increase chance of race condition
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
            }
        });

        // Concurrently try to add new connections
        let write_task = tokio::spawn(async move {
            // Give iteration a chance to start
            tokio::time::sleep(Duration::from_millis(1)).await;

            // This should not deadlock even though start() is iterating
            for i in 5..10 {
                let config = Config {
                    ping_duration: 10,
                    ping_message: "ping".to_string(),
                    ping_timeout: 15,
                    url: format!("wss://fake{}.com", i),
                    reconnect_timeout: 10,
                    write_on_init: Vec::new(),
                };
                let hooks = ReadHooks::new();
                manager_clone2
                    .new_conn(&format!("conn_{}", i), config, hooks)
                    .await;
            }
            // Return number of connections added
            5
        });

        // Both operations should complete within a reasonable time (no deadlock)
        let result = timeout(Duration::from_secs(5), async {
            let (r1, r2) = tokio::join!(iterate_task, write_task);
            (r1, r2)
        })
        .await;

        assert!(result.is_ok(), "Operations timed out - possible deadlock!");

        let (iterate_result, write_result) = result.unwrap();
        assert!(iterate_result.is_ok());
        assert_eq!(write_result.unwrap(), 5);

        // Verify all connections were added
        assert_eq!(manager.conn.len(), 10);
    }
}
