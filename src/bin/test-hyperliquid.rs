use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, Manager, Message};
use log::info;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

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

    let manager = Manager::new();

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
        name: "hyperliquid".to_string(),
        url: "wss://api.hyperliquid.xyz/ws".to_string(),
        ping_duration: 10,
        ping_message: r#"{"method":"ping"}"#.to_string(),
        ping_timeout: 30,
        reconnect_timeout: 5,
    };

    manager.new_conn("hyperliquid", config).await;

    let mut read_channel = manager.read();
    tokio::spawn(async move {
        while let Ok(msg) = read_channel.recv().await {
            info!("Received message: {:?}", msg);
        }
    });

    sleep(Duration::from_secs(2)).await;
    let subscription_msg = Message::TextMessage("hyperliquid".to_string(), subscribe_json);
    manager.write("hyperliquid", subscription_msg.clone());

    sleep(Duration::from_secs(5)).await;

    info!("Trigger reconnect");
    let _ = manager.reconnect("hyperliquid").await;
    manager.write("hyperliquid", subscription_msg);

    sleep(Duration::from_secs(20)).await;

    info!("Closing connection...");
    manager.close_conn("hyperliquid").await;

    Ok(())
}
