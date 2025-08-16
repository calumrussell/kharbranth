use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Config {
    pub url: String,
    pub ping_duration: u64,
    pub ping_message: String,
    pub ping_timeout: u64,
    pub reconnect_timeout: u64,
    pub write_on_init: Vec<String>,
    pub connection_init_delay_ms: Option<u64>,
}

impl Config {
    pub fn connection_init_delay(&self) -> u64 {
        self.connection_init_delay_ms.unwrap_or(500)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub target: String,
    pub action: u8,
}

#[derive(Debug)]
pub enum BroadcastMessageType {
    Restart,
}

impl TryFrom<u8> for BroadcastMessageType {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(BroadcastMessageType::Restart),
            _ => Err(format!("Unknown broadcast message type: {}", value)),
        }
    }
}
