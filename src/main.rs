use std::{collections::HashMap, sync::Arc, time::{Duration, SystemTime}};

use futures_util::{stream::{SplitSink, SplitStream}, SinkExt, StreamExt};
use tokio::{net::TcpStream, sync::{broadcast, Mutex}, task::JoinHandle, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::{client::IntoClientRequest, protocol::{frame::Frame, CloseFrame}, Message, Utf8Bytes}, MaybeTlsStream, WebSocketStream};
use tokio_util::{bytes::Bytes, sync::CancellationToken};

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
    
    async fn ping_loop(&mut self) -> JoinHandle<()> {
        let write_clone = Arc::clone(&self.write.as_ref().unwrap());
        let ping_message_clone = self.config.ping_message.clone().into_bytes();
        let ping_duration_clone = self.config.ping_duration.clone();

        tokio::spawn(async move {
            loop {
                {
                    let mut write_lock = write_clone.lock().await;
                    let _ = write_lock.send(Message::Ping(ping_message_clone.clone().into())).await;
                }
                sleep(Duration::from_secs(ping_duration_clone.into())).await;
            }
        })
    }

    async fn read_loop(&mut self, cancel_token: CancellationToken) -> JoinHandle<()> {
        let read_clone = Arc::clone(&self.read.as_ref().unwrap());
        let cancel_token_clone = cancel_token.clone();
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
                if SystemTime::now() > last_pong.checked_add(Duration::from_secs(ping_duration_clone)).unwrap() {
                    cancel_token_clone.cancel();
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
                                cancel_token_clone.cancel();
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

    pub async fn start_loop(&mut self, cancel_token: CancellationToken) -> (JoinHandle<()>, JoinHandle<()>) {
        let request = self.config.url.clone().into_client_request().unwrap();
        let (conn, response) = connect_async(request).await.unwrap();
        let (write, read) = conn.split();
        
        self.read = Some(Arc::new(Mutex::new(read)));
        self.write = Some(Arc::new(Mutex::new(write)));
     
        let read_loop = self.read_loop(cancel_token.clone()).await;
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
}

pub struct WSManager {
    conn: HashMap<String, Arc<Mutex<Connection>>>
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

        self.conn.insert(name.to_string(), Arc::new(Mutex::new(conn)));
    }

    pub async fn start(&self, tx: broadcast::Sender<(String, u8)>) -> HashMap<String, JoinHandle<()>> {
        let mut res = HashMap::with_capacity(self.conn.len());
        
        for (name, conn ) in &self.conn {
            let conn_clone = Arc::clone(conn);
            let name_clone = name.clone();
            let mut rx_clone = tx.subscribe();
            let manager_handle = tokio::spawn(async move {
                loop {
                    println!("Connected");
                    let cancel_token = CancellationToken::new();
                    
                    let mut read_handle: Option<JoinHandle<()>> = None;
                    let mut ping_handle: Option<JoinHandle<()>> = None;

                    {
                        let mut locked_conn = conn_clone.lock().await;
                        let (read_handle_tmp, ping_handle_tmp) = locked_conn.start_loop(cancel_token.clone()).await;
                        read_handle = Some(read_handle_tmp);
                        ping_handle = Some(ping_handle_tmp);
                    }
                    
                    let read_handle_actual = std::mem::take(&mut read_handle).unwrap();
                    let ping_handle_actual = std::mem::take(&mut ping_handle).unwrap();

                    let abort_read = read_handle_actual.abort_handle();
                    let abort_ping = ping_handle_actual.abort_handle();

                    let cancel_token_select = cancel_token.clone();
                    tokio::select! {
                        maybe_msg = rx_clone.recv() => {
                            if let Ok(msg) = maybe_msg {
                                if msg.0 == name_clone && msg.1 == 0 {
                                    println!("Received abort message");
                                    abort_ping.abort();
                                    abort_read.abort();
                                }
                            }
                        },
                        _ = cancel_token_select.cancelled() => {
                            println!("Canncellled");
                            abort_read.abort();
                            abort_ping.abort();
                        },
                        _ = read_handle_actual => {
                            println!("Read handle");
                            abort_read.abort();
                            abort_ping.abort();
                        }
                        _ = ping_handle_actual => {
                            println!("Ping handle");
                            abort_read.abort();
                            abort_ping.abort();
                        }
                    }
                }
            });
            res.insert(name.clone(), manager_handle);
        }
        res
    }
    
    pub async fn write(&mut self, name: &str, msg: Message) {
        if let Some(conn) = self.conn.get(name) {
            let mut locked_conn = conn.lock().await;
            locked_conn.write(msg).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let hl_config = Config {
        url: "wss://api.hyperliquid.xyz/ws".to_string(),
        ping_duration: 10,
        ping_message: "{\"method\": \"ping\"}".to_string(),
    };
    
    let mut hooks = ReadHooks::new();
    hooks.on_text = Arc::new(Some(Box::new(|text| {
        println!("{:?}", text);
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
