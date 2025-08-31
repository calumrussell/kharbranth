use std::sync::Arc;

use anyhow::{Result, anyhow};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use log::debug;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;

use crate::config::Config;

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

#[derive(Clone, Debug)]
pub enum ConnectionMessage {
    Message(String, Message),
    ReadError(String),
    WriteError(String),
}

type Reader = SplitStream<WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>>;

pub struct ReadActor {
    name: String,
    reader: Reader,
    bytes_recv: u64,
    send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
}

impl ReadActor {
    fn new(
        name: String,
        reader: Reader,
        send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
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
                            let _ = self.send.send(ConnectionMessage::Message(self.name.clone(), msg));
                        },
                        Some(Err(_e)) => {
                            let _ = self.send.send(ConnectionMessage::ReadError(self.name.clone()));
                            break;
                        },
                        None => {
                            let _ = self.send.send(ConnectionMessage::ReadError(self.name.clone()));
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

impl ReadActorHandle {
    pub fn new(
        name: String,
        reader: Reader,
        cancel_token: CancellationToken,
        sender: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    ) -> Self {
        let mut actor = ReadActor::new(name, reader, Arc::clone(&sender));
        tokio::spawn(async move {
            actor.run(cancel_token).await;
        });

        Self
    }
}

type Writer = SplitSink<WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>, Message>;

pub struct WriteActor {
    name: String,
    writer: Writer,
    read: tokio::sync::broadcast::Receiver<ConnectionMessage>,
    global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
}

impl WriteActor {
    fn new(
        name: String,
        writer: Writer,
        read: tokio::sync::broadcast::Receiver<ConnectionMessage>,
        global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    ) -> Self {
        Self { name, writer, read, global_send }
    }

    async fn handle_message(&mut self, msg: ConnectionMessage) {
        match msg {
            ConnectionMessage::Message(_name, msg) => {
                if let Err(_) = self.writer.send(msg).await {
                    let _ = self.global_send.send(ConnectionMessage::WriteError(self.name.clone()));
                }
            },
            _ => (),
        }
    }

    async fn run(&mut self, cancel_token: CancellationToken) {
        loop {
            tokio::select! {
                msg_recv = self.read.recv() => {
                    match msg_recv {
                        Ok(msg) => {
                            self.handle_message(msg).await;
                        },
                        Err(_e) => {
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
    pub sender: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
}

impl WriteActorHandle {
    pub fn new(
        name: String, 
        writer: Writer, 
        cancel_token: CancellationToken,
        global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    ) -> Self {
        let (sender, receiver) = tokio::sync::broadcast::channel(8);
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
pub struct Connection {
    name: String,
    pub writer: WriteActorHandle,
    pub reader: ReadActorHandle,
    pub config: Config,
    pub cancel_token: CancellationToken,
}

impl Connection {
    pub async fn new(
        name: &str,
        config: Config,
        global_send: Arc<tokio::sync::broadcast::Sender<ConnectionMessage>>,
    ) -> Result<Self> {
        let request = config
            .url
            .clone()
            .into_client_request()
            .map_err(|e| anyhow!("Invalid WebSocket URL '{}': {}", config.url, e))?;

        match connect_async(request).await {
            Ok((conn, response)) => {
                debug!("Handshake response: {:?}", response);
                let (write_stream, read_stream) = conn.split();

                let cancel_token = CancellationToken::new();
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

                Ok(Self {
                    name: name.to_string(),
                    writer,
                    reader,
                    config,
                    cancel_token,
                })
            }
            Err(e) => Err(anyhow!(ConnectionError::ConnectionInitFailed(
                e.to_string()
            ))),
        }
    }
}
