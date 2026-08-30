use crate::config::Config;
use crate::obs_client::ObsClient;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;

/// One entry in the clip library (raw grab or trimmed export).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipEntry {
    pub path: String,
    /// "grab" | "trim"
    pub kind: String,
    pub saved_at_epoch: u64,
    pub duration_secs: f64,
}

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
    /// Bumped on every successful grab so docks can auto-load the new clip
    /// into their trim editor (e.g. when a Stream Deck key triggered it).
    pub grab_seq: u64,
    pub last_grab: Option<String>,
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub obs: Mutex<Option<ObsClient>>,
    /// Bumped on every push/manual toggle so a pending auto-hide task can
    /// tell it's been superseded and must not hide a newer playback.
    pub push_gen: AtomicU64,
    pub overlay: Mutex<OverlayState>,
    /// Recent clips (grabs + trims), newest last; persisted to clips_file.
    pub clips: Mutex<Vec<ClipEntry>>,
    pub clips_file: std::path::PathBuf,
    /// OBS's recording directory, cached after the first lookup — used to
    /// allow-list folder browsing.
    pub record_dir: Mutex<Option<std::path::PathBuf>>,
}

impl AppState {
    pub fn new(
        config: Config,
        clips: Vec<ClipEntry>,
        clips_file: std::path::PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            obs: Mutex::new(None),
            push_gen: AtomicU64::new(0),
            overlay: Mutex::new(OverlayState::default()),
            clips: Mutex::new(clips),
            clips_file,
            record_dir: Mutex::new(None),
        })
    }
}
