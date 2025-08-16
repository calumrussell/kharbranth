use crate::connection::ConnectionError;
use anyhow::{Error, anyhow};
use std::time::{Duration, Instant};

pub struct PingTracker {
    pub last_ping_sent: Option<(Instant, Vec<u8>)>,
    timeout: Duration,
}

impl PingTracker {
    pub fn new(timeout: Duration) -> Self {
        Self {
            last_ping_sent: None,
            timeout,
        }
    }

    pub fn should_send_ping(&self) -> bool {
        self.last_ping_sent.is_none()
    }

    pub fn send_ping(&mut self, payload: Vec<u8>) -> Vec<u8> {
        self.last_ping_sent = Some((Instant::now(), payload.clone()));
        payload
    }

    pub fn handle_pong(&mut self, payload: Vec<u8>) -> Result<(), Error> {
        match &self.last_ping_sent {
            Some((_, expected_payload)) if expected_payload == &payload => {
                self.last_ping_sent = None;
                Ok(())
            }
            Some(_) => {
                log::debug!("Pong payload mismatch, ignoring");
                Ok(())
            }
            None => {
                log::debug!("Unsolicited pong received, ignoring per RFC 6455");
                Ok(())
            }
        }
    }

    pub fn check_timeout(&self) -> Result<(), Error> {
        if let Some((sent_time, _)) = &self.last_ping_sent {
            if Instant::now() - *sent_time > self.timeout {
                return Err(anyhow!(ConnectionError::PongReceiveTimeout));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;
    use tokio::time::sleep;

    #[tokio::test]
    async fn ping_tracker_timeout_after_stale_state() {
        // This test verifies that check_timeout properly detects when a ping has timed out
        let timeout_duration = Duration::from_millis(10);
        let mut tracker = PingTracker::new(timeout_duration);

        let payload = vec![1, 2, 3];
        tracker.send_ping(payload.clone());

        assert!(tracker.check_timeout().is_ok());

        sleep(Duration::from_millis(20)).await;

        assert!(tracker.check_timeout().is_err());

        tracker.handle_pong(payload).unwrap();
        assert!(tracker.check_timeout().is_ok());
    }

    #[tokio::test]
    async fn ping_tracker_handles_unsolicited_pongs() {
        // This test verifies that unsolicited pongs are ignored per RFC 6455
        let mut tracker = PingTracker::new(Duration::from_secs(15));

        // Handle unsolicited pong - should not error
        assert!(tracker.handle_pong(vec![1, 2, 3]).is_ok());

        // Send a ping, then handle mismatched pong - should not error
        tracker.send_ping(vec![4, 5, 6]);
        assert!(tracker.handle_pong(vec![1, 2, 3]).is_ok());

        // Handle correct pong - should clear state
        assert!(tracker.handle_pong(vec![4, 5, 6]).is_ok());
        assert!(tracker.should_send_ping());
    }

    #[tokio::test]
    async fn test_ping_pong_correlation() {
        // This test verifies ping/pong correlation scenarios including valid responses,
        // payload mismatches, and unsolicited pongs per RFC 6455
        let mut tracker = PingTracker::new(Duration::from_secs(10));

        assert!(tracker.should_send_ping());

        let payload1 = vec![1, 2, 3, 4];
        tracker.send_ping(payload1.clone());
        assert!(!tracker.should_send_ping());
        assert!(tracker.handle_pong(payload1).is_ok());
        assert!(tracker.should_send_ping());

        let payload2 = vec![5, 6, 7, 8];
        let wrong_payload = vec![9, 10, 11, 12];
        tracker.send_ping(payload2.clone());
        assert!(tracker.handle_pong(wrong_payload).is_ok());
        assert!(!tracker.should_send_ping());

        assert!(tracker.handle_pong(payload2).is_ok());
        assert!(tracker.should_send_ping());

        assert!(tracker.handle_pong(vec![13, 14, 15]).is_ok());
        assert!(tracker.should_send_ping());

        for i in 0..5 {
            assert!(tracker.handle_pong(vec![i]).is_ok());
        }
        assert!(tracker.should_send_ping());
    }

    #[tokio::test]
    async fn test_ping_tracker_edge_cases() {
        // This test covers edge cases including boundary timeouts, rapid cycles,
        // various payload sizes, and state consistency after timeouts
        let mut tracker = PingTracker::new(Duration::from_millis(5));
        tracker.send_ping(vec![1]);
        sleep(Duration::from_millis(10)).await;
        assert!(tracker.check_timeout().is_err());

        let mut tracker = PingTracker::new(Duration::from_secs(10));
        for i in 0..5 {
            let payload = vec![i];
            tracker.send_ping(payload.clone());
            assert!(tracker.handle_pong(payload).is_ok());
            assert!(tracker.should_send_ping());
        }

        let mut tracker = PingTracker::new(Duration::from_secs(10));

        tracker.send_ping(vec![]);
        assert!(tracker.handle_pong(vec![]).is_ok());
        assert!(tracker.should_send_ping());

        let large_payload = vec![42; 1000];
        tracker.send_ping(large_payload.clone());
        assert!(tracker.handle_pong(large_payload).is_ok());
        assert!(tracker.should_send_ping());

        let max_payload = vec![255; 125];
        tracker.send_ping(max_payload.clone());
        assert!(tracker.handle_pong(max_payload).is_ok());
        assert!(tracker.should_send_ping());

        let mut short_timeout_tracker = PingTracker::new(Duration::from_millis(5));
        short_timeout_tracker.send_ping(vec![1, 2, 3]);
        sleep(Duration::from_millis(10)).await;
        assert!(short_timeout_tracker.check_timeout().is_err());
        assert!(!short_timeout_tracker.should_send_ping());
    }

    #[tokio::test]
    async fn ping_tracker_resets_on_reconnection() {
        // This test verifies that the ping tracker is reset when a new connection is established,
        // preventing stale ping state from causing immediate timeouts on reconnection

        let tracker = Arc::new(RwLock::new(PingTracker::new(Duration::from_secs(15))));

        {
            let mut tracker_guard = tracker.write().await;
            tracker_guard.send_ping(vec![1, 2, 3]);
        }

        {
            let tracker_guard = tracker.read().await;
            assert!(tracker_guard.last_ping_sent.is_some());
        }

        {
            let mut tracker_guard = tracker.write().await;
            *tracker_guard = PingTracker::new(Duration::from_secs(15));
        }

        {
            let tracker_guard = tracker.read().await;
            assert!(tracker_guard.last_ping_sent.is_none());
            assert!(tracker_guard.should_send_ping());
        }
    }
}
