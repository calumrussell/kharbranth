use std::{collections::HashMap, sync::Arc, time::{Duration, SystemTime}};

use anyhow::anyhow;
use futures_util::{lock, stream::{SplitSink, SplitStream}, SinkExt, StreamExt};
use log::{debug, info, error};
use tokio::{net::TcpStream, sync::{broadcast, Mutex, RwLock}, task::JoinHandle, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::{client::IntoClientRequest, protocol::{frame::Frame, CloseFrame}, Message, Utf8Bytes}, MaybeTlsStream, WebSocketStream};
use tokio_util::bytes::Bytes;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

pub struct ReadHooks {
    pub on_text: Arc<Option<Box<dyn Fn(Utf8Bytes) + Send + Sync>>>,
    pub on_binary: Arc<Option<Box<dyn Fn(Bytes) + Send + Sync>>>,
    pub on_ping: Arc<Option<Box<dyn Fn(Bytes) + Send + Sync>>>,
    pub on_pong: Arc<Option<Box<dyn Fn(Bytes) + Send + Sync>>>,
    pub on_close: Arc<Option<Box<dyn Fn(Option<CloseFrame>) + Send + Sync>>>,
    pub on_frame: Arc<Option<Box<dyn Fn(Frame) + Send + Sync>>>,
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

#[derive(Debug)]
pub enum ConnectionError {
    PingFailed,
    CloseFrameReceived,
    PongReceiveTimeout,
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::PingFailed => write!(f, "Ping Failed"),
            ConnectionError::CloseFrameReceived => write!(f, "Close Frame Received"),
            ConnectionError::PongReceiveTimeout => write!(f, "Timed out waiting for Pong"),
        }
    }
}

impl std::error::Error for ConnectionError {}

type ConnectionResult = Result<(), anyhow::Error>;

pub struct Connection {
    pub config: Config,
    pub read: Option<Arc<Mutex<WsSource>>>,
    pub write: Option<Arc<Mutex<WsSink>>>,
    pub hooks: ReadHooks,
}

impl Connection {
    pub async fn write(&mut self, msg: Message) {
        let write_clone = Arc::clone(&self.write.as_ref().unwrap());
        let mut write_lock = write_clone.lock().await;
        let _ = write_lock.send(msg).await;
    }
    
    async fn ping_loop(&mut self) -> JoinHandle<ConnectionResult> {
        let write_clone = Arc::clone(&self.write.as_ref().unwrap());
        let ping_message_clone = self.config.ping_message.clone().into_bytes();
        let ping_duration_clone = self.config.ping_duration.clone();

        tokio::spawn(async move {
            loop {
                {
                    let mut write_lock = write_clone.lock().await;
                    if let Err(e) = write_lock.send(Message::Ping(ping_message_clone.clone().into())).await {
                        return Err(anyhow!(ConnectionError::PingFailed))
                    }
                }
                sleep(Duration::from_secs(ping_duration_clone.into())).await;
            }
        })
    }

    async fn read_loop(&mut self) -> JoinHandle<ConnectionResult> {
        let read_clone = Arc::clone(&self.read.as_ref().unwrap());
        let ping_duration_clone = self.config.ping_duration.clone();
        let mut last_pong = SystemTime::now();

        let on_text_clone = Arc::clone(&self.hooks.on_text);
        let on_binary_clone = Arc::clone(&self.hooks.on_binary);
        let on_ping_clone = Arc::clone(&self.hooks.on_ping);
        let on_pong_clone = Arc::clone(&self.hooks.on_pong);
        let on_close_clone = Arc::clone(&self.hooks.on_close);
        let on_frame_clone = Arc::clone(&self.hooks.on_frame);
        tokio::spawn(async move {
            loop {
                if SystemTime::now() > last_pong.checked_add(Duration::from_secs(ping_duration_clone + 5)).unwrap() {
                    return Err(anyhow!(ConnectionError::PingFailed));
                }

                let mut read_lock = read_clone.lock().await;
                if let Some(received) = read_lock.next().await {
                    if let Ok(msg) = received {
                        match msg {
                            Message::Text(text) => {
                                if let Some(on_text_func) = on_text_clone.as_ref() {
                                    on_text_func(text);
                                }
                            },
                            Message::Binary(binary) => {
                                if let Some(on_binary_func) = on_binary_clone.as_ref() {
                                    on_binary_func(binary);
                                }
                            },
                            Message::Ping(ping) => {
                                if let Some(on_ping_func) = on_ping_clone.as_ref() {
                                    on_ping_func(ping);
                                }
                            },
                            Message::Pong(pong) => {
                                if let Some(on_pong_func) = on_pong_clone.as_ref() {
                                    on_pong_func(pong);
                                }
                                last_pong = SystemTime::now();
                            },
                            Message::Close(maybe_frame) => {
                                if let Some (on_close_func) = on_close_clone.as_ref() {
                                    on_close_func(maybe_frame);
                                }
                                return Err(anyhow!(ConnectionError::CloseFrameReceived));
                            },
                            Message::Frame(frame) => {
                                if let Some(on_frame_func) = on_frame_clone.as_ref() {
                                    on_frame_func(frame);
                                }
                            }
                        }

                    }
                }
            }
        })
    }

