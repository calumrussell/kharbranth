use anyhow::Error;
use tokio_tungstenite::tungstenite::{
    Utf8Bytes,
    protocol::{CloseFrame, frame::Frame},
};
use tokio_util::bytes::Bytes;

pub type ConnectionResult = Result<(), Error>;

pub type BytesFunc = Box<dyn Fn(Bytes) + Send + Sync>;
pub type Utf8BytesFunc = Box<dyn Fn(Utf8Bytes) + Send + Sync>;
pub type CloseFrameFunc = Box<dyn Fn(Option<CloseFrame>) + Send + Sync>;
pub type FrameFunc = Box<dyn Fn(Frame) + Send + Sync>;

pub enum HookType {
    Text(Utf8BytesFunc),
    Binary(BytesFunc),
    Ping(BytesFunc),
    Pong(BytesFunc),
    Close(CloseFrameFunc),
    Frame(FrameFunc),
}
