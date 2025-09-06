use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, Manager, Message};
use log::{error, info};
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

    let mut read_channel = manager.read();
    tokio::spawn(async move {
        loop {
            match read_channel.recv().await {
                Ok(msg) => {
                    info!("Received message: {:?}", msg);
                }
                Err(e) => {
                    error!("Read channel error: {:?}", e);
                }
            }
        }
    });

    manager.new_conn("hyperliquid", config);

    sleep(Duration::from_secs(2)).await;
    let subscription_msg = Message::TextMessage("hyperliquid".to_string(), subscribe_json.clone());
    let write_resp = manager.write("hyperliquid", subscription_msg.clone());
    info!("Write resp: {:?}", write_resp);

    sleep(Duration::from_secs(10)).await;

    info!("Trigger reconnect");
    manager.reconnect("hyperliquid").await?;

    sleep(Duration::from_secs(10)).await;
    info!("Resubscription");
    let write_resp = manager.write("hyperliquid", subscription_msg.clone());
    info!("Write resp: {:?}", write_resp);

    sleep(Duration::from_secs(30)).await;

    info!("Closing connection...");
    manager.close_conn("hyperliquid")?;

    Ok(())
}
