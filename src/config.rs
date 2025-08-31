#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub url: String,
    pub ping_duration: u64,
    pub ping_message: String,
    pub ping_timeout: u64,
    pub reconnect_timeout: u64,
}
