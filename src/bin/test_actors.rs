use std::time::Duration;
use anyhow::Result;
use tokio::time::sleep;
use kharbranth::{Config, Manager, ConnectionMessage};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    
    println!("Testing new actor handle implementation...");
    
    // Create manager
    let mut manager = Manager::new();

    // Test with a public WebSocket echo server
    let config = Config {
        url: "wss://futures.kraken.com/ws/v1".to_string(),
        ping_timeout: 30,
        ping_duration: 10,
        ping_message:"{'method':'ping'}".to_string(),
        reconnect_timeout: 10,
        write_on_init: vec![],
        connection_init_delay_ms: Some(100),
    };
    
    // Create connection
    println!("Creating connection...");
    manager.new_conn("test", config).await?;
    
    // Start read loop in background
    let manager_clone = manager.clone();
    tokio::spawn(async move {
        manager_clone.read().await;
    });
    
    // Start timeout loop in background
    let manager_clone2 = manager.clone();
    tokio::spawn(async move {
        manager_clone2.read_timeout_loop().await;
    });

    sleep(Duration::from_secs(2)).await;

    let test = r#"{
      "event": "subscribe",
      "product_ids": ["PF_SOLUSD", "PF_XBTUSD", "PF_ETHUSD"],
      "feed": "book"
    }"#;

    let test_msg = ConnectionMessage::Message(
        "test".to_string(), 
        Message::Text(test.into())
    );
    manager.write("test", test_msg).await;
    
    // Let it run for a bit to see messages
    println!("Running for 30 seconds to observe behavior...");
    sleep(Duration::from_secs(30)).await;
    
    // Close connection
    println!("Closing connection...");
    manager.close_conn("test").await;
    
    println!("Test completed!");
    Ok(())
}
