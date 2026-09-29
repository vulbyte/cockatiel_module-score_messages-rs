use futures_util::{SinkExt, StreamExt};
use prost::Message;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{info, warn};
use tracing_subscriber::FmtSubscriber;

use cockatiel_client::{proto::container::Payload, proto::*, CockatielClient, PromptKind};

use score_messages_rs::{apply_flat_overlay, default_config, flat_map_for_write, score_message, Config};

type WsWriteHalf = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    WsMessage,
>;

/// Send a Prompt to the engine (forwarded to connected UIs) and wait for the
/// operator's response (`PromptResponse.reason`). Returns None on cancel/timeout.
async fn prompt_for_input(
    write_ws: &Arc<AsyncMutex<WsWriteHalf>>,
    prompt_rx: &mut mpsc::UnboundedReceiver<PromptResponse>,
    auth_token: &str,
    module_name: &str,
    instance_uuid: &str,
    title: &str,
    details: &str,
    input_label: &str,
    kind: PromptKind,
    timeout: u32,
) -> Option<String> {
    let prompt_id = uuid::Uuid::now_v7().to_string();
    let prompt_type = match kind {
        PromptKind::Boolean => PromptType::Boolean,
        PromptKind::String => PromptType::String,
        PromptKind::Credential => PromptType::Credential,
    };
    let prompt = Prompt {
        prompt_id_uuid7: prompt_id.clone(),
        prompt: title.to_string(),
        details: details.to_string(),
        yes_dialog: "Submit".to_string(),
        no_dialog: "Cancel".to_string(),
        timeout,
        origin: module_name.to_string(),
        origin_uuid7: String::new(),
        instructions: String::new(),
        link: String::new(),
        input_label: input_label.to_string(),
        prompt_type: prompt_type as i32,
    };
    let container = Container {
        version: 1,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(Payload::Prompt(prompt)),
    };
    let mut buf = Vec::new();
    if container.encode(&mut buf).is_err() {
        return None;
    }
    let mut guard = write_ws.lock().await;
    if guard.send(WsMessage::Binary(buf)).await.is_err() {
        return None;
    }
    drop(guard);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout as u64 + 10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(10), prompt_rx.recv()).await {
            Ok(Some(resp)) if resp.prompt_id_uuid7 == prompt_id => {
                return if resp.accepted {
                    Some(resp.reason)
                } else {
                    None
                };
            }
            Ok(Some(_)) => continue, // a different prompt's response
            Ok(None) => return None,
            // The 10s poll interval elapsed with no response yet: keep waiting
            // until the real deadline (the `timeout` seconds above), rather than
            // bailing out 10 seconds in and auto-cancelling every prompt.
            Err(_) => continue,
        }
    }
    None
}

