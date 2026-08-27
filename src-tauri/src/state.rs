use crate::config::Config;
use crate::obs_client::ObsClient;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;

/// What the on-stream overlay page should be showing. The page polls
/// /api/state and reacts to generation changes.
#[derive(Default)]
pub struct OverlayState {
    pub clip_path: Option<std::path::PathBuf>,
    pub generation: u64,
    /// Command channel to the overlay page: bump cmd_seq and set cmd
    /// ("replay" | "pause" | "hide"); the page applies each new seq once.
    pub cmd_seq: u64,
    pub cmd: Option<String>,
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub obs: Mutex<Option<ObsClient>>,
    /// Bumped on every push/manual toggle so a pending auto-hide task can
    /// tell it's been superseded and must not hide a newer playback.
    pub push_gen: AtomicU64,
    pub overlay: Mutex<OverlayState>,
}

impl AppState {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            obs: Mutex::new(None),
            push_gen: AtomicU64::new(0),
            overlay: Mutex::new(OverlayState::default()),
        })
    }
}
