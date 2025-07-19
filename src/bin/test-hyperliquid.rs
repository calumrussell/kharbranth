use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, ReadHooks, WSManager, AsyncHandler};
use log::info;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

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
    let hooks = ReadHooks::new();

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
        write_on_init: vec![Message::Text(subscribe_json.into())],
    };

    manager.new_conn("hyperliquid", config, hooks).await;

    let _text_subscription = manager.subscribe("hyperliquid", AsyncHandler::new(|text| async move {
        info!("Received candle data: {}", text);
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
            info!("Parsed JSON: {}", parsed);
        }
    })).await?;

  
    let _event_subscription = manager.subscribe_connection_events("hyperliquid", AsyncHandler::new(|event| async move {
        info!("Connection event: {}", event);
    })).await?;

    let (tx, _rx) = broadcast::channel(16);
    let _handles = manager.start(tx.clone());

    let manager_clone = manager.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(30)).await;
            if let Ok(stats) = manager_clone.get_connection_stats("hyperliquid").await {
                info!("Stats - Received: {}, Processed: {}, Dropped: {}", 
                      stats.total_received, stats.total_processed, stats.total_dropped);
            }
        }
    });

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(20)).await;
            info!("Sending restart signal to hyperliquid connection");
            if let Err(e) = tx_clone.send(kharbranth::BroadcastMessage {
                target: "hyperliquid".to_string(),
                action: 0,
            }) {
                info!("Failed to send restart signal: {}", e);
            }
        }
    });

    sleep(Duration::from_secs(2)).await;

    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
