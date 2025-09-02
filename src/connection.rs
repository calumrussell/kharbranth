use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use chrono::Local;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use log::{debug, error};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::{WebSocketStream, connect_async, tungstenite::client::IntoClientRequest};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, manager::Message};

#[derive(Debug)]
pub enum ConnectionError {
    ConnectionInitFailed(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::ConnectionInitFailed(msg) => {
                write!(f, "Connection initialization failed: {:?}", msg)
            }
        }
    }
}

impl std::error::Error for ConnectionError {}

type Reader = SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>>;

pub struct ReadActor {
    name: String,
    reader: Reader,
    bytes_recv: u64,
    send: Arc<tokio::sync::broadcast::Sender<Message>>,
}

impl ReadActor {
    fn new(
        name: String,
        reader: Reader,
        send: Arc<tokio::sync::broadcast::Sender<Message>>,
    ) -> Self {
        Self {
            name,
            reader,
            bytes_recv: 0,
            send,
        }
    }

    async fn run(&mut self, cancel_token: CancellationToken) {
        loop {
            tokio::select! {
                msg_result = self.reader.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            self.bytes_recv += msg.len() as u64;
                            match msg {
                                TungsteniteMessage::Ping(data) => {
                                    let text = String::from_utf8_lossy(&data).to_string();
                                    let _ = self.send.send(Message::PingMessage(self.name.clone(), text));
                                },
                                TungsteniteMessage::Pong(data) => {
                                    let text = String::from_utf8_lossy(&data).to_string();
                                    let _ = self.send.send(Message::PongMessage(self.name.clone(), text));
                                },
                                TungsteniteMessage::Text(text) => {
                                    let _ = self.send.send(Message::TextMessage(self.name.clone(), text.to_string()));
                                },
                                TungsteniteMessage::Binary(data) => {
                                    let _ = self.send.send(Message::BinaryMessage(self.name.clone(), data.to_vec()));
                                },
                                TungsteniteMessage::Close(close_frame) => {
                                    let reason = close_frame.map(|f| f.reason.to_string());
                                    let _ = self.send.send(Message::CloseMessage(self.name.clone(), reason));
                                },
                                TungsteniteMessage::Frame(_frame) => {
                                    let _ = self.send.send(Message::FrameMessage(self.name.clone(), "Raw frame received".to_string()));
                                }
                            }
                        },
                        Some(Err(e)) => {
                            error!("ReadError for {:?}: {:?}", self.name, e);
                            let _ = self.send.send(Message::ReadError(self.name.clone(), e.to_string()));
                            break;
                        },
                        None => {
                            error!("ReadError for {:?}: None", self.name);
                            let _ = self.send.send(Message::ReadError(self.name.clone(), "None".to_string()));
                            break;
                        }
                    }
                },
                _ = cancel_token.cancelled() => break,
            }
        }
    }
}

#[derive(Clone)]
pub struct ReadActorHandle;

pub struct PingActor {
    name: String,
    config: Config,
    writer_send: Arc<tokio::sync::broadcast::Sender<Message>>,
    global_recv: tokio::sync::broadcast::Receiver<Message>,
    last_pong_time: Option<i64>,
}

impl PingActor {
    fn new(
        name: String,
        config: Config,
        writer_send: Arc<tokio::sync::broadcast::Sender<Message>>,
        global_recv: tokio::sync::broadcast::Receiver<Message>,
    ) -> Self {
        Self {
            name,
            config,
            writer_send,
            global_recv,
            last_pong_time: None,
        }
    }

