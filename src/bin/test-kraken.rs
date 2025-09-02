use std::time::Duration;

use anyhow::Result;
use kharbranth::{Config, Manager, Message};
use log::info;
use tokio::time::sleep;


#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    info!("Starting Hyperliquid WebSocket test");

    let manager = Manager::new();

    let config = Config {
        name: "kraken".to_string(),
        url: "wss://futures.kraken.com/ws/v1".to_string(),
        ping_duration: 10,
        ping_message: r#"{"method":"ping"}"#.to_string(),
        ping_timeout: 30,
        reconnect_timeout: 5,
    };

    manager.new_conn("kraken", config).await;

    let mut read_channel = manager.read();
    tokio::spawn(async move {
        while let Ok(msg) = read_channel.recv().await {
            info!("Received message: {:?}", msg);
        }
    });

    let msg = r#"{"event":"subscribe", "feed": "book", "product_ids": ["PF_XBTUSD"]}"#.to_string();
    sleep(Duration::from_secs(5)).await;
    let subscription_msg = Message::TextMessage("kraken".to_string(), msg);
    manager.write("kraken", subscription_msg.clone()).await;
    sleep(Duration::from_secs(5)).await;

    sleep(Duration::from_secs(10)).await;

    let msg0 = r#"{"event":"unsubscribe", "feed": "book", "product_ids": ["PF_XBTUSD"]}"#.to_string();
    sleep(Duration::from_secs(5)).await;
    let unsubscription_msg = Message::TextMessage("kraken".to_string(), msg0);
    manager.write("kraken", unsubscription_msg.clone()).await;
    sleep(Duration::from_secs(40)).await;

    info!("Closing connection...");
    manager.close_conn("kraken").await;

    Ok(())
}
