use crate::config::Config;
use crate::obs_client::ObsClient;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    pub config: Mutex<Config>,
    pub obs: Mutex<Option<ObsClient>>,
    /// Bumped on every push/manual toggle so a pending auto-hide task can
    /// tell it's been superseded and must not hide a newer playback.
    pub push_gen: AtomicU64,
}

impl AppState {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            obs: Mutex::new(None),
            push_gen: AtomicU64::new(0),
        })
    }
}