    async fn run(&mut self, cancel_token: CancellationToken) {
        let mut ping_duration =
            tokio::time::interval(Duration::from_secs(self.config.ping_duration));
        let mut ping_timeout = tokio::time::interval(Duration::from_secs(self.config.ping_timeout));
        let ping_msg_bytes: tokio_tungstenite::tungstenite::Bytes =
            self.config.ping_message.clone().into();

        loop {
            tokio::select! {
                _ = ping_duration.tick() => {
                    let ping_text = String::from_utf8_lossy(&ping_msg_bytes).to_string();
                    let _ = self.writer_send.send(Message::PingMessage(
                        self.name.clone(),
                        ping_text
                    ));
                },
                _ = ping_timeout.tick() => {
                    let now = Local::now().timestamp();
                    if let Some(last_pong) = self.last_pong_time {
                        if now - last_pong > self.config.ping_timeout as i64 {
                            let _ = self.writer_send.send(Message::PongReceiveTimeoutError(self.name.clone()));
                        }
                    }
                },
                _ = cancel_token.cancelled() => break,
                msg_result = self.global_recv.recv() => {
                    if let Ok(Message::PongMessage(conn_name, _val)) = msg_result {
                        if conn_name == self.name {
                            self.last_pong_time = Some(Local::now().timestamp());
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct PingActorHandle;

impl PingActorHandle {
    pub fn new(
        name: String,
        config: Config,
        writer_send: Arc<tokio::sync::broadcast::Sender<Message>>,
        global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
        cancel_token: CancellationToken,
    ) -> Self {
        let global_recv = global_send.subscribe();
        let mut actor = PingActor::new(name, config, writer_send, global_recv);
        tokio::spawn(async move {
            actor.run(cancel_token).await;
        });

        Self
    }
}

impl ReadActorHandle {
    pub fn new(
        name: String,
        reader: Reader,
        cancel_token: CancellationToken,
        sender: Arc<tokio::sync::broadcast::Sender<Message>>,
    ) -> Self {
        let mut actor = ReadActor::new(name, reader, Arc::clone(&sender));
        tokio::spawn(async move {
            actor.run(cancel_token).await;
        });

        Self
    }
}

type Writer =
    SplitSink<WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>, TungsteniteMessage>;

pub struct WriteActor {
    name: String,
    writer: Writer,
    read: tokio::sync::broadcast::Receiver<Message>,
    global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
}

impl WriteActor {
    fn new(
        name: String,
        writer: Writer,
        read: tokio::sync::broadcast::Receiver<Message>,
        global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
    ) -> Self {
        Self {
            name,
            writer,
            read,
            global_send,
        }
    }

    async fn handle_message(&mut self, msg: Message) {
        let tungstenite_msg = match msg {
            Message::PingMessage(_name, data) => TungsteniteMessage::Ping(data.into_bytes().into()),
            Message::PongMessage(_name, data) => TungsteniteMessage::Pong(data.into_bytes().into()),
            Message::TextMessage(_name, text) => TungsteniteMessage::Text(text.into()),
            Message::BinaryMessage(_name, data) => TungsteniteMessage::Binary(data.into()),
            Message::CloseMessage(_name, reason) => {
                let close_frame = reason.map(|r| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: r.into(),
                });
                TungsteniteMessage::Close(close_frame)
            }
            _ => return, // Don't handle error messages or frame messages
        };

        println!("Writing: {:?}", tungstenite_msg);

        if let Err(e) = self.writer.send(tungstenite_msg).await {
            error!("Write Error for {:?}: {:?}", self.name, e.to_string());
            let _ = self
                .global_send
                .send(Message::WriteError(self.name.clone(), e.to_string()));
        }
    }

    async fn run(&mut self, cancel_token: CancellationToken) {
        loop {
            tokio::select! {
                msg_recv = self.read.recv() => {
                    println!("Write Actor Recv: {:?}", msg_recv);

                    match msg_recv {
                        Ok(msg) => {
                            self.handle_message(msg).await;
                        },
                        Err(e) => {
                            error!("WriteActor for '{}' exiting due to recv error: {:?}", self.name, e);
                            break;
                        }
                    }
                },
                _ = cancel_token.cancelled() => break,
            }
        }
    }
}

#[derive(Clone)]
pub struct WriteActorHandle {
    pub sender: Arc<tokio::sync::broadcast::Sender<Message>>,
}

impl WriteActorHandle {
    pub fn new(
        name: String,
        writer: Writer,
        cancel_token: CancellationToken,
        global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
    ) -> Self {
        let (sender, receiver) = tokio::sync::broadcast::channel(512);
        let mut actor = WriteActor::new(name, writer, receiver, global_send);
        tokio::spawn(async move {
            actor.run(cancel_token).await;
        });

        Self {
            sender: Arc::new(sender),
        }
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct Connection {
    pub name: String,
    pub writer: WriteActorHandle,
    pub reader: ReadActorHandle,
    pub ping: PingActorHandle,
    pub config: Config,
    pub cancel_token: CancellationToken,
}

impl Connection {
    pub async fn new(
        name: &str,
        config: Config,
        global_send: Arc<tokio::sync::broadcast::Sender<Message>>,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let request = match config.url.clone().into_client_request() {
            Ok(req) => req,
            Err(e) => {
                error!("Invalid WebSocket URL '{}': {}", config.url, e);
                return Err(anyhow!(ConnectionError::ConnectionInitFailed(format!(
                    "Invalid URL: {}",
                    e
                ))));
            }
        };

        match connect_async(request).await {
            Ok((conn, response)) => {
                debug!("Handshake response: {:?}", response);
                let (write_stream, read_stream) = conn.split();

                let writer = WriteActorHandle::new(
                    name.to_string(),
                    write_stream,
                    cancel_token.clone(),
                    Arc::clone(&global_send),
                );
                let reader = ReadActorHandle::new(
                    name.to_string(),
                    read_stream,
                    cancel_token.clone(),
                    Arc::clone(&global_send),
                );
                let ping = PingActorHandle::new(
                    name.to_string(),
                    config.clone(),
                    Arc::clone(&writer.sender),
                    Arc::clone(&global_send),
                    cancel_token.clone(),
                );

                Ok(Self {
                    name: name.to_string(),
                    writer,
                    reader,
                    ping,
                    config,
                    cancel_token,
                })
            }
            Err(e) => {
                error!("Failed to connect to '{}': {}", config.url, e);
                Err(anyhow!(ConnectionError::ConnectionInitFailed(
                    e.to_string()
                )))
            }
        }
    }
}
