use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsSource = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;
type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub struct ObsClient {
    write: Arc<Mutex<WsSink>>,
    pending: PendingMap,
}

impl ObsClient {
    pub async fn connect(host: &str, port: u16, password: &str) -> anyhow::Result<Self> {
        let url = format!("ws://{host}:{port}");
        let (ws_stream, _) = connect_async(&url).await?;
        let (mut write, mut read) = ws_stream.split();

        let hello = read_next_json(&mut read).await?;
        let rpc_version = hello["d"]["rpcVersion"].as_i64().unwrap_or(1);

        let mut identify = json!({
            "op": 1,
            "d": { "rpcVersion": rpc_version, "eventSubscriptions": 0 }
        });
        if let Some(auth_info) = hello["d"].get("authentication") {
            let challenge = auth_info["challenge"].as_str().unwrap_or_default();
            let salt = auth_info["salt"].as_str().unwrap_or_default();
            identify["d"]["authentication"] = json!(compute_auth(password, salt, challenge));
        }
        write.send(Message::Text(identify.to_string())).await?;

        let identified = read_next_json(&mut read).await?;
        if identified["op"].as_i64() != Some(2) {
            anyhow::bail!("OBS did not identify us — check the WebSocket password: {identified}");
        }

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = pending.clone();

        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                let Ok(Message::Text(text)) = msg else { continue };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                if value["op"].as_i64() == Some(7) {
                    if let Some(request_id) = value["d"]["requestId"].as_str() {
                        if let Some(sender) = pending_for_reader.lock().await.remove(request_id) {
                            let _ = sender.send(value["d"].clone());
                        }
                    }
                }
            }
        });

        Ok(Self {
            write: Arc::new(Mutex::new(write)),
            pending,
        })
    }

    async fn call(&self, request_type: &str, request_data: Value) -> anyhow::Result<Value> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);

        let msg = json!({
            "op": 6,
            "d": { "requestType": request_type, "requestId": request_id, "requestData": request_data }
        });
        self.write
            .lock()
            .await
            .send(Message::Text(msg.to_string()))
            .await?;

        let response = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await??;
        if !response["requestStatus"]["result"].as_bool().unwrap_or(false) {
            anyhow::bail!(
                "OBS request '{request_type}' failed: {}",
                response["requestStatus"]
            );
        }
        Ok(response.get("responseData").cloned().unwrap_or(Value::Null))
    }

    pub async fn save_replay_buffer(&self) -> anyhow::Result<()> {
        self.call("SaveReplayBuffer", json!({})).await?;
        Ok(())
    }

    pub async fn get_replay_buffer_active(&self) -> anyhow::Result<bool> {
        let data = self.call("GetReplayBufferStatus", json!({})).await?;
        Ok(data["outputActive"].as_bool().unwrap_or(false))
    }

    pub async fn start_replay_buffer(&self) -> anyhow::Result<()> {
        self.call("StartReplayBuffer", json!({})).await?;
        Ok(())
    }

    /// Canvas base resolution, for sizing the overlay browser source.
    pub async fn get_canvas_size(&self) -> anyhow::Result<(u32, u32)> {
        let data = self.call("GetVideoSettings", json!({})).await?;
        Ok((
            data["baseWidth"].as_u64().unwrap_or(1920) as u32,
            data["baseHeight"].as_u64().unwrap_or(1080) as u32,
        ))
    }

    /// Creates the on-stream overlay: a Browser Source filling the canvas,
    /// pointed at our local overlay server. The page is transparent until a
    /// clip plays, so it can stay visible permanently.
    pub async fn create_browser_source(
        &self,
        scene: &str,
        name: &str,
        url: &str,
        width: u32,
        height: u32,
    ) -> anyhow::Result<()> {
        self.call(
            "CreateInput",
            json!({
                "sceneName": scene,
                "inputName": name,
                "inputKind": "browser_source",
                "inputSettings": {
                    "url": url,
                    "width": width,
                    "height": height,
                    "shutdown": false,
                    "restart_when_active": false
                }
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn get_last_replay_buffer_replay(&self) -> anyhow::Result<String> {
        let data = self.call("GetLastReplayBufferReplay", json!({})).await?;
        Ok(data["savedReplayPath"].as_str().unwrap_or_default().to_string())
    }

    pub async fn get_media_input_list(&self) -> anyhow::Result<Vec<String>> {
        let data = self.call("GetInputList", json!({})).await?;
        let inputs = data["inputs"].as_array().cloned().unwrap_or_default();
        Ok(inputs
            .into_iter()
            .filter(|i| {
                let kind = i["inputKind"].as_str().unwrap_or_default();
                kind == "ffmpeg_source" || kind.contains("media")
            })
            .map(|i| i["inputName"].as_str().unwrap_or_default().to_string())
            .collect())
    }

    /// Returns (scene names, current program scene name).
    pub async fn get_scene_list(&self) -> anyhow::Result<(Vec<String>, String)> {
        let data = self.call("GetSceneList", json!({})).await?;
        let current = data["currentProgramSceneName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let scenes = data["scenes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s["sceneName"].as_str().unwrap_or_default().to_string())
            .collect();
        Ok((scenes, current))
    }

    /// Creates the media source hidden — it only becomes visible while a
    /// clip is actually playing.
    pub async fn create_media_source(&self, scene: &str, name: &str) -> anyhow::Result<()> {
        let data = self
            .call(
                "CreateInput",
                json!({
                    "sceneName": scene,
                    "inputName": name,
                    "inputKind": "ffmpeg_source",
                    "inputSettings": { "looping": false, "is_local_file": true }
                }),
            )
            .await?;
        if let Some(item_id) = data["sceneItemId"].as_i64() {
            self.set_scene_item_enabled(scene, item_id, false).await?;
        }
        Ok(())
    }

    /// True if any input (of any kind) has this name.
    pub async fn input_exists(&self, name: &str) -> anyhow::Result<bool> {
        let data = self.call("GetInputList", json!({})).await?;
        Ok(data["inputs"]
            .as_array()
            .map(|arr| arr.iter().any(|i| i["inputName"] == name))
            .unwrap_or(false))
    }

    /// Finds every (scene, sceneItemId) pair the source appears in.
    pub async fn find_scene_items(&self, source: &str) -> anyhow::Result<Vec<(String, i64)>> {
        let (scenes, _) = self.get_scene_list().await?;
        let mut found = Vec::new();
        for scene in scenes {
            if let Ok(data) = self
                .call(
                    "GetSceneItemId",
                    json!({ "sceneName": scene, "sourceName": source }),
                )
                .await
            {
                if let Some(id) = data["sceneItemId"].as_i64() {
                    found.push((scene, id));
                }
            }
        }
        Ok(found)
    }

    pub async fn set_scene_item_enabled(
        &self,
        scene: &str,
        item_id: i64,
        enabled: bool,
    ) -> anyhow::Result<()> {
        self.call(
            "SetSceneItemEnabled",
            json!({
                "sceneName": scene,
                "sceneItemId": item_id,
                "sceneItemEnabled": enabled
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn get_scene_item_enabled(&self, scene: &str, item_id: i64) -> anyhow::Result<bool> {
        let data = self
            .call(
                "GetSceneItemEnabled",
                json!({ "sceneName": scene, "sceneItemId": item_id }),
            )
            .await?;
        Ok(data["sceneItemEnabled"].as_bool().unwrap_or(false))
    }

    pub async fn set_input_file(&self, input_name: &str, file_path: &str) -> anyhow::Result<()> {
        self.call(
            "SetInputSettings",
            json!({
                "inputName": input_name,
                "inputSettings": { "local_file": file_path, "is_local_file": true },
                "overlay": true
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn restart_media(&self, input_name: &str) -> anyhow::Result<()> {
        self.call(
            "TriggerMediaInputAction",
            json!({
                "inputName": input_name,
                "mediaAction": "OBS_WEBSOCKET_MEDIA_INPUT_ACTION_RESTART"
            }),
        )
        .await?;
        Ok(())
    }
}

async fn read_next_json(read: &mut WsSource) -> anyhow::Result<Value> {
    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Close(frame) => {
                let detail = frame
                    .map(|f| format!("{} ({})", f.reason, f.code))
                    .unwrap_or_else(|| "no reason given".into());
                anyhow::bail!("OBS closed the connection: {detail}");
            }
            _ => {}
        }
    }
    anyhow::bail!("OBS closed the connection before sending the expected message")
}

fn compute_auth(password: &str, salt: &str, challenge: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let secret = BASE64.encode(hasher.finalize());

    let mut hasher2 = Sha256::new();
    hasher2.update(secret.as_bytes());
    hasher2.update(challenge.as_bytes());
    BASE64.encode(hasher2.finalize())
}
