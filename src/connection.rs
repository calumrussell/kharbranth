use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::{Error, anyhow};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use log::{debug, error};
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
    time::sleep,
};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use crate::{
    config::Config,
    hooks::ReadHooks,
    ping::PingTracker,
    types::{ConnectionResult, HookType},
};

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
                write!(f, "Connection initialization failed: {:?}", msg)
            }
        }
    }
}

impl std::error::Error for ConnectionError {}

type ReadStream<S> = Arc<Mutex<SplitStream<WebSocketStream<S>>>>;
type WriteStream<S> = Arc<Mutex<SplitSink<WebSocketStream<S>, Message>>>;

pub struct Connection<S> {
    pub config: Config,
    pub read: Option<ReadStream<S>>,
    pub write: Option<WriteStream<S>>,
    pub hooks: ReadHooks,
    ping_tracker: Arc<RwLock<PingTracker>>,
}

impl<S> Connection<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    pub async fn write(&mut self, msg: Message) -> ConnectionResult {
        let write = self
            .write
            .as_ref()
            .ok_or_else(|| anyhow!("Connection write stream not initialized"))?;
        let write_clone = Arc::clone(write);
        let mut write_lock = write_clone.lock().await;
        if let Err(e) = write_lock.send(msg).await {
            return Err(anyhow!(ConnectionError::WriteError(e.to_string())));
        }
        Ok(())
    }

    pub async fn ping_loop(&mut self) -> Result<JoinHandle<ConnectionResult>, Error> {
        let write = self
            .write
            .as_ref()
            .ok_or_else(|| anyhow!("Connection write stream not initialized for ping loop"))?;
        let write_clone = Arc::clone(write);
        let ping_tracker_clone = Arc::clone(&self.ping_tracker);
        let ping_duration_clone = self.config.ping_duration;

        Ok(tokio::spawn(async move {
            loop {
                {
                    let mut tracker = ping_tracker_clone.write().await;
                    tracker.check_timeout()?;

                    if tracker.should_send_ping() {
                        let timestamp = SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map_err(|e| anyhow!("System time error: {}", e))?
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
        }))
    }

    pub async fn read_loop(&mut self) -> Result<JoinHandle<ConnectionResult>, Error> {
        let read = self
            .read
            .as_ref()
            .ok_or_else(|| anyhow!("Connection read stream not initialized for read loop"))?;
        let read_clone = Arc::clone(read);
        let write = self
            .write
            .as_ref()
            .ok_or_else(|| anyhow!("Connection write stream not initialized for read loop"))?;
        let write_clone = Arc::clone(write);
        let ping_tracker_clone = Arc::clone(&self.ping_tracker);

        let on_text_clone = Arc::clone(&self.hooks.on_text);
        let on_binary_clone = Arc::clone(&self.hooks.on_binary);
        let on_ping_clone = Arc::clone(&self.hooks.on_ping);
        let on_pong_clone = Arc::clone(&self.hooks.on_pong);
        let on_close_clone = Arc::clone(&self.hooks.on_close);
        let on_frame_clone = Arc::clone(&self.hooks.on_frame);

        Ok(tokio::spawn(async move {
            loop {
                let mut read_lock = read_clone.lock().await;
                match read_lock.next().await {
                    Some(received) => {
                        debug!("Read: {:?}", &received);
                        match received {
                            Ok(msg) => match msg {
                                Message::Text(text) => {
                                    if let Some(on_text_func) = on_text_clone.as_ref() {
                                        on_text_func(text.clone());
                                    }
                                }
                                Message::Binary(binary) => {
                                    if let Some(on_binary_func) = on_binary_clone.as_ref() {
                                        on_binary_func(binary);
                                    }
                                }
                                Message::Ping(ping) => {
                                    if let Some(on_ping_func) = on_ping_clone.as_ref() {
                                        on_ping_func(ping.clone());
                                    }
                                    let mut write_lock = write_clone.lock().await;
                                    if let Err(e) = write_lock.send(Message::Pong(ping)).await {
                                        error!("Failed to send pong: {}", e);
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
        }))
    }

    pub fn add_hook(&mut self, hook: HookType) {
        self.hooks.add_hook(hook);
    }

    pub fn new(config: Config, hooks: ReadHooks) -> Self {
        let ping_timeout = Duration::from_secs(config.ping_timeout);
        Self {
            config,
            read: None,
            write: None,
            hooks,
            ping_tracker: Arc::new(RwLock::new(PingTracker::new(ping_timeout))),
        }
    }
}

impl Connection<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    pub async fn start_loop(
        &mut self,
    ) -> Result<(JoinHandle<ConnectionResult>, JoinHandle<ConnectionResult>), Error> {
        let request = self
            .config
            .url
            .clone()
            .into_client_request()
            .map_err(|e| anyhow!("Invalid WebSocket URL '{}': {}", self.config.url, e))?;
        match connect_async(request).await {
            Ok((conn, response)) => {
                debug!("Handshake response: {:?}", response);
                let (write, read) = conn.split();

                self.read = Some(Arc::new(Mutex::new(read)));
                self.write = Some(Arc::new(Mutex::new(write)));

                {
                    let mut tracker = self.ping_tracker.write().await;
                    *tracker = PingTracker::new(Duration::from_secs(self.config.ping_timeout));
                }

                let read_loop = self.read_loop().await?;
                let ping_loop = self.ping_loop().await?;

                sleep(Duration::from_millis(self.config.connection_init_delay())).await;
                for message_str in self.config.write_on_init.clone() {
                    let message = Message::Text(message_str.into());
                    if let Err(e) = self.write(message).await {
                        error!("Failed to send initialization message: {}", e);
                    }
                }

                Ok((read_loop, ping_loop))
            }
            Err(e) => Err(anyhow!(ConnectionError::ConnectionInitFailed(
                e.to_string()
            ))),
        }
    }
}
