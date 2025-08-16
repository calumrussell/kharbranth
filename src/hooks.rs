pub use crate::types::HookType;
use crate::types::{BytesFunc, CloseFrameFunc, FrameFunc, Utf8BytesFunc};
use std::sync::Arc;

pub struct ReadHooks {
    pub on_text: Arc<Option<Utf8BytesFunc>>,
    pub on_binary: Arc<Option<BytesFunc>>,
    pub on_ping: Arc<Option<BytesFunc>>,
    pub on_pong: Arc<Option<BytesFunc>>,
    pub on_close: Arc<Option<CloseFrameFunc>>,
    pub on_frame: Arc<Option<FrameFunc>>,
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

    pub fn add_hook(&mut self, hook: HookType) {
        match hook {
            HookType::Text(func) => self.on_text = Arc::new(Some(func)),
            HookType::Binary(func) => self.on_binary = Arc::new(Some(func)),
            HookType::Ping(func) => self.on_ping = Arc::new(Some(func)),
            HookType::Pong(func) => self.on_pong = Arc::new(Some(func)),
            HookType::Close(func) => self.on_close = Arc::new(Some(func)),
            HookType::Frame(func) => self.on_frame = Arc::new(Some(func)),
        }
    }
}

impl Default for ReadHooks {
    fn default() -> Self {
        Self::new()
    }
}
