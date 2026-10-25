use futures_util::{SinkExt, StreamExt};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, info, warn};
use tracing_subscriber::FmtSubscriber;

use cockatiel_client::proto::container_for_engine::Payload as EnginePayload;
use cockatiel_client::proto::container_for_module::Payload as ModulePayload;
use cockatiel_client::proto::*;
use cockatiel_client::{CockatielClient, PromptKind};

use score_messages_rs::{
    apply_flat_overlay, apply_jitter, clamp_delta, default_config, flat_map_for_write,
    jitter_multiplier, score_message, viewership_multiplier, Config,
};

type WsWriteHalf = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

const BANS_COMMAND: &str = "score";
const DEFAULT_FLAG: &str = "!";
const DEFAULT_DATA_DIR: &str = "~/.cockatiel/score-messages";
const MODULE_BANS_QUERY: &str = "module_bans_query";
const MODULE_BANS_REPORT: &str = "module_bans_report";
const DM_DB: &str = "_dm";

/// Expand a leading `~` to `$HOME` (module config paths are written by hand).
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    } else if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    }
    path.to_string()
}

/// Config for the per-module ban list, read from `module_specific` (data_dir /
/// bans_path) and backfilled on first run.
struct BansSettings {
    command_flag: String,
    bans_path: String,
}

fn load_bans_settings() -> BansSettings {
    let root: Option<serde_json::Value> = std::fs::read_to_string("config.json")
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok());
    let ms = root
        .as_ref()
        .and_then(|r| r.get("module_specific"))
        .and_then(|m| m.as_object())
        .cloned()
        .unwrap_or_default();
    let command_flag = ms
        .get("command_flag")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_FLAG)
        .to_string();
    let data_dir = ms
        .get("data_dir")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_DATA_DIR)
        .to_string();
    let bans_path = ms
        .get("bans_path")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}/bans.json", data_dir.trim_end_matches('/')));
    if let Some(mut root) = root {
        if let Some(obj) = root.as_object_mut() {
            if let Some(ms) = obj
                .entry("module_specific".to_string())
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
            {
                ms.entry("command_flag".to_string())
                    .or_insert_with(|| serde_json::json!(command_flag));
                ms.entry("data_dir".to_string())
                    .or_insert_with(|| serde_json::json!(data_dir));
                ms.entry("bans_path".to_string())
                    .or_insert_with(|| serde_json::json!(bans_path));
            }
            let _ = std::fs::write("config.json", serde_json::to_string_pretty(&root).unwrap());
        }
    }
    BansSettings {
        command_flag,
        bans_path,
    }
}

fn commands_registration(flag: &str) -> Commands {
    Commands {
        commands: vec![Command {
            command_name: BANS_COMMAND.to_string(),
            command_flag: flag.to_string(),
            command_description: "manage score-messages bans".to_string(),
            command_flags: vec![],
        }],
        alert_on_unknown_command: true,
    }
}

/// Send a Prompt to the engine (forwarded to connected UIs) and wait for the
/// operator's response (`PromptResponse.reason`). Returns None on cancel/timeout.
#[allow(clippy::too_many_arguments)]
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
    let container = ContainerForEngine {
        version: 2,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(EnginePayload::Prompt(prompt)),
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
    let mut flat = flat_map_for_write(cfg, &root, &ms);
    // Keep the ban-file settings (not part of the scoring Config) alongside the
    // scoring knobs instead of dropping them on every config rewrite.
    for key in ["command_flag", "data_dir", "bans_path"] {
        if let Some(v) = ms.get(key) {
            flat.insert(key.to_string(), v.clone());
        }
    }
    root["module_specific"] = serde_json::Value::Object(flat);
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
async fn send_container(
    write_shared: &Arc<AsyncMutex<WsWriteHalf>>,
    container: ContainerForEngine,
) {
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
) -> ContainerForEngine {
    ContainerForEngine {
        version: 2,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(EnginePayload::MessagePreProcess(MessagePreProcess {
            message_uuid7,
            raw_message,
            audio,
            audio_type,
        })),
    }
}

// ── bans.json ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BanEntry {
    user: String,
    key: String,
    by: String,
    reason: String,
    at: i64,
}

type BanMap = HashMap<String, HashMap<String, BanEntry>>;

fn load_bans(path: &str) -> BanMap {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|d| serde_json::from_str::<BanMap>(&d).ok())
        .unwrap_or_default()
}

fn save_bans(path: &str, map: &BanMap) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn normalize_ban_user(tok: &str) -> String {
    let t = tok.trim();
    if let Some(inner) = t.strip_prefix("<@") {
        let inner = inner.trim_end_matches('>');
        let inner = inner.trim_start_matches(['!', '&']);
        return inner.trim().to_string();
    }
    t.trim_start_matches('@').trim().to_string()
}

fn all_ban_ids(map: &BanMap) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for guild in map.values() {
        for entry in guild.values() {
            let id = entry.user.trim();
            if !id.is_empty() && seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        }
    }
    out
}

