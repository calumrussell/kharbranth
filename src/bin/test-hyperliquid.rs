use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, HookType, WSManager};
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

    let mut manager = WSManager::new();

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
        write_on_init: vec![subscribe_json],
    };

    manager.new_conn("hyperliquid", config).await;

    let text_hook = HookType::Text(Box::new(|text| {
        info!("Received candle data: {}", text);
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
            info!("Parsed JSON: {}", parsed);
        };
    }));

    manager.add_hook("hyperliquid", text_hook).await;

    let _handles = manager.start();

    let manager_clone = manager.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(20)).await;
            info!("Sending restart signal to hyperliquid connection");
            if let Err(e) = manager_clone.restart("hyperliquid") {
                info!("Failed to send restart signal: {}", e);
            }
        }
    });

    sleep(Duration::from_secs(2)).await;

    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
