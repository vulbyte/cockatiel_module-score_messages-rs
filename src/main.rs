use futures_util::{SinkExt, StreamExt};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{info, warn};
use tracing_subscriber::FmtSubscriber;

use cockatiel_client::{proto::container::Payload, proto::*, CockatielClient, PromptKind};

type WsWriteHalf = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    WsMessage,
>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Rule {
    toggle: bool,
    score: i64,
}

impl Default for Rule {
    fn default() -> Self {
        Self { toggle: true, score: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrequencyRule {
    toggle: bool,
    score: i64,
    #[serde(default = "default_interval")]
    interval_secs: u64,
}

impl Default for FrequencyRule {
    fn default() -> Self {
        Self {
            toggle: true,
            score: -2,
            interval_secs: default_interval(),
        }
    }
}

fn default_interval() -> u64 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    punctuation: Rule,
    #[serde(default)]
    question: Rule,
    #[serde(default)]
    length: Rule,
    #[serde(default)]
    spam: Rule,
    #[serde(default)]
    no_spacing: Rule,
    #[serde(default)]
    wordless: Rule,
    #[serde(default)]
    emoji: Rule,
    #[serde(default)]
    frequency: FrequencyRule,
    #[serde(default = "default_length_min_chars")]
    length_min_chars: i64,
    #[serde(default = "default_no_spacing_min_len")]
    no_spacing_min_len: i64,
    #[serde(default = "default_spam_min_token_len")]
    spam_min_token_len: i64,
    #[serde(default = "default_keyboard_mash_min_len")]
    keyboard_mash_min_len: i64,
    #[serde(default = "default_wordless_vowel_ratio")]
    wordless_vowel_ratio: f64,
    #[serde(default = "default_frequency_interval_floor_secs")]
    frequency_interval_floor_secs: u64,
    #[serde(default = "default_last_msg_cap")]
    last_msg_cap: usize,
    #[serde(default = "default_prompt_timeout_secs")]
    prompt_timeout_secs: u32,
    #[serde(default = "default_reconnect_base_secs")]
    reconnect_base_secs: u64,
    #[serde(default = "default_reconnect_max_secs")]
    reconnect_max_secs: u64,
}

fn default_length_min_chars() -> i64 {
    40
}

fn default_no_spacing_min_len() -> i64 {
    20
}

fn default_spam_min_token_len() -> i64 {
    4
}

fn default_keyboard_mash_min_len() -> i64 {
    5
}

fn default_wordless_vowel_ratio() -> f64 {
    0.5
}

fn default_frequency_interval_floor_secs() -> u64 {
    1
}

fn default_last_msg_cap() -> usize {
    10_000
}

fn default_prompt_timeout_secs() -> u32 {
    60
}

fn default_reconnect_base_secs() -> u64 {
    1
}

fn default_reconnect_max_secs() -> u64 {
    30
}

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

fn default_config() -> Config {
    Config {
        punctuation: Rule { toggle: true, score: 1 },
        question: Rule { toggle: true, score: 2 },
        length: Rule { toggle: true, score: 1 },
        spam: Rule { toggle: true, score: -5 },
        no_spacing: Rule { toggle: true, score: -2 },
        wordless: Rule { toggle: true, score: -3 },
        emoji: Rule { toggle: true, score: 1 },
        frequency: FrequencyRule::default(),
        length_min_chars: default_length_min_chars(),
        no_spacing_min_len: default_no_spacing_min_len(),
        spam_min_token_len: default_spam_min_token_len(),
        keyboard_mash_min_len: default_keyboard_mash_min_len(),
        wordless_vowel_ratio: default_wordless_vowel_ratio(),
        frequency_interval_floor_secs: default_frequency_interval_floor_secs(),
        last_msg_cap: default_last_msg_cap(),
        prompt_timeout_secs: default_prompt_timeout_secs(),
        reconnect_base_secs: default_reconnect_base_secs(),
        reconnect_max_secs: default_reconnect_max_secs(),
    }
}

/// Parse a toggle value that may be a JSON bool, number, or string like
/// "1"/"true". Unparseable garbage (e.g. "1t") yields None so callers fall
/// back to a default instead of panicking.
fn parse_toggle(v: Option<&serde_json::Value>) -> Option<bool> {
    match v? {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::Number(n) => n.as_i64().map(|n| n != 0),
        serde_json::Value::String(s) => {
            let t = s.trim().to_ascii_lowercase();
            match t.as_str() {
                "1" | "true" | "yes" | "y" | "on" => Some(true),
                "0" | "false" | "no" | "n" | "off" => Some(false),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a score value that may be a JSON number or string like "1"/"-5".
fn parse_score(v: Option<&serde_json::Value>) -> Option<i64> {
    match v? {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => {
            let t = s.trim();
            t.parse::<i64>()
                .ok()
                .or_else(|| t.parse::<f64>().ok().map(|f| f as i64))
        }
        serde_json::Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// Parse an interval value that may be a JSON number or string.
fn parse_u64(v: Option<&serde_json::Value>) -> Option<u64> {
    match v? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|i| u64::try_from(i).ok())),
        serde_json::Value::String(s) => {
            let t = s.trim();
            t.parse::<u64>()
                .ok()
                .or_else(|| t.parse::<i64>().ok().and_then(|i| u64::try_from(i).ok()))
        }
        _ => None,
    }
}

/// Parse an f64 value that may be a JSON number or string.
fn parse_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Parse a usize value that may be a JSON number or string.
fn parse_usize(v: Option<&serde_json::Value>) -> Option<usize> {
    parse_u64(v).and_then(|u| usize::try_from(u).ok())
}

/// Parse a u32 value that may be a JSON number or string.
fn parse_u32(v: Option<&serde_json::Value>) -> Option<u32> {
    parse_u64(v).and_then(|u| u32::try_from(u).ok())
}

/// Overlay the engine's flat module_specific credential keys onto a Config,
/// falling back to the given legacy values for any key that is absent or
/// unparseable.
fn apply_flat_overlay(mut cfg: Config, ms: &serde_json::Map<String, serde_json::Value>) -> Config {
    macro_rules! apply_rule {
        ($field:ident, $prefix:literal) => {
            if let Some(t) = parse_toggle(ms.get(concat!($prefix, "_toggle"))) {
                cfg.$field.toggle = t;
            }
            if let Some(s) = parse_score(ms.get(concat!($prefix, "_score"))) {
                cfg.$field.score = s;
            }
        };
    }
    apply_rule!(punctuation, "punctuation");
    apply_rule!(question, "question");
    apply_rule!(length, "length");
    apply_rule!(spam, "spam");
    apply_rule!(no_spacing, "no_spacing");
    apply_rule!(wordless, "wordless");
    apply_rule!(emoji, "emoji");
    apply_rule!(frequency, "frequency");
    if let Some(i) = parse_u64(ms.get("frequency_interval_secs")) {
        cfg.frequency.interval_secs = i;
    }
    if let Some(v) = parse_score(ms.get("length_min_chars")) {
        cfg.length_min_chars = v;
    }
    if let Some(v) = parse_score(ms.get("no_spacing_min_len")) {
        cfg.no_spacing_min_len = v;
    }
    if let Some(v) = parse_score(ms.get("spam_min_token_len")) {
        cfg.spam_min_token_len = v;
    }
    if let Some(v) = parse_score(ms.get("keyboard_mash_min_len")) {
        cfg.keyboard_mash_min_len = v;
    }
    if let Some(v) = parse_f64(ms.get("wordless_vowel_ratio")) {
        cfg.wordless_vowel_ratio = v;
    }
    if let Some(v) = parse_u64(ms.get("frequency_interval_floor_secs")) {
        cfg.frequency_interval_floor_secs = v;
    }
    if let Some(v) = parse_usize(ms.get("last_msg_cap")) {
        cfg.last_msg_cap = v;
    }
    if let Some(v) = parse_u32(ms.get("prompt_timeout_secs")) {
        cfg.prompt_timeout_secs = v;
    }
    if let Some(v) = parse_u64(ms.get("reconnect_base_secs")) {
        cfg.reconnect_base_secs = v;
    }
    if let Some(v) = parse_u64(ms.get("reconnect_max_secs")) {
        cfg.reconnect_max_secs = v;
    }
    cfg
}

/// Build the flat module_specific map for a Config, preferring values already
/// present (and parseable) in the existing module_specific object, then
/// migrating legacy top-level nested values, then falling back to the given
/// Config.
fn flat_map_for_write(
    cfg: &Config,
    root: &serde_json::Value,
    ms: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    macro_rules! put_rule {
        ($field:ident, $prefix:literal) => {
            let toggle_key = concat!($prefix, "_toggle");
            let score_key = concat!($prefix, "_score");
            let legacy = root.get($prefix).and_then(|r| r.as_object());
            let toggle = parse_toggle(ms.get(toggle_key))
                .or_else(|| legacy.and_then(|l| parse_toggle(l.get("toggle"))))
                .unwrap_or(cfg.$field.toggle);
            let score = parse_score(ms.get(score_key))
                .or_else(|| legacy.and_then(|l| parse_score(l.get("score"))))
                .unwrap_or(cfg.$field.score);
            out.insert(toggle_key.to_string(), serde_json::Value::Bool(toggle));
            out.insert(score_key.to_string(), serde_json::json!(score));
        };
    }
    put_rule!(punctuation, "punctuation");
    put_rule!(question, "question");
    put_rule!(length, "length");
    put_rule!(spam, "spam");
    put_rule!(no_spacing, "no_spacing");
    put_rule!(wordless, "wordless");
    put_rule!(emoji, "emoji");
    put_rule!(frequency, "frequency");
    let interval = parse_u64(ms.get("frequency_interval_secs")).unwrap_or(cfg.frequency.interval_secs);
    out.insert("frequency_interval_secs".to_string(), serde_json::json!(interval));
    macro_rules! put_scalar {
        ($key:literal, $parse:ident, $cfg_val:expr) => {{
            let v = $parse(ms.get($key)).unwrap_or($cfg_val);
            out.insert($key.to_string(), serde_json::json!(v));
        }};
    }
    put_scalar!("length_min_chars", parse_score, cfg.length_min_chars);
    put_scalar!("no_spacing_min_len", parse_score, cfg.no_spacing_min_len);
    put_scalar!("spam_min_token_len", parse_score, cfg.spam_min_token_len);
    put_scalar!("keyboard_mash_min_len", parse_score, cfg.keyboard_mash_min_len);
    put_scalar!("wordless_vowel_ratio", parse_f64, cfg.wordless_vowel_ratio);
    put_scalar!(
        "frequency_interval_floor_secs",
        parse_u64,
        cfg.frequency_interval_floor_secs
    );
    put_scalar!("last_msg_cap", parse_usize, cfg.last_msg_cap);
    put_scalar!("prompt_timeout_secs", parse_u32, cfg.prompt_timeout_secs);
    put_scalar!("reconnect_base_secs", parse_u64, cfg.reconnect_base_secs);
    put_scalar!("reconnect_max_secs", parse_u64, cfg.reconnect_max_secs);
    out
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
                return apply_flat_overlay(legacy, ms);
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

/// Is this token made of repeated characters (spam: bbbbb, asdlfkjsqaldkfja)?
fn looks_like_spam_token(token: &str, min_token_len: i64, mash_min_len: i64) -> bool {
    let t = token.to_lowercase();
    if (t.len() as i64) < min_token_len {
        return false;
    }
    let chars: Vec<char> = t.chars().collect();
    // Repeated single char, or no vowels (keyboard mashing).
    let all_same = chars.iter().all(|c| *c == chars[0]);
    if all_same {
        return true;
    }
    let has_vowel = chars.iter().any(|c| "aeiou".contains(*c));
    !has_vowel && (t.len() as i64) >= mash_min_len
}

fn is_wordless(message: &str, vowel_ratio: f64) -> bool {
    // Uses a crude trigram check: if most tokens contain no vowels, treat as wordless.
    let tokens: Vec<&str> = message.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    let wordlike = tokens
        .iter()
        .filter(|t| t.chars().any(|c| "aeiou".contains(c.to_ascii_lowercase())))
        .count();
    (wordlike as f64 / tokens.len() as f64) < vowel_ratio
}

/// Common emoji Unicode ranges.
fn is_emoji(c: char) -> bool {
    let cp = c as u32;
    (0x1F300..=0x1F5FF).contains(&cp)
        || (0x1F600..=0x1F64F).contains(&cp)
        || (0x1F680..=0x1F6FF).contains(&cp)
        || (0x1F900..=0x1F9FF).contains(&cp)
        || (0x1FA70..=0x1FAFF).contains(&cp)
        || (0x2600..=0x27BF).contains(&cp)
        || (0xFE00..=0xFE0F).contains(&cp)
        || (0x1F000..=0x1F0FF).contains(&cp)
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

/// Score a message and return the total delta. Positive = reward, negative = punishment.
fn score_message(message: &str, cfg: &Config) -> (i64, Vec<String>) {
    let mut delta = 0i64;
    let mut notes = Vec::new();

    let trimmed = message.trim();

    // Punctuation: reward for ending with ., !, ?.
    if cfg.punctuation.toggle {
        if let Some(last) = trimmed.chars().last() {
            if ".!?".contains(last) {
                delta += cfg.punctuation.score;
                notes.push("punctuation".into());
            }
        }
    }

    // Questions: reward for ending with ?.
    if cfg.question.toggle && trimmed.ends_with('?') {
        delta += cfg.question.score;
        notes.push("question".into());
    }

    // Length: reward for substantial messages (>= length_min_chars).
    if cfg.length.toggle && (trimmed.chars().count() as i64) >= cfg.length_min_chars {
        delta += cfg.length.score;
        notes.push("length".into());
    }

    // Spam: repeated-char tokens / keyboard mash.
    if cfg.spam.toggle {
        let spam = trimmed.split_whitespace().any(|tok| {
            looks_like_spam_token(tok, cfg.spam_min_token_len, cfg.keyboard_mash_min_len)
        });
        if spam {
            delta += cfg.spam.score;
            notes.push("spam".into());
        }
    }

    // No spacing: long message with very few spaces.
    if cfg.no_spacing.toggle {
        let len = trimmed.chars().count();
        let spaces = trimmed.chars().filter(|c| *c == ' ').count();
        if (len as i64) >= cfg.no_spacing_min_len && spaces == 0 {
            delta += cfg.no_spacing.score;
            notes.push("no_spacing".into());
        }
    }

    // Wordless: mostly vowel-less tokens.
    if cfg.wordless.toggle && is_wordless(trimmed, cfg.wordless_vowel_ratio) {
        delta += cfg.wordless.score;
        notes.push("wordless".into());
    }

    // Emoji: reward messages that include an emoji (engagement).
    if cfg.emoji.toggle && trimmed.chars().any(is_emoji) {
        delta += cfg.emoji.score;
        notes.push("emoji".into());
    }

    (delta, notes)
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

    fn all_off() -> Config {
        let mut c = default_config();
        for r in [&mut c.punctuation, &mut c.question, &mut c.length, &mut c.spam, &mut c.no_spacing, &mut c.wordless, &mut c.emoji] {
            r.toggle = false;
        }
        c
    }

    #[test]
    fn punctuation_rewarded() {
        let c = all_off();
        let (delta, _) = score_message("hello there", &c);
        assert_eq!(delta, 0);
        let mut c2 = c.clone();
        c2.punctuation.toggle = true;
        let (delta, notes) = score_message("hello there!", &c2);
        assert_eq!(delta, 1);
        assert!(notes.contains(&"punctuation".to_string()));
    }

    #[test]
    fn question_rewarded() {
        let mut c = all_off();
        c.question.toggle = true;
        let (delta, notes) = score_message("are you ok?", &c);
        assert_eq!(delta, 2);
        assert!(notes.contains(&"question".to_string()));
    }

    #[test]
    fn length_rewarded_over_40() {
        let mut c = all_off();
        c.length.toggle = true;
        let long = "this is a fairly long message that definitely exceeds forty characters by a bit";
        let (delta, _) = score_message(long, &c);
        assert_eq!(delta, 1);
        let (delta, _) = score_message("short", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn spam_punished() {
        let mut c = all_off();
        c.spam.toggle = true;
        let (delta, notes) = score_message("aaaaaaa llllllll oooooooo", &c);
        assert_eq!(delta, -5);
        assert!(notes.contains(&"spam".to_string()));
    }

    #[test]
    fn emoji_rewarded() {
        let mut c = all_off();
        c.emoji.toggle = true;
        let (delta, _) = score_message("nice stream 👍", &c);
        assert_eq!(delta, 1);
    }

    #[test]
    fn wordless_punished() {
        let mut c = all_off();
        c.wordless.toggle = true;
        let (delta, _) = score_message("tr th s", &c);
        assert_eq!(delta, -3);
    }

    #[test]
    fn flat_overlay_round_trips_new_tunables() {
        let cfg = default_config();
        let root = serde_json::json!({});
        let written = flat_map_for_write(&cfg, &root, &serde_json::Map::new());
        // Defaults are written back for every new tunable key.
        assert_eq!(written["length_min_chars"], 40);
        assert_eq!(written["no_spacing_min_len"], 20);
        assert_eq!(written["spam_min_token_len"], 4);
        assert_eq!(written["keyboard_mash_min_len"], 5);
        assert_eq!(written["wordless_vowel_ratio"], 0.5);
        assert_eq!(written["frequency_interval_floor_secs"], 1);
        assert_eq!(written["last_msg_cap"], 10_000);
        assert_eq!(written["prompt_timeout_secs"], 60);
        assert_eq!(written["reconnect_base_secs"], 1);
        assert_eq!(written["reconnect_max_secs"], 30);
        // And read back by the overlay, preserving the defaults.
        let restored = apply_flat_overlay(default_config(), &written);
        assert_eq!(restored.length_min_chars, 40);
        assert_eq!(restored.no_spacing_min_len, 20);
        assert_eq!(restored.spam_min_token_len, 4);
        assert_eq!(restored.keyboard_mash_min_len, 5);
        assert_eq!(restored.wordless_vowel_ratio, 0.5);
        assert_eq!(restored.frequency_interval_floor_secs, 1);
        assert_eq!(restored.last_msg_cap, 10_000);
        assert_eq!(restored.prompt_timeout_secs, 60);
        assert_eq!(restored.reconnect_base_secs, 1);
        assert_eq!(restored.reconnect_max_secs, 30);
        // A configured override survives the write → read round trip.
        let mut overridden = written.clone();
        overridden.insert("length_min_chars".to_string(), serde_json::json!(99));
        overridden.insert("reconnect_max_secs".to_string(), serde_json::json!(120));
        let restored = apply_flat_overlay(default_config(), &overridden);
        assert_eq!(restored.length_min_chars, 99);
        assert_eq!(restored.reconnect_max_secs, 120);
    }

    #[test]
    fn tunables_drive_scoring() {
        let mut c = all_off();
        // Lower the length bar: a short message now earns the length reward.
        c.length.toggle = true;
        c.length_min_chars = 5;
        let (delta, notes) = score_message("hello", &c);
        assert_eq!(delta, 1);
        assert!(notes.contains(&"length".to_string()));

        // Raise the wordless bar so a mildly vowel-less message is not penalized.
        let mut w = all_off();
        w.wordless.toggle = true;
        w.wordless_vowel_ratio = 0.1;
        assert_eq!(score_message("tr th s ae", &w).0, 0);
        let mut d = all_off();
        d.wordless.toggle = true;
        assert_eq!(score_message("tr th s ae", &d).0, -3);
    }

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