fn is_banned_in_guild(map: &BanMap, guild: &str, candidates: &[&str]) -> bool {
    let Some(banned) = map.get(guild) else {
        return false;
    };
    candidates.iter().any(|c| {
        let c = c.trim();
        !c.is_empty() && banned.values().any(|b| b.user == c)
    })
}

fn guild_key(chat: &ChatMessage) -> String {
    let guild = serde_json::from_slice::<serde_json::Value>(&chat.raw_data)
        .ok()
        .and_then(|v| v.get("guild_id").and_then(|x| x.as_str()).map(str::to_string))
        .unwrap_or_default();
    if guild.is_empty() {
        DM_DB.to_string()
    } else {
        guild
    }
}

fn discord_author(raw_data: &[u8]) -> (String, String) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(raw_data) else {
        return (String::new(), String::new());
    };
    let author = v.get("author");
    let id = author
        .and_then(|a| a.get("id"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let username = author
        .and_then(|a| a.get("username"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    (id, username)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// mtime-cached ban file: a hand edit is picked up on the next message.
struct BanStore {
    path: String,
    cache: Option<BanMap>,
    mtime: Option<std::time::SystemTime>,
}

impl BanStore {
    fn new(path: &str) -> Self {
        Self {
            path: expand_tilde(path),
            cache: None,
            mtime: None,
        }
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn bans(&mut self) -> &BanMap {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        if self.cache.is_none() || mtime != self.mtime {
            self.cache = Some(load_bans(&self.path));
            self.mtime = mtime;
        }
        self.cache.as_ref().unwrap()
    }

    fn invalidate(&mut self) {
        self.cache = None;
        self.mtime = None;
    }
}

fn bans_report_container(auth: &str, module: &str, instance: &str, map: &BanMap) -> ContainerForEngine {
    let json = serde_json::json!({ "bans": all_ban_ids(map) }).to_string();
    ContainerForEngine {
        version: 2,
        auth_token: auth.to_string(),
        module_name: module.to_string(),
        module_instance_uuid7: instance.to_string(),
        payload: Some(EnginePayload::DatabaseQuery(DatabaseQuery {
            query_id: MODULE_BANS_REPORT.to_string(),
            sql: json,
            params: Vec::new(),
        })),
    }
}

fn ban_actor(chat: &ChatMessage) -> (bool, String) {
    let Some(ud) = chat.user_data.as_ref() else {
        return (false, chat.user_uuid7.clone());
    };
    let allowed = ud.is_moderator || ud.is_admin || ud.is_owner;
    let actor = if !ud.uuid.is_empty() {
        ud.uuid.clone()
    } else if !ud.username.is_empty() {
        ud.username.clone()
    } else {
        chat.user_uuid7.clone()
    };
    (allowed, actor)
}

fn reply_container(auth: &str, module: &str, instance: &str, chat: &ChatMessage, msg: String) -> ContainerForEngine {
    let handle = chat
        .user_data
        .as_ref()
        .map(|u| u.username.clone())
        .unwrap_or_default();
    ContainerForEngine {
        version: 2,
        auth_token: auth.to_string(),
        module_name: module.to_string(),
        module_instance_uuid7: instance.to_string(),
        payload: Some(EnginePayload::SendToPlatforms(SendToPlatforms {
            msg,
            level: 0,
            module_uuid7: instance.to_string(),
            pid: String::new(),
            platform: chat.platform.clone(),
            actor_platform: chat.platform.clone(),
            actor_handle: handle,
            actor_uuid7: chat.user_uuid7.clone(),
            channel_id: chat.channel_id.clone(),
        })),
    }
}

#[derive(Debug, PartialEq)]
enum BanCmd {
    Ban { user: String, reason: String },
    Unban(String),
}

fn parse_ban_subcommand(args: &str) -> Option<BanCmd> {
    let args = args.trim();
    let (head, rest) = match args.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (args, ""),
    };
    match head.to_ascii_lowercase().as_str() {
        "ban" => {
            let (user, reason) = match rest.split_once(char::is_whitespace) {
                Some((u, r)) => (u, r.trim()),
                None => (rest, ""),
            };
            (!user.is_empty()).then(|| BanCmd::Ban {
                user: user.to_string(),
                reason: reason.to_string(),
            })
        }
        "unban" => {
            let target = rest.split_whitespace().next().unwrap_or("");
            (!target.is_empty()).then(|| BanCmd::Unban(target.to_string()))
        }
        _ => None,
    }
}

enum BanOutcome {
    Reply(String),
    Report(String, BanMap),
}

fn handle_bans_command(
    chat: &ChatMessage,
    args: &str,
    store: &mut BanStore,
    label: &str,
) -> Option<BanOutcome> {
    let cmd = parse_ban_subcommand(args)?;
    let guild = guild_key(chat);
    let (allowed, actor) = ban_actor(chat);
    if !allowed {
        return Some(BanOutcome::Reply(format!(
            "Only moderators can manage {label} bans."
        )));
    }
    let mut map = load_bans(store.path());
    let outcome = match cmd {
        BanCmd::Ban { user, reason } => {
            let target = normalize_ban_user(&user);
            if target.is_empty() {
                return Some(BanOutcome::Reply(
                    "Usage: `ban <user> [reason]`.".to_string(),
                ));
            }
            let entry = BanEntry {
                user: target.clone(),
                key: target.clone(),
                by: actor,
                reason: reason.clone(),
                at: now_ms(),
            };
            map.entry(guild.clone()).or_default().insert(target.clone(), entry);
            match save_bans(store.path(), &map) {
                Ok(()) => {
                    store.invalidate();
                    if reason.is_empty() {
                        BanOutcome::Report(format!("Banned `{target}` from {label}."), map)
                    } else {
                        BanOutcome::Report(
                            format!("Banned `{target}` from {label}: {reason}"),
                            map,
                        )
                    }
                }
                Err(e) => BanOutcome::Reply(format!("Could not save the ban: {e}")),
            }
        }
        BanCmd::Unban(user) => {
            let target = normalize_ban_user(&user);
            let removed = map
                .get_mut(&guild)
                .and_then(|g| g.remove(&target))
                .is_some();
            if removed && map.get(&guild).map(|g| g.is_empty()).unwrap_or(false) {
                map.remove(&guild);
            }
            match save_bans(store.path(), &map) {
                Ok(()) => {
                    store.invalidate();
                    if removed {
                        BanOutcome::Report(format!("Unbanned `{target}` from {label}."), map)
                    } else {
                        BanOutcome::Reply(format!("`{target}` was not banned."))
                    }
                }
                Err(e) => BanOutcome::Reply(format!("Could not save the ban list: {e}")),
            }
        }
    };
    Some(outcome)
}



/// Strip the `<flag><command>` prefix from a routed command message.
fn strip_command(raw: &str, flag: &str, name: &str) -> String {
    let mut rest = raw.trim().to_string();
    if let Some(idx) = rest.find(flag) {
        rest = rest[idx + flag.len()..].trim_start().to_string();
    }
    if rest.starts_with(name) {
        rest = rest[name.len()..].trim_start().to_string();
    }
    rest
}

/// Build the in-process ACK for a message: pass the stage through unchanged so
/// it never stalls, echoing the (post-censorship) processed text verbatim —
/// scoring does not modify the message.
fn in_process_ack_container(
    auth_token: &str,
    module_name: &str,
    instance_uuid: &str,
    process: MessageInProcess,
) -> ContainerForEngine {
    ContainerForEngine {
        version: 2,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(EnginePayload::MessageInProcess(process)),
    }
}

/// Build the adjust-score payload for a scored user. A uuid7-shaped id targets
/// the user DB row directly; anything else (platform handle) falls back to the
/// platform+handle pair. The engine's `userdb_adjust_score` applies the REAL
/// signed delta to `score` without touching the human-rating counters.
fn rating_payload(
    user_id: &str,
    chat: &ChatMessage,
    delta: i32,
    notes: &[String],
) -> serde_json::Value {
    let reason = format!("score-messages: {}", notes.join(","));
    let uuid7_shaped = user_id.chars().filter(|c| *c == '-').count() == 4 && user_id.len() == 36;
    if uuid7_shaped {
        serde_json::json!({ "uuid7": user_id, "delta": delta, "reason": reason })
    } else {
        serde_json::json!({ "platform": chat.platform, "handle": user_id, "delta": delta, "reason": reason })
    }
}

/// A tiny xorshift64* PRNG — no extra dependency. Seeded from the clock and the
/// process id at construction so each run (and each reconnect) differs.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let seed = nanos ^ (std::process::id() as u64).wrapping_mul(0x2545_F491_4F6C_DD1D);
        Rng(seed | 1)
    }

    #[allow(dead_code)]
    fn from_seed(seed: u64) -> Self {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A uniform `f32` in `[-1.0, 1.0)`.
    fn signed_unit(&mut self) -> f32 {
        let v = (self.next_u64() >> 40) as f32 / (1u32 << 24) as f32;
        v * 2.0 - 1.0
    }
}

/// Score one in-process message and, when the resulting delta is non-zero,
/// build the engine's `userdb_adjust_score` DatabaseQuery. The text scored is
/// the POST-CENSORSHIP `processed_message` when present, so banned-words and
/// language-constrainer rewrites punish the bad message further; it falls back
/// to the raw message when there is no processed text. Returns None when there
/// is nothing to adjust (no raw message, delta == 0, or an empty uuid).
#[allow(clippy::too_many_arguments)]
fn in_process_adjust(
    auth_token: &str,
    module_name: &str,
    instance_uuid: &str,
    process: &MessageInProcess,
    config: &Config,
    viewers: i32,
    last_msg: &mut HashMap<String, Instant>,
    roll: f32,
) -> Option<ContainerForEngine> {
    let chat = process.raw_message.as_ref()?;
    let text = if !process.processed_message.is_empty() {
        process.processed_message.clone()
    } else {
        chat.raw_message.clone()
    };
    let user_id = chat.user_uuid7.clone();

    let (mut delta, mut notes) = score_message(&text, config);

    // Frequency: punish posting again too soon after the last message.
    if config.frequency.toggle && !user_id.is_empty() {
        let now = Instant::now();
        let interval = Duration::from_secs(
            config
                .frequency
                .interval_secs
                .max(config.frequency_interval_floor_secs) as u64,
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
        if last_msg.len() >= config.last_msg_cap as usize {
            if let Some(evict) = last_msg
                .iter()
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone())
            {
                last_msg.remove(&evict);
            }
        }
    }

    if delta == 0 || process.message_uuid7.is_empty() {
        return None;
    }

    // Apply the viewership multiplier: small streams
    // reward interaction more (peak viewers → a higher
    // factor up to the configured ceiling). Disabled
    // when viewership_toggle is off.
    if config.viewership_toggle {
        let mult = viewership_multiplier(config, viewers);
        delta = ((delta as f32) * mult).round() as i32;
        if delta != 0 {
            notes.push(format!("viewership x{mult:.2}"));
        }
    }

    // Score jitter: randomly scale the EARNED points so farming is harder to
    // predict. Penalties are never jittered. Applied before the clamp so the
    // clamp stays the hard bound.
    if config.jitter_toggle {
        let before = delta;
        delta = apply_jitter(delta, config.jitter_percent, roll);
        if delta != before {
            notes.push(format!(
                "jitter x{:.2}",
                jitter_multiplier(config.jitter_percent, roll)
            ));
        }
    }

    // Clamp the earned delta to the configured range, if enabled.
    delta = clamp_delta(config, delta);

    // Clamping can collapse a delta to zero (e.g. a penalty hitting the clamp
    // floor): that's a no-op, so skip the guaranteed zero-delta DB round-trip.
    if delta == 0 {
        return None;
    }

    // Apply the real signed delta via the engine's
    // score-only virtual query (no ±1 clamp, no
    // rating-counter inflation, no cooldown).
    let query_id = "userdb_adjust_score";
    let payload = rating_payload(&user_id, chat, delta, &notes);
    let query = ContainerForEngine {
        version: 2,
        auth_token: auth_token.to_string(),
        module_name: module_name.to_string(),
        module_instance_uuid7: instance_uuid.to_string(),
        payload: Some(EnginePayload::DatabaseQuery(DatabaseQuery {
            query_id: query_id.to_string(),
            sql: payload.to_string(),
            params: vec![],
        })),
    };
    // Per-message trace: `debug` (dropped at the module's INFO level) so a
    // scored message is not a synchronous write on the hot path.
    debug!(
        "[score] uuid={} delta={} notes={:?} ({} user={})",
        process.message_uuid7, delta, notes, query_id, user_id
    );
    Some(query)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Non-blocking stderr: logs go through a background writer thread with a
    // bounded buffer (dropping on overflow) instead of a synchronous write from
    // the calling task. A synchronous `warn!` on the message hot path could
    // block the read loop when the stderr pipe back-pressures (e.g. while the
    // supervisor/TUI log reader catches up at startup), stalling the module's
    // acks. The guard must live for the whole process, so it is held here.
    let (non_blocking, _log_guard) = tracing_appender::non_blocking(std::io::stderr());
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(non_blocking)
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

    // Cached peak viewer count across platforms (fed by the read loop's
    // viewership poll, used to scale score deltas by the viewership multiplier).
    let viewers_shared: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));

    // Read task: forward PromptResponses to the awaiting prompt AND handle
    // message pre-processing. Spawned BEFORE load_config so prompts work.
    // Owns the read half + session identity so it can reconnect with backoff
    // when the engine drops the socket (instead of dying and leaving main to
    // sleep forever while the watchdog severs the unresponsive module).
    {
        let prompt_tx_task = prompt_tx.clone();
        let config_shared = Arc::clone(&config_shared);
        let viewers_shared = Arc::clone(&viewers_shared);
        let write_shared = Arc::clone(&write_shared);
        let mut auth_token = auth_token.clone();
        let mut module_name = module_name.clone();
        let mut instance_uuid = instance_uuid.clone();
        let mut read = read;
        tokio::spawn(async move {
            let bans_settings = load_bans_settings();
            let mut bans = BanStore::new(&bans_settings.bans_path);
            let mut last_msg: HashMap<String, Instant> = HashMap::new();
            let mut rng = Rng::new();
            let mut poll_secs = config_shared.lock().unwrap().viewership_poll_secs.max(1);
            let mut next_poll = tokio::time::Instant::now() + Duration::from_secs(poll_secs as u64);
            'reconnect: loop {
                // Register our minimal command and announce current bans so a
                // reconnect's fresh session learns both.
                send_container(
                    &write_shared,
                    ContainerForEngine {
                        version: 2,
                        auth_token: auth_token.clone(),
                        module_name: module_name.clone(),
                        module_instance_uuid7: instance_uuid.clone(),
                        payload: Some(EnginePayload::Commands(commands_registration(
                            &bans_settings.command_flag,
                        ))),
                    },
                )
                .await;
                {
                    let report = {
                        let map = bans.bans();
                        bans_report_container(&auth_token, &module_name, &instance_uuid, map)
                    };
                    send_container(&write_shared, report).await;
                }
                loop {
                    tokio::select! {
                        msg = read.next() => {
                            let Some(msg) = msg else { break };
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
                            let Ok(container) = ContainerForModule::decode(data.as_ref()) else { continue };

                            match container.payload {
                                Some(ModulePayload::PromptResponse(resp)) => {
                                    // Forward operator answers to the awaiting prompt.
                                    let _ = prompt_tx_task.send(resp);
                                }
                                Some(ModulePayload::AuthVerify(_)) => {
                                    // Answer the engine's liveness probe with our auth token
                                    // so a quiet period never severs us (this module does
                                    // nothing during dead air, so it would otherwise be
                                    // flagged unresponsive and killed on a schedule).
                                    let reply = ContainerForEngine {
                                        version: 2,
                                        auth_token: auth_token.clone(),
                                        module_name: module_name.clone(),
                                        module_instance_uuid7: instance_uuid.clone(),
                                        payload: Some(EnginePayload::AuthVerify(AuthVerify {
                                            cur_auth: auth_token.clone(),
                                        })),
                                    };
                                    send_container(&write_shared, reply).await;
                                }
                                Some(ModulePayload::MessagePreProcess(pre)) => {
                                    let MessagePreProcess {
                                        message_uuid7: uuid,
                                        raw_message,
                                        audio,
                                        audio_type,
                                    } = pre;
                                    if !uuid.is_empty() {
                                        let receipt = ContainerForEngine {
                                            version: 2,
                                            auth_token: auth_token.clone(),
                                            module_name: module_name.clone(),
                                            module_instance_uuid7: instance_uuid.clone(),
                                            payload: Some(EnginePayload::MessageAck(MessageAck {
                                                message_uuid7: uuid.clone(),
                                            })),
                                        };
                                        send_container(&write_shared, receipt).await;
                                    }
                                    // Pass-through ack: the engine advances the
                                    // pre-process stage only when every tracked
                                    // recipient replies with the same message_uuid7.
                                    // This module scores in-process, so echo the raw
                                    // ChatMessage back unchanged.
                                    send_container(
                                        &write_shared,
                                        ack_preprocess_container(
                                            &auth_token,
                                            &module_name,
                                            &instance_uuid,
                                            uuid,
                                            raw_message,
                                            audio,
                                            audio_type,
                                        ),
                                    )
                                    .await;
                                }
                                Some(ModulePayload::MessageInProcess(process)) => {
                                    if !process.message_uuid7.is_empty() {
                                        let receipt = ContainerForEngine {
                                            version: 2,
                                            auth_token: auth_token.clone(),
                                            module_name: module_name.clone(),
                                            module_instance_uuid7: instance_uuid.clone(),
                                            payload: Some(EnginePayload::MessageAck(MessageAck {
                                                message_uuid7: process.message_uuid7.clone(),
                                            })),
                                        };
                                        send_container(&write_shared, receipt).await;
                                    }
                                    if process.raw_message.is_none() {
                                        // No raw ChatMessage: pass through unchanged.
                                        send_container(
                                            &write_shared,
                                            in_process_ack_container(
                                                &auth_token,
                                                &module_name,
                                                &instance_uuid,
                                                process,
                                            ),
                                        )
                                        .await;
                                        continue;
                                    }
                                    {
                                        let chat = process.raw_message.as_ref().unwrap();
                                        let guild = guild_key(chat);
                                        let (author_id, author_name) = discord_author(&chat.raw_data);
                                        let locally_banned = is_banned_in_guild(
                                            bans.bans(),
                                            &guild,
                                            &[chat.user_uuid7.as_str(), author_id.as_str(), author_name.as_str()],
                                        );
                                        if locally_banned {
                                            // Local enforcement: don't score banned senders.
                                            send_container(
                                                &write_shared,
                                                in_process_ack_container(
                                                    &auth_token,
                                                    &module_name,
                                                    &instance_uuid,
                                                    process,
                                                ),
                                            )
                                            .await;
                                            continue;
                                        }
                                        if let Some(cmd) = &chat.command {
                                            if cmd.command_name == BANS_COMMAND {
                                                let args = strip_command(
                                                    &chat.raw_message,
                                                    &cmd.command_flag,
                                                    &cmd.command_name,
                                                );
                                                if let Some(outcome) = handle_bans_command(
                                                    chat, &args, &mut bans, "score-messages",
                                                ) {
                                                    match outcome {
                                                        BanOutcome::Reply(msg) => {
                                                            send_container(
                                                                &write_shared,
                                                                reply_container(
                                                                    &auth_token, &module_name,
                                                                    &instance_uuid, chat, msg,
                                                                ),
                                                            )
                                                            .await;
                                                        }
                                                        BanOutcome::Report(msg, map) => {
                                                            send_container(
                                                                &write_shared,
                                                                reply_container(
                                                                    &auth_token, &module_name,
                                                                    &instance_uuid, chat, msg,
                                                                ),
                                                            )
                                                            .await;
                                                            send_container(
                                                                &write_shared,
                                                                bans_report_container(
                                                                    &auth_token, &module_name,
                                                                    &instance_uuid, &map,
                                                                ),
                                                            )
                                                            .await;
                                                        }
                                                    }
                                                    send_container(
                                                        &write_shared,
                                                        in_process_ack_container(
                                                            &auth_token,
                                                            &module_name,
                                                            &instance_uuid,
                                                            process,
                                                        ),
                                                    )
                                                    .await;
                                                    continue;
                                                }
                                            }
                                        }
                                    }
                                    // Score the POST-CENSORSHIP text so censorship
                                    // patterns (banned-words / language-constrainer)
                                    // punish bad messages further.
                                    let config = config_shared.lock().unwrap().clone();
                                    let viewers = *viewers_shared.lock().unwrap();
                                    let roll = rng.signed_unit();
                                    if let Some(query) = in_process_adjust(
                                        &auth_token,
                                        &module_name,
                                        &instance_uuid,
                                        &process,
                                        &config,
                                        viewers,
                                        &mut last_msg,
                                        roll,
                                    ) {
                                        send_container(&write_shared, query).await;
                                    }
                                    // Pass-through ack of the in-process stage so it
                                    // never stalls, echoing the processed message
                                    // unchanged (scoring does not modify text).
                                    send_container(
                                        &write_shared,
                                        in_process_ack_container(
                                            &auth_token,
                                            &module_name,
                                            &instance_uuid,
                                            process,
                                        ),
                                    )
                                    .await;
                                }
                                Some(ModulePayload::DatabaseQueryResult(res))
                                    if res.query_id == MODULE_BANS_QUERY =>
                                {
                                    let report = {
                                        let map = bans.bans();
                                        bans_report_container(
                                            &auth_token, &module_name, &instance_uuid, map,
                                        )
                                    };
                                    send_container(&write_shared, report).await;
                                }
                                Some(ModulePayload::DatabaseQueryResult(res))
                                    if res.query_id == "channel_viewers" =>
                                {
                                    // Peak viewer count across all channels.
                                    let mut peak = 0i32;
                                    if let Ok(json) =
                                        serde_json::from_slice::<serde_json::Value>(&res.result_blob)
                                    {
                                        if let Some(channels) =
                                            json.get("channels").and_then(|c| c.as_array())
                                        {
                                            for ch in channels {
                                                if let Some(v) = ch.get("viewers").and_then(|v| v.as_i64()) {
                                                    peak = peak.max(v as i32);
                                                }
                                            }
                                        }
                                    }
                                    *viewers_shared.lock().unwrap() = peak;
                                }
                                Some(ModulePayload::Shutdown(_)) => {
                                    info!("Engine requested shutdown — exiting");
                                    std::process::exit(0);
                                }
                                _ => {}
                            }
                        }
                        _ = tokio::time::sleep_until(next_poll) => {
                            let config = config_shared.lock().unwrap().clone();
                            poll_secs = config.viewership_poll_secs.max(1);
                            next_poll = tokio::time::Instant::now()
                                + Duration::from_secs(poll_secs as u64);
                            if config.viewership_toggle {
                                let query = ContainerForEngine {
                                    version: 2,
                                    auth_token: auth_token.clone(),
                                    module_name: module_name.clone(),
                                    module_instance_uuid7: instance_uuid.clone(),
                                    payload: Some(EnginePayload::DatabaseQuery(DatabaseQuery {
                                        query_id: "channel_viewers".to_string(),
                                        sql: "{}".to_string(),
                                        params: vec![],
                                    })),
                                };
                                send_container(&write_shared, query).await;
                            }
                        }
                    }
                }

                // The engine connection dropped — reconnect with backoff instead
                // of leaving the module unresponsive.
                info!("Engine disconnected — reconnecting...");
                let reconnect_cfg = config_shared.lock().unwrap().clone();
                let mut backoff = reconnect_cfg.reconnect_base_secs;
                loop {
                    tokio::time::sleep(Duration::from_secs(backoff as u64)).await;
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

    fn sample_process(message: &str, raw: Option<ChatMessage>) -> MessageInProcess {
        MessageInProcess {
            message_uuid7: "msg-uuid".to_string(),
            raw_message: raw,
            processed_message: message.to_string(),
            abandon_message: false,
            audio: Vec::new(),
            audio_type: String::new(),
        }
    }

    /// A config with every AC check off except the leading-capital rule, so a
    /// capitalised censored message yields a deterministic +30 delta.
    fn capital_only_config() -> Config {
        let mut config = default_config();
        for r in [
            &mut config.punctuation,
            &mut config.trigram,
            &mut config.repeating,
            &mut config.space_ratio,
            &mut config.run_on,
            &mut config.grouping,
            &mut config.emoji,
            &mut config.spam,
        ] {
            r.toggle = false;
        }
        config.capital.toggle = true;
        config.frequency.toggle = false;
        config.base.toggle = false;
        config
    }

    #[test]
    fn in_process_scores_censored_text_and_emits_adjust_query() {
        let chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        let config = capital_only_config();
        let mut last_msg = HashMap::new();
        // The post-censorship text is the one scored ("AAAAAAAA" -> capital
        // reward, +30), not the raw "hello".
        let process = sample_process("AAAAAAAA", Some(chat.clone()));

        let query = in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .expect("non-zero delta must emit an adjust query");

        let Some(EnginePayload::DatabaseQuery(q)) = query.payload else {
            panic!("expected a DatabaseQuery container");
        };
        assert_eq!(q.query_id, "userdb_adjust_score");
        let payload: serde_json::Value = serde_json::from_str(&q.sql).unwrap();
        assert_eq!(payload["uuid7"], chat.user_uuid7);
        assert_eq!(payload["delta"], 30);

        // A pass-through ack is produced with the processed message unchanged.
        let ack = in_process_ack_container("tok", "score-messages", "inst", process);
        let Some(EnginePayload::MessageInProcess(m)) = ack.payload else {
            panic!("expected a MessageInProcess ack");
        };
        assert_eq!(m.message_uuid7, "msg-uuid");
        assert_eq!(m.processed_message, "AAAAAAAA");
        assert!(!m.abandon_message);
    }

    #[test]
    fn in_process_zero_delta_emits_no_query() {
        let chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        let mut config = capital_only_config();
        config.capital.toggle = false;
        let mut last_msg = HashMap::new();
        let process = sample_process("hello there", Some(chat));
        assert!(in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .is_none());
    }

    #[test]
    fn in_process_adjust_ignores_missing_raw_message() {
        let config = capital_only_config();
        let mut last_msg = HashMap::new();
        let process = sample_process("AAAAAAAA", None);
        assert!(in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .is_none());
    }

    #[test]
    fn in_process_falls_back_to_raw_message_when_unprocessed() {
        let mut chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        chat.raw_message = "Hello".to_string();
        let config = capital_only_config();
        let mut last_msg = HashMap::new();
        let process = sample_process("", Some(chat));
        let query = in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .expect("raw fallback should still score");
        let Some(EnginePayload::DatabaseQuery(q)) = query.payload else {
            panic!("expected a DatabaseQuery container");
        };
        let payload: serde_json::Value = serde_json::from_str(&q.sql).unwrap();
        assert_eq!(payload["delta"], 30);
    }

    #[test]
    fn in_process_clamps_final_delta_when_enabled() {
        let chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        let mut config = capital_only_config();
        config.clamp_toggle = true;
        config.clamp_max = 5;
        let mut last_msg = HashMap::new();
        let process = sample_process("AAAAAAAA", Some(chat.clone()));

        let query = in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .expect("clamped delta must still emit an adjust query");
        let Some(EnginePayload::DatabaseQuery(q)) = query.payload else {
            panic!("expected a DatabaseQuery container");
        };
        let payload: serde_json::Value = serde_json::from_str(&q.sql).unwrap();
        assert_eq!(payload["delta"], 5);

        // With clamping off the +30 delta is preserved.
        let mut config = capital_only_config();
        config.clamp_toggle = false;
        config.clamp_max = 5;
        let mut last_msg = HashMap::new();
        let query = in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .expect("unclamped delta must emit an adjust query");
        let Some(EnginePayload::DatabaseQuery(q)) = query.payload else {
            panic!("expected a DatabaseQuery container");
        };
        let payload: serde_json::Value = serde_json::from_str(&q.sql).unwrap();
        assert_eq!(payload["delta"], 30);
    }

    #[test]
    fn in_process_clamped_to_zero_emits_no_query() {
        let chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        let mut config = capital_only_config();
        // A lowercase-leading message scores -capital_penalty; with clamp_min 0
        // it clamps to 0, which must not send a no-op adjust query.
        config.capital_penalty = 50;
        config.clamp_toggle = true;
        config.clamp_min = 0;
        config.clamp_max = 100;
        let mut last_msg = HashMap::new();
        let process = sample_process("hello there", Some(chat));
        assert!(in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            &config,
            0,
            &mut last_msg,
            0.0,
        )
        .is_none());
    }

    fn jittered_delta(config: &Config, roll: f32) -> i32 {
        let chat = sample_chat("0189a0c1-1111-4222-8333-444455556666");
        let mut last_msg = HashMap::new();
        let process = sample_process("AAAAAAAA", Some(chat));
        let query = in_process_adjust(
            "tok",
            "score-messages",
            "inst",
            &process,
            config,
            0,
            &mut last_msg,
            roll,
        )
        .expect("jittered gain must emit an adjust query");
        let Some(EnginePayload::DatabaseQuery(q)) = query.payload else {
            panic!("expected a DatabaseQuery container");
        };
        let payload: serde_json::Value = serde_json::from_str(&q.sql).unwrap();
        payload["delta"].as_i64().unwrap() as i32
    }

    #[test]
    fn in_process_applies_jitter_to_gains_only() {
        // A +30 delta is scaled by the roll: round(30 * 1.3) = 39,
        // round(30 * 0.7) = 21, and roll 0.0 leaves it unchanged.
        let mut config = capital_only_config();
        config.jitter_toggle = true;
        config.jitter_percent = 30;
        assert_eq!(jittered_delta(&config, 1.0), 39);
        assert_eq!(jittered_delta(&config, -1.0), 21);
        assert_eq!(jittered_delta(&config, 0.0), 30);

        // The clamp remains the hard bound: jitter up (+30 → 39) still clamps.
        let mut config = capital_only_config();
        config.jitter_toggle = true;
        config.jitter_percent = 30;
        config.clamp_toggle = true;
        config.clamp_max = 5;
        assert_eq!(jittered_delta(&config, 1.0), 5);
    }

    #[test]
    fn rng_signed_unit_is_bounded_and_seed_is_deterministic() {
        let mut rng = Rng::new();
        for _ in 0..10_000 {
            let v = rng.signed_unit();
            assert!((-1.0..1.0).contains(&v), "signed_unit out of range: {v}");
        }
        let mut a = Rng::from_seed(0x1234_5678_9ABC_DEF0);
        let mut b = Rng::from_seed(0x1234_5678_9ABC_DEF0);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    fn mod_chat(text: &str, guild: &str) -> ChatMessage {
        ChatMessage {
            platform: "discord".into(),
            raw_data: format!(r#"{{"guild_id":"{guild}"}}"#).into_bytes(),
            raw_message: text.into(),
            user_uuid7: "mod".into(),
            command: None,
            user_data: Some(UserData {
                uuid: "mod-uuid".into(),
                username: "mod".into(),
                is_moderator: true,
                ..Default::default()
            }),
            channel_id: "c".into(),
        }
    }

    #[test]
    fn commands_registration_owns_score_but_keeps_plain_messages() {
        let c = commands_registration("!");
        assert_eq!(c.commands.len(), 1);
        assert_eq!(c.commands[0].command_name, "score");
        assert!(c.alert_on_unknown_command);
    }

    #[test]
    fn parses_ban_and_unban_subcommands() {
        assert_eq!(
            parse_ban_subcommand("ban @bob spam"),
            Some(BanCmd::Ban {
                user: "@bob".into(),
                reason: "spam".into()
            })
        );
        assert_eq!(parse_ban_subcommand("unban bob"), Some(BanCmd::Unban("bob".into())));
        assert_eq!(parse_ban_subcommand("ban"), None);
        assert_eq!(parse_ban_subcommand("unban"), None);
        assert_eq!(normalize_ban_user("@bob"), "bob");
        assert_eq!(normalize_ban_user("<@!123>"), "123");
    }

    #[test]
    fn ban_command_persists_and_enforces_locally() {
        let dir = std::env::temp_dir().join(format!("cockatiel-score-{}", uuid::Uuid::now_v7()));
        let path = dir.join("bans.json");
        let mut store = BanStore::new(path.to_string_lossy().as_ref());
        let chat = mod_chat("!score ban @badguy spam", "guild1");
        match handle_bans_command(&chat, "ban @badguy spam", &mut store, "score-messages").unwrap() {
            BanOutcome::Report(msg, map) => {
                assert!(msg.contains("Banned `badguy`"));
                assert_eq!(all_ban_ids(&map), vec!["badguy".to_string()]);
            }
            BanOutcome::Reply(m) => panic!("unexpected reply: {m}"),
        }
        let map = load_bans(store.path());
        assert_eq!(map["guild1"]["badguy"].by, "mod-uuid");
        assert!(is_banned_in_guild(store.bans(), "guild1", &["badguy"]));
        assert!(!is_banned_in_guild(store.bans(), "guild2", &["badguy"]));

        let mut norm = mod_chat("!score ban @other", "guild1");
        norm.user_data = Some(UserData {
            uuid: "u".into(),
            username: "someone".into(),
            ..Default::default()
        });
        match handle_bans_command(&norm, "ban @other", &mut store, "score-messages").unwrap() {
            BanOutcome::Reply(msg) => assert!(msg.contains("Only moderators")),
            _ => panic!("non-mod must not ban"),
        }
        match handle_bans_command(&chat, "unban badguy", &mut store, "score-messages").unwrap() {
            BanOutcome::Report(msg, _) => assert!(msg.contains("Unbanned `badguy`")),
            BanOutcome::Reply(m) => panic!("unexpected reply: {m}"),
        }
        assert!(!load_bans(store.path()).contains_key("guild1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