/// Write config under the module_specific object using the flat credential
/// keys, preserving the rest of the file (connection fields etc.) and
/// migrating any legacy top-level nested values into the flat keys.
fn write_config(cfg: &Config) {
    let mut root: serde_json::Value = std::fs::read_to_string("config.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let ms = root
        .get("module_specific")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    root["module_specific"] = serde_json::Value::Object(flat_map_for_write(cfg, &root, &ms));
    if let Ok(pretty) = serde_json::to_string_pretty(&root) {
        let _ = std::fs::write("config.json", pretty);
    }
}

async fn load_config(
    write_ws: &Arc<AsyncMutex<WsWriteHalf>>,
    prompt_rx: &mut mpsc::UnboundedReceiver<PromptResponse>,
    auth_token: &str,
    module_name: &str,
    instance_uuid: &str,
) -> Config {
    if let Ok(s) = std::fs::read_to_string("config.json") {
        if let Ok(root) = serde_json::from_str::<serde_json::Value>(&s) {
            // The engine stores this module's non-sensitive credential
            // settings under module_specific, keyed by the flat credential
            // keys. Map those onto the nested Config structure.
            if let Some(ms) = root.get("module_specific").and_then(|v| v.as_object()) {
                let legacy = serde_json::from_value::<Config>(root.clone())
                    .unwrap_or_else(|_| default_config());
                let cfg = apply_flat_overlay(legacy, ms);
                // Config convention: settings are created with their default
                // when missing — persist any absent flat keys so every setting
                // always exists and is editable in place.
                write_config(&cfg);
                return cfg;
            }
            // Legacy layouts stored the nested Config at the top level.
            if let Ok(cfg) = serde_json::from_value::<Config>(root) {
                return cfg;
            }
        }
    }

    // No saved config: ask the operator via the engine prompt subwindow
    // instead of silently writing a default.
    let emoji_answer = prompt_for_input(
        write_ws,
        prompt_rx,
        auth_token,
        module_name,
        instance_uuid,
        "Configure score-messages module",
        "No saved config was found. Enable the emoji rule (reward messages containing emoji)?",
        "Enable emoji rule? (y/n)",
        PromptKind::Boolean,
        default_config().prompt_timeout_secs,
    )
    .await;

    let freq_answer = prompt_for_input(
        write_ws,
        prompt_rx,
        auth_token,
        module_name,
        instance_uuid,
        "Configure score-messages module",
        "Enable the frequency rule (punish users who post again too soon)?",
        "Enable frequency rule? (y/n)",
        PromptKind::Boolean,
        default_config().prompt_timeout_secs,
    )
    .await;

    let mut default = default_config();
    default.emoji.toggle = emoji_answer
        .as_deref()
        .map(|c| c.trim().eq_ignore_ascii_case("y") || c.trim().eq_ignore_ascii_case("yes"))
        .unwrap_or(default.emoji.toggle);
    default.frequency.toggle = freq_answer
        .as_deref()
        .map(|c| c.trim().eq_ignore_ascii_case("y") || c.trim().eq_ignore_ascii_case("yes"))
        .unwrap_or(default.frequency.toggle);

    write_config(&default);
    default
}

/// Encode and send a Container on the shared write half (used by the read loop
/// and the config prompts alike).
async fn send_container(write_shared: &Arc<AsyncMutex<WsWriteHalf>>, container: Container) {
    let mut buf = Vec::new();
    if container.encode(&mut buf).is_ok() {
        let mut w = write_shared.lock().await;
        let _ = w.send(WsMessage::Binary(buf)).await;
    }
}

/// Build the pre-process ACK for a message: the engine advances the stage only
/// when every tracked recipient replies with the same `message_uuid7`, so we
/// echo the raw ChatMessage back (or an empty body when there was none) on
/// every path — scoring or not.
fn ack_preprocess_container(
    auth_token: &str,
    module_name: &str,
    instance_uuid: &str,
    message_uuid7: String,
    raw_message: Option<ChatMessage>,
    audio: Vec<u8>,
    audio_type: String,
) -> Container {
    Container {
        version: 1,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(Payload::MessagePreProcess(MessagePreProcess {
            message_uuid7,
            raw_message,
            audio,
            audio_type,
        })),
    }
}

/// Build the adjust-score payload for a scored user. A uuid7-shaped id targets
/// the user DB row directly; anything else (platform handle) falls back to the
/// platform+handle pair. The engine's `userdb_adjust_score` applies the REAL
/// signed delta to `score` without touching the human-rating counters.
fn rating_payload(user_id: &str, chat: &ChatMessage, delta: i64, notes: &[String]) -> serde_json::Value {
    let reason = format!("score-messages: {}", notes.join(","));
    let uuid7_shaped =
        user_id.chars().filter(|c| *c == '-').count() == 4 && user_id.len() == 36;
    if uuid7_shaped {
        serde_json::json!({ "uuid7": user_id, "delta": delta, "reason": reason })
    } else {
        serde_json::json!({ "platform": chat.platform, "handle": user_id, "delta": delta, "reason": reason })
    }
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let client = CockatielClient::connect("config.json").await?;
    let (write, read) = client.stream.split();
    let write_shared: Arc<AsyncMutex<WsWriteHalf>> = Arc::new(AsyncMutex::new(write));
    let auth_token = client.auth_token.clone();
    let instance_uuid = client.instance_uuid7.clone();
    let module_name = client.config.module_name.clone();

    // Channel carrying PromptResponses from the engine to the config prompt,
    // so `prompt_for_input` can await the operator's typed answer.
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<PromptResponse>();

    // Shared config: the read task must run while config is being loaded (so
    // the config prompt can receive its response), so it reads config from an
    // Arc<Mutex> that is populated right after load_config returns.
    let config_shared: Arc<Mutex<Config>> = Arc::new(Mutex::new(default_config()));

    // Read task: forward PromptResponses to the awaiting prompt AND handle
    // message pre-processing. Spawned BEFORE load_config so prompts work.
    // Owns the read half + session identity so it can reconnect with backoff
    // when the engine drops the socket (instead of dying and leaving main to
    // sleep forever while the watchdog severs the unresponsive module).
    {
        let prompt_tx_task = prompt_tx.clone();
        let config_shared = Arc::clone(&config_shared);
        let write_shared = Arc::clone(&write_shared);
        let mut auth_token = auth_token.clone();
        let mut module_name = module_name.clone();
        let mut instance_uuid = instance_uuid.clone();
        let mut read = read;
        tokio::spawn(async move {
            let mut last_msg: HashMap<String, Instant> = HashMap::new();
            'reconnect: loop {
                loop {
                    let Some(msg) = read.next().await else { break };
                    let data = match msg {
                        Ok(WsMessage::Binary(d)) => d,
                        Ok(WsMessage::Close(_)) => {
                            info!("Engine closed connection");
                            break;
                        }
                        Ok(_) => continue,
                        Err(e) => {
                            warn!("Engine WebSocket error: {}", e);
                            break;
                        }
                    };
                    let Ok(container) = Container::decode(data.as_ref()) else { continue };

                    match container.payload {
                        Some(Payload::PromptResponse(resp)) => {
                            // Forward operator answers to the awaiting prompt.
                            let _ = prompt_tx_task.send(resp);
                        }
                        Some(Payload::AuthVerify(_)) => {
                            // Answer the engine's liveness probe with our auth token
                            // so a quiet period never severs us (this module does
                            // nothing during dead air, so it would otherwise be
                            // flagged unresponsive and killed on a schedule).
                            let reply = Container {
                                version: 1,
                                auth_token: auth_token.clone(),
                                module_name: module_name.clone(),
                                module_instance_uuid7: instance_uuid.clone(),
                                payload: Some(Payload::AuthVerify(AuthVerify {
                                    cur_auth: auth_token.clone(),
                                })),
                            };
                            send_container(&write_shared, reply).await;
                        }
                        Some(Payload::MessagePreProcess(pre)) => {
                            let MessagePreProcess {
                                message_uuid7: uuid,
                                raw_message,
                                audio,
                                audio_type,
                            } = pre;
                            // ACK on EVERY path: the engine advances the
                            // pre-process stage only when every tracked
                            // recipient replies with the same message_uuid7.
                            // Echo the raw ChatMessage back whether we scored
                            // it or not (delta==0, empty uuid, or error).
                            let Some(chat) = &raw_message else {
                                send_container(
                                    &write_shared,
                                    ack_preprocess_container(
                                        &auth_token,
                                        &module_name,
                                        &instance_uuid,
                                        uuid,
                                        None,
                                        audio,
                                        audio_type,
                                    ),
                                )
                                .await;
                                continue;
                            };
                            let config = config_shared.lock().unwrap().clone();
                            let text = chat.raw_message.clone();
                            let user_id = chat.user_uuid7.clone();

                            let (mut delta, mut notes) = score_message(&text, &config);

                            // Frequency: punish posting again too soon after the last message.
                            if config.frequency.toggle && !user_id.is_empty() {
                                let now = Instant::now();
                                let interval = Duration::from_secs(
                                    config
                                        .frequency
                                        .interval_secs
                                        .max(config.frequency_interval_floor_secs),
                                );
                                if let Some(prev) = last_msg.get(&user_id) {
                                    if now.duration_since(*prev) < interval {
                                        delta += config.frequency.score;
                                        notes.push("frequency".into());
                                    }
                                }
                                last_msg.insert(user_id.clone(), now);
                                // Bound the frequency map: evict entries older
                                // than the interval, then oldest-first if the
                                // map still exceeds the size cap.
                                last_msg.retain(|_, t| now.duration_since(*t) < interval);
                                if last_msg.len() >= config.last_msg_cap {
                                    if let Some(evict) = last_msg
                                        .iter()
                                        .min_by_key(|(_, t)| **t)
                                        .map(|(k, _)| k.clone())
                                    {
                                        last_msg.remove(&evict);
                                    }
                                }
                            }

                            if delta != 0 && !uuid.is_empty() {
                                // Apply the real signed delta via the engine's
                                // score-only virtual query (no ±1 clamp, no
                                // rating-counter inflation, no cooldown).
                                let query_id = "userdb_adjust_score";
                                let payload = rating_payload(&user_id, chat, delta, &notes);
                                let query = Container {
                                    version: 1,
                                    auth_token: auth_token.clone(),
                                    module_name: module_name.clone(),
                                    module_instance_uuid7: instance_uuid.clone(),
                                    payload: Some(Payload::DatabaseQuery(DatabaseQuery {
                                        query_id: query_id.to_string(),
                                        sql: payload.to_string(),
                                        params: vec![],
                                    })),
                                };
                                send_container(&write_shared, query).await;
                                warn!(
                                    "[score] uuid={} delta={} notes={:?} ({} user={})",
                                    uuid, delta, notes, query_id, user_id
                                );
                            }

                            // Always ack so the engine advances the stage.
                            send_container(
                                &write_shared,
                                ack_preprocess_container(
                                    &auth_token,
                                    &module_name,
                                    &instance_uuid,
                                    uuid,
                                    Some(chat.clone()),
                                    audio,
                                    audio_type,
                                ),
                            )
                            .await;
                        }
                        Some(Payload::MessageInProcess(process)) => {
                            // Pass-through ack of the in-process stage so it
                            // never stalls (even though this module only
                            // declares pre-process capability).
                            let ack = Container {
                                version: 1,
                                auth_token: auth_token.clone(),
                                module_name: module_name.clone(),
                                module_instance_uuid7: instance_uuid.clone(),
                                payload: Some(Payload::MessageInProcess(MessageInProcess {
                                    message_uuid7: process.message_uuid7,
                                    raw_message: process.raw_message,
                                    processed_message: process.processed_message,
                                    abandon_message: process.abandon_message,
                                    audio: process.audio,
                                    audio_type: process.audio_type,
                                })),
                            };
                            send_container(&write_shared, ack).await;
                        }
                        _ => {}
                    }
                }

                // The engine connection dropped — reconnect with backoff instead
                // of leaving the module unresponsive.
                info!("Engine disconnected — reconnecting...");
                let reconnect_cfg = config_shared.lock().unwrap().clone();
                let mut backoff = reconnect_cfg.reconnect_base_secs;
                loop {
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    match CockatielClient::connect("config.json").await {
                        Ok(conn) => {
                            info!("Reconnected to engine");
                            let (w, r) = conn.stream.split();
                            *write_shared.lock().await = w;
                            auth_token = conn.auth_token;
                            instance_uuid = conn.instance_uuid7;
                            module_name = conn.config.module_name;
                            read = r;
                            continue 'reconnect;
                        }
                        Err(e) => {
                            warn!("Engine reconnect failed: {} — retrying in {}s", e, backoff);
                            backoff = (backoff * 2).min(reconnect_cfg.reconnect_max_secs);
                        }
                    }
                }
            }
        });
    }

    let config = load_config(
        &write_shared,
        &mut prompt_rx,
        &auth_token,
        &module_name,
        &instance_uuid,
    )
    .await;
    *config_shared.lock().unwrap() = config.clone();
    info!("Score-messages module active");

    // Keep the process alive; the read task does all the work.
    loop {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chat(user_id: &str) -> ChatMessage {
        ChatMessage {
            platform: "twitch".to_string(),
            raw_data: Vec::new(),
            raw_message: "hello".to_string(),
            user_uuid7: user_id.to_string(),
            command: None,
            user_data: None,
            channel_id: String::new(),
        }
    }

    #[test]
    fn rating_payload_uses_uuid7_when_shape_matches() {
        let uuid = "0189a0c1-1111-4222-8333-444455556666";
        let chat = sample_chat(uuid);
        let p = rating_payload(uuid, &chat, -5, &["spam".to_string()]);
        assert!(p.get("uuid7").is_some());
        assert!(p.get("platform").is_none());
        assert_eq!(p["delta"], -5);
        assert_eq!(p["reason"], "score-messages: spam");
    }

    #[test]
    fn rating_payload_falls_back_to_platform_handle() {
        let chat = sample_chat("some_chatter");
        let p = rating_payload("some_chatter", &chat, 3, &[]);
        assert!(p.get("uuid7").is_none());
        assert_eq!(p["platform"], "twitch");
        assert_eq!(p["handle"], "some_chatter");
        assert_eq!(p["delta"], 3);
    }
}
