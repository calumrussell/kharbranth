mod config;
mod connection;
mod manager;

pub use config::Config;
pub use manager::{Manager, Message};

#[cfg(test)]
mod test {
    use std::time::Duration;

    use crate::{Config, Manager, Message};

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

        manager.new_conn("test", config);
        let _ = manager.close_conn("test");
    }

    #[tokio::test]
    async fn manager_handles_write_to_nonexistent_connection() {
        let manager = Manager::new();

        let message = Message::TextMessage("nonexistent".to_string(), "test".to_string());
        let _ = manager.write("nonexistent", message);
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

        let _ = manager.close_conn("nonexistent");
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
    async fn message_variants_work() {
        let msg1 = Message::TextMessage("test".to_string(), "hello".to_string());
        let msg2 = Message::ReadError("test".to_string(), "error_string".to_string());
        let msg3 = Message::WriteError("test".to_string(), "error_string".to_string());
        let msg4 = Message::PongReceiveTimeoutError("test".to_string());

        match msg1 {
            Message::TextMessage(name, _) => assert_eq!(name, "test"),
            _ => panic!("Expected TextMessage variant"),
        }

        match msg2 {
            Message::ReadError(name, _err_str) => assert_eq!(name, "test"),
            _ => panic!("Expected ReadError variant"),
        }

        match msg3 {
            Message::WriteError(name, _err_str) => assert_eq!(name, "test"),
            _ => panic!("Expected WriteError variant"),
        }

        match msg4 {
            Message::PongReceiveTimeoutError(name) => assert_eq!(name, "test"),
            _ => panic!("Expected PongReceiveTimeoutError variant"),
        }
    }

    #[tokio::test]
    async fn manager_reconnect_nonexistent_connection() {
        let manager = Manager::new();

        let result = manager.reconnect("nonexistent").await;
        assert!(result.is_err());

        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Connection Unknown"));
    }
}
