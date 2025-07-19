use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, Message, WSManager};
use log::info;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

#[derive(Debug, Serialize, Deserialize)]
struct HyperliquidSubscribe {
    method: String,
    subscription: HyperliquidSubscriptionMessage,
}

#[derive(Debug, Serialize, Deserialize)]
struct HyperliquidSubscriptionMessage {
    #[serde(rename = "type")]
    typ: String,
    coin: String,
    interval: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting Hyperliquid WebSocket test");

    let manager = WSManager::new();

    let subscription = HyperliquidSubscriptionMessage {
        typ: "candle".to_string(),
        coin: "BTC".to_string(),
        interval: "1m".to_string(),
    };

    let subscribe = HyperliquidSubscribe {
        method: "subscribe".to_string(),
        subscription,
    };

    let subscribe_json = serde_json::to_string(&subscribe)?;

    let config = Config {
        url: "wss://api.hyperliquid.xyz/ws".to_string(),
        ping_duration: 10,
        ping_message: "{\"method\":\"ping\"}".to_string(),
        ping_timeout: 30,
        reconnect_timeout: 5,
        write_on_init: vec![TungsteniteMessage::Text(subscribe_json.into())],
    };

    // Use the new Stream/Sink API
    use futures_util::StreamExt;
    let (_sink, mut stream) = manager.connect_stream("hyperliquid", config).await?;

    // Spawn a task to handle incoming messages
    tokio::spawn(async move {
        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => match message {
                    Message::Text(text) => {
                        info!("Received text: {}", text);
                    }
                    Message::Binary(data) => {
                        info!("Received binary data: {} bytes", data.len());
                    }
                    Message::Close(_) => {
                        info!("Connection closed");
                        break;
                    }
                    _ => {}
                },
                Err(e) => {
                    info!("Stream error: {}", e);
                    break;
                }
            }
        }
    });

    sleep(Duration::from_secs(2)).await;

    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
