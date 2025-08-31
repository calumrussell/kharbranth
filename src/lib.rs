mod config;
mod connection;
mod manager;

pub use config::Config;
pub use connection::ConnectionMessage;
pub use manager::Manager;

#[cfg(test)]
mod test {
    use std::time::Duration;

    use crate::{Config, ConnectionMessage, Manager};
    use tokio_tungstenite::tungstenite::Message;

    fn create_test_config(name: &str, url: &str) -> Config {
        Config {
            name: name.to_string(),
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: url.to_string(),
            reconnect_timeout: 10,
        }
    }

    #[tokio::test]
    async fn manager_creates_connections() {
        let manager = Manager::new();
        let config = create_test_config("test", "wss://echo.websocket.org");

        manager.new_conn("test", config).await;
        manager.close_conn("test").await;
    }

    #[tokio::test]
    async fn manager_handles_write_to_nonexistent_connection() {
        let manager = Manager::new();

        let message =
            ConnectionMessage::Message("nonexistent".to_string(), Message::Text("test".into()));
        manager.write("nonexistent", message).await;
    }

    #[tokio::test]
    async fn manager_read_channel_works() {
        let manager = Manager::new();
        let mut receiver = manager.read();

        let result = tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn manager_close_conn_works() {
        let manager = Manager::new();

        manager.close_conn("nonexistent").await;
    }

    #[tokio::test]
    async fn config_creation_works() {
        let config = create_test_config("test", "wss://example.com");

        assert_eq!(config.name, "test");
        assert_eq!(config.url, "wss://example.com");
        assert_eq!(config.ping_duration, 10);
        assert_eq!(config.ping_timeout, 15);
        assert_eq!(config.reconnect_timeout, 10);
    }

    #[tokio::test]
    async fn connection_message_variants_work() {
        let msg1 = ConnectionMessage::Message("test".to_string(), Message::Text("hello".into()));
        let msg2 = ConnectionMessage::ReadError("test".to_string());
        let msg3 = ConnectionMessage::WriteError("test".to_string());
        let msg4 = ConnectionMessage::PongReceiveTimeoutError("test".to_string());

        match msg1 {
            ConnectionMessage::Message(name, _) => assert_eq!(name, "test"),
            _ => panic!("Expected Message variant"),
        }

        match msg2 {
            ConnectionMessage::ReadError(name) => assert_eq!(name, "test"),
            _ => panic!("Expected ReadError variant"),
        }

        match msg3 {
            ConnectionMessage::WriteError(name) => assert_eq!(name, "test"),
            _ => panic!("Expected WriteError variant"),
        }

        match msg4 {
            ConnectionMessage::PongReceiveTimeoutError(name) => assert_eq!(name, "test"),
            _ => panic!("Expected PongReceiveTimeoutError variant"),
        }
    }

    #[tokio::test]
    async fn manager_reconnect_nonexistent_connection() {
        let manager = Manager::new();

        let result = manager.reconnect("nonexistent").await;
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Connection 'nonexistent' not found for reconnect"));
    }
}
