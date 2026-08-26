use crate::config::Config;
use crate::obs_client::ObsClient;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct AppState {
    pub config: Mutex<Config>,
    pub obs: Mutex<Option<ObsClient>>,
}

impl AppState {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            obs: Mutex::new(None),
        })
    }
}