    pub async fn start_loop(&mut self) -> (JoinHandle<ConnectionResult>, JoinHandle<ConnectionResult>) {
        let request = self.config.url.clone().into_client_request().unwrap();
        let (conn, response) = connect_async(request).await.unwrap();
        debug!("Websocket connection handshake response: {:?}", response);
        let (write, read) = conn.split();
        
        self.read = Some(Arc::new(Mutex::new(read)));
        self.write = Some(Arc::new(Mutex::new(write)));
     
        let read_loop = self.read_loop().await;
        let ping_loop = self.ping_loop().await;
        
        (read_loop, ping_loop)
    }
    
    pub fn new(config: Config, hooks: ReadHooks) -> Self {
        Self {
            config,
            read: None,
            write: None,
            hooks,
        }
    }
}

pub struct Config {
    pub url: String,
    pub ping_duration: u64,
    pub ping_message: String,
    pub reconnect_timeout: u64,
}

pub type BroadcastMesasge = (String, u8);

pub struct WSManager {
    conn: HashMap<String, Arc<RwLock<Connection>>>,
}

impl WSManager {
    pub fn new() -> Self {
        Self { 
            conn: HashMap::new() 
        }
    }
    
    pub fn new_conn(&mut self, name: &str, config: Config, hooks: ReadHooks) {
        let conn = Connection {
            config,
            read: None,
            write: None,
            hooks,
        };

        self.conn.insert(name.to_string(), Arc::new(RwLock::new(conn)));
    }

    pub async fn start(&self, tx: broadcast::Sender<BroadcastMesasge>) -> HashMap<String, JoinHandle<()>> {
        let mut res = HashMap::with_capacity(self.conn.len());
        
        for (name, conn ) in &self.conn {
            let conn_clone = Arc::clone(conn);
            let name_clone = name.clone();
            let mut rx_clone = tx.subscribe();
            let manager_handle = tokio::spawn(async move {
                loop {
                    
                    let (read_handle, ping_handle) = {
                        let mut locked_conn = conn_clone.write().await;
                        info!("Connected to {}", locked_conn.config.url);
                        locked_conn.start_loop().await
                    };
                   
                    let abort_read = read_handle.abort_handle();
                    let abort_ping = ping_handle.abort_handle();

                    tokio::select! {
                        maybe_msg = rx_clone.recv() => {
                            if let Ok(msg) = maybe_msg {
                                if msg.0 == name_clone && msg.1 == 0 {
                                    error!("Received abort message");
                                    abort_ping.abort();
                                    abort_read.abort();
                                }
                            }
                        },
                        maybe_read_result = read_handle => {
                            if let Ok(read_result) = maybe_read_result {
                                if let Err(e) = read_result {
                                   error!("Error: {:?}", e);
                                }
                            }
                            abort_read.abort();
                            abort_ping.abort();
                        }
                        maybe_ping_result = ping_handle => {
                            if let Ok(ping_result) = maybe_ping_result {
                                if let Err(e) = ping_result {
                                   error!("Error: {:?}", e);
                                }
                            }
                            abort_read.abort();
                            abort_ping.abort();
                        }
                    }
                    
                    {
                        let locked_conn = conn_clone.read().await;
                        let reconnect_timeout = locked_conn.config.reconnect_timeout;
                        info!("Waiting for {} seconds before reconnecting", reconnect_timeout);
                        sleep(Duration::from_secs(reconnect_timeout)).await;
                    }
                }
            });
            res.insert(name.clone(), manager_handle);
        }
        res
    }
    
    pub async fn write(&mut self, name: &str, msg: Message) {
        if let Some(conn) = self.conn.get(name) {
            let mut locked_conn = conn.write().await;
            locked_conn.write(msg).await;
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let hl_config = Config {
        url: "wss://api.hyperliquid.xyz/ws".to_string(),
        ping_duration: 10,
        ping_message: "{\"method\": \"ping\"}".to_string(),
        reconnect_timeout: 5,
    };
    
    let mut hooks = ReadHooks::new();
    hooks.on_text = Arc::new(Some(Box::new(|text| {
        debug!("{:?}", text);
    })));
       
    let mut mgr = WSManager::new();
    mgr.new_conn("hl", hl_config, hooks);
    
    let (tx, rx ) = broadcast::channel(64);
    let _handles = mgr.start(tx.clone()).await;
    
    loop {
        sleep(Duration::from_secs(5)).await;
        mgr.write("hl", Message::Text("{ \"method\": \"subscribe\", \"subscription\": { \"type\": \"l2Book\", \"coin\": \"SOL\" } }".into())).await;
        sleep(Duration::from_secs(10)).await;
        let _ = tx.send(("hl".to_string(), 0));
    }
}
