use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::Result;
use futures_util::{stream::{SplitSink, SplitStream}, SinkExt, StreamExt};
use tokio::{net::TcpStream, sync::{broadcast, Mutex}, task::JoinHandle, time::sleep};
use tokio_tungstenite::{connect_async, tungstenite::{client::IntoClientRequest, Message, Utf8Bytes}, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

struct ReadHooks {
    pub on_text: Arc<dyn Fn(Utf8Bytes) + Send + Sync>,
}

struct Connection {
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
    
    async fn ping_loop(&mut self, ping_message: String, ping_duration: u64) -> JoinHandle<()> {
        let write_clone = Arc::clone(&self.write.as_ref().unwrap());
        tokio::spawn(async move {
            let ping_message_clone = ping_message.into_bytes();
            loop {
                {
                    let mut write_lock = write_clone.lock().await;
                    let _ = write_lock.send(Message::Ping(ping_message_clone.clone().into())).await;
                }
                sleep(Duration::from_secs(ping_duration.into())).await;
            }
        })
    }

    async fn read_loop(&mut self, cancel_token: CancellationToken) -> JoinHandle<()> {
        let read_clone = Arc::clone(&self.read.as_ref().unwrap());
        let cancel_token_clone = cancel_token.clone();
        let callback_clone = Arc::clone(&self.hooks.on_text);
        tokio::spawn(async move {
            loop {
                let mut read_lock = read_clone.lock().await;
                if let Some(received) = read_lock.next().await {
                    if let Ok(msg) = received {
                        match msg {
                            Message::Text(text) => {
                                callback_clone(text);
                            }
                            _ => (),
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
        let ping_loop = self.ping_loop(self.config.ping_message.to_string(), self.config.ping_duration).await;
        
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

struct Config {
    pub url: String,
    pub ping_duration: u64,
    pub ping_message: String,
}

struct WSManager {
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
    
    let hooks = ReadHooks {
        on_text: Arc::new(|text: Utf8Bytes| {
            println!("{:?}", text);
        })
    };
    
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
