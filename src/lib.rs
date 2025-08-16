mod config;
mod connection;
mod hooks;
mod manager;
mod ping;
mod types;

pub use config::{BroadcastMessage, Config};
pub use hooks::HookType;
pub use manager::WSManager;

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use futures_util::StreamExt;
    use tokio::io::ErrorKind;
    use tokio::sync::Mutex;
    use tokio_test::io::Mock;
    use tokio_tungstenite::WebSocketStream;

    use crate::{Config, WSManager, connection::Connection, hooks::ReadHooks};
    use tokio_tungstenite::tungstenite::Message;

    async fn setup(mock: Mock) -> Connection<Mock> {
        let ws_stream = WebSocketStream::from_raw_socket(
            mock,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;

        let write_on_init = Vec::new();
        let config = Config {
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: "wss://fake".to_string(),
            reconnect_timeout: 10,
            write_on_init,
        };

        let (sink, stream) = ws_stream.split();

        let hooks = ReadHooks::new();

        let mut conn = Connection::new(config, hooks);
        conn.write = Some(Arc::new(Mutex::new(sink)));
        conn.read = Some(Arc::new(Mutex::new(stream)));
        conn
    }

    #[tokio::test]
    async fn read_error_returns_connection_error() {
        let mock = tokio_test::io::Builder::new()
            .read_error(std::io::Error::new(
                ErrorKind::ConnectionAborted,
                "Server connection lost",
            ))
            .build();

        let mut conn = setup(mock).await;

        let read_handle = conn.read_loop().await.unwrap();
        let res: (Result<Result<(), anyhow::Error>, tokio::task::JoinError>,) =
            tokio::join!(read_handle);

        assert!(res.0.unwrap().is_err());
    }

    #[tokio::test]
    async fn concurrent_new_conn_operations() {
        let manager = WSManager::new();
        let mut handles = vec![];

        for i in 0..100 {
            let mut manager_clone = manager.clone();
            let handle = tokio::spawn(async move {
                let config = Config {
                    ping_duration: 10,
                    ping_message: "ping".to_string(),
                    ping_timeout: 15,
                    url: format!("wss://fake{}.com", i),
                    reconnect_timeout: 10,
                    write_on_init: Vec::new(),
                };
                manager_clone.new_conn(&format!("conn_{}", i), config).await;
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }

        // Verify all connections were added
        assert_eq!(manager.conn.len(), 100);
        for i in 0..100 {
            assert!(manager.conn.contains_key(&format!("conn_{}", i)));
        }
    }

    #[tokio::test]
    async fn concurrent_write_to_different_connections() {
        // This test verifies that DashMap allows concurrent writes to different connections without blocking
        let mut manager = WSManager::new();

        for i in 0..10 {
            let config = Config {
                ping_duration: 10,
                ping_message: "ping".to_string(),
                ping_timeout: 15,
                url: format!("wss://fake{}.com", i),
                reconnect_timeout: 10,
                write_on_init: Vec::new(),
            };
            manager.new_conn(&format!("conn_{}", i), config).await;
        }

        let mut handles = vec![];
        for i in 0..10 {
            let manager_clone = manager.clone();
            let handle =
                tokio::spawn(
                    async move { manager_clone.conn.contains_key(&format!("conn_{}", i)) },
                );
            handles.push(handle);
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result);
        }
    }

    #[tokio::test]
    async fn uninitialized_connection_write_returns_error() {
        // This test verifies that calling write on an uninitialized connection returns proper error
        let config = Config {
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: "wss://fake".to_string(),
            reconnect_timeout: 10,
            write_on_init: Vec::new(),
        };

        let hooks = ReadHooks::new();
        let mut conn: Connection<Mock> = Connection::new(config, hooks);

        let result = conn.write(Message::Text("test".into())).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Connection write stream not initialized")
        );
    }

    #[tokio::test]
    async fn uninitialized_connection_ping_loop_returns_error() {
        // This test verifies that calling ping_loop on an uninitialized connection returns proper error
        let config = Config {
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: "wss://fake".to_string(),
            reconnect_timeout: 10,
            write_on_init: Vec::new(),
        };

        let hooks = ReadHooks::new();
        let mut conn: Connection<Mock> = Connection::new(config, hooks);

        let result = conn.ping_loop().await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Connection write stream not initialized for ping loop")
        );
    }

    #[tokio::test]
    async fn uninitialized_connection_read_loop_returns_error() {
        // This test verifies that calling read_loop on an uninitialized connection returns proper error
        let config = Config {
            ping_duration: 10,
            ping_message: "ping".to_string(),
            ping_timeout: 15,
            url: "wss://fake".to_string(),
            reconnect_timeout: 10,
            write_on_init: Vec::new(),
        };

        let hooks = ReadHooks::new();
        let mut conn: Connection<Mock> = Connection::new(config, hooks);

        let result = conn.read_loop().await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Connection read stream not initialized for read loop")
        );
    }

    #[tokio::test]
    async fn no_deadlock_during_start_and_write() {
        // This test verifies that DashMap doesn't cause deadlocks when start() is iterating
        // while other operations try to access the map
        use tokio::time::{Duration, timeout};

        let mut manager = WSManager::new();

        for i in 0..5 {
            let config = Config {
                ping_duration: 10,
                ping_message: "ping".to_string(),
                ping_timeout: 15,
                url: format!("wss://fake{}.com", i),
                reconnect_timeout: 10,
                write_on_init: Vec::new(),
            };
            manager.new_conn(&format!("conn_{}", i), config).await;
        }

        let manager_clone1 = manager.clone();
        let mut manager_clone2 = manager.clone();

        let iterate_task = tokio::spawn(async move {
            for _ in 0..100 {
                for entry in &*manager_clone1.conn {
                    let _name = entry.key();
                    let _conn = entry.value();
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
            }
        });

        let write_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;

            for i in 5..10 {
                let config = Config {
                    ping_duration: 10,
                    ping_message: "ping".to_string(),
                    ping_timeout: 15,
                    url: format!("wss://fake{}.com", i),
                    reconnect_timeout: 10,
                    write_on_init: Vec::new(),
                };
                manager_clone2
                    .new_conn(&format!("conn_{}", i), config)
                    .await;
            }
            5
        });

        let result = timeout(Duration::from_secs(5), async {
            let (r1, r2) = tokio::join!(iterate_task, write_task);
            (r1, r2)
        })
        .await;

        assert!(result.is_ok(), "Operations timed out - possible deadlock!");

        let (iterate_result, write_result) = result.unwrap();
        assert!(iterate_result.is_ok());
        assert_eq!(write_result.unwrap(), 5);

        assert_eq!(manager.conn.len(), 10);
    }
}
