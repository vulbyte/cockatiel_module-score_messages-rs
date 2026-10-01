//! The pure scoring core for the score-messages module.
//!
//! Scoring follows the Animal Crossing (GCN) letter-scoring algorithm (see
//! https://hunter-r.com/ac-letter-scorer/ and
//! https://github.com/HunterRDev/AC-Letter-Scorer), ported to chat messages.
//! A message is scored through eight checks (A–H) whose points sum to the
//! delta applied to the user's score:
//!
//!   A. Punctuation        — +30 if the text ends with `.`/`!`/`?` (and is
//!                           under the 192-char max); then for every
//!                           punctuation mark, +20 if a capital letter appears
//!                           within the next 3 chars, else 0.
//!   B. Trigrams           — +8 per valid leading-3-chars-of-each-word
//!                           trigram found in the game's (bugged) trigram
//!                           tables.
//!   C. Leading capital    — +30 if the first non-space char is a capital,
//!                           else 0.
//!   D. Repeating chars    — −50 if any letter is repeated 3+ times
//!                           sequentially (spaces ignored).
//!   E. Space ratio        — +25 if spaces are ≥ 10% of non-space chars,
//!                           else +5 (the penalty is a negative number, so a
//!                           space-less short message is *rewarded* not
//!                           punished — modern chat is built on "gg", "lol").
//!   F. Run-on sentence    — −150 if a run of 75+ chars has no punctuation
//!                           after a punctuation mark.
//!   G. 32-char groupings  — −20 per 32-char group that contains no space.
//!   H. Spam & gibberish   — −100 per signal: `spam_emoji_max`+ emoji, a
//!                           symbol-ratio wall (morse), or a vowel-less
//!                           long word. The counter to the positive bias.
//!
//! The weights are tuned to bias scoring toward positive: lowercase is never
//! penalised, short space-less messages are rewarded, and the pure rewards
//! (emoji, trigrams, punctuation, leading capital) are the main driver of a
//! user's score — while exploitative spam is still punished harshly.
//!
//! Everything is a deterministic function of its inputs (a message + a
//! config), with no network, no state and no side effects — which is exactly
//! what makes it unit-testable and probeable: the tuning probe
//! (`src/bin/score_probe.rs`) runs real chat sentences through this same code
//! to visualise how the checks shape the distribution of message scores.
//!
//! The live module (`main.rs`) calls the same functions, so a tuning decision
//! made against the probe is guaranteed to match production behaviour.

use serde::{Deserialize, Serialize};

/// The game's letter body maximum. Checks A uses this as a ceiling.
const MAX_CHARS: usize = 192;

/// The run-on check's no-punctuation threshold (chars).
pub const RUN_ON_CHARS: i32 = 75;

/// The 32-char space-grouping window.
pub const GROUPING_SIZE: usize = 32;

/// The space-ratio threshold (spaces ÷ non-spaces, ×100). Tuned to 10% so a
/// message with any spaces earns the reward.
pub const SPACE_RATIO_PERCENT: i32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub toggle: bool,
    pub score: i32,
}

impl Default for Rule {
    fn default() -> Self {
        Self { toggle: true, score: 20 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequencyRule {
    pub toggle: bool,
    pub score: i32,
    #[serde(default = "default_interval")]
    pub interval_secs: u32,
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

fn default_interval() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    // ── The seven AC letter checks ──────────────────────────────────────
    /// Check A: punctuation.
    #[serde(default = "default_punctuation")]
    pub punctuation: Rule,
    /// Check B: valid trigrams.
    #[serde(default = "default_trigram")]
    pub trigram: Rule,
    /// Path to an external trigram table (CSV, JSON, or `~X~`-sectioned text).
    /// Empty = use the embedded game-accurate (bugged) table.
    #[serde(default)]
    pub trigram_table_path: String,
    /// Check C: leading capital.
    #[serde(default = "default_capital")]
    pub capital: Rule,
    /// Check D: repeating characters.
    #[serde(default = "default_repeating")]
    pub repeating: Rule,
    /// Check E: space ratio.
    #[serde(default = "default_space_ratio")]
    pub space_ratio: Rule,
    /// Check E: points applied when the space ratio is too low. This is a
    /// *negative* number by default (−5), so a space-less short message is
    /// actually rewarded (`-(-5)` = +5) rather than punished — modern chat is
    /// built on "gg", "lol", "pog". The reward and this value are decoupled so
    /// the generous reward on well-spaced text doesn't hammer short messages.
    #[serde(default = "default_space_ratio_penalty")]
    pub space_ratio_penalty: i32,
    /// Check F: run-on sentence.
    #[serde(default = "default_run_on")]
    pub run_on: Rule,
    /// Check G: 32-char space groupings.
    #[serde(default = "default_grouping")]
    pub grouping: Rule,
    // ── Chat-specific extras (not part of the AC algorithm) ────────────
    /// Reward messages that include an emoji (engagement signal).
    #[serde(default = "default_emoji")]
    pub emoji: Rule,
    /// Check H: punish spammy messages — too many emojis, symbol-heavy
    /// (morse-like) text, or long vowel-less (gibberish) words.
    #[serde(default = "default_spam")]
    pub spam: Rule,
    /// Check H: max emoji before the spam penalty fires.
    #[serde(default = "default_spam_emoji_max")]
    pub spam_emoji_max: i32,
    /// Check H: % of non-space chars that are symbols (`.`, `-`, emoji, …)
    /// which flags symbol/morse spam.
    #[serde(default = "default_spam_symbol_ratio_pct")]
    pub spam_symbol_ratio_pct: i32,
    /// Check H: min word length to be checked for gibberish (vowel-less).
    #[serde(default = "default_spam_gibberish_word_min")]
    pub spam_gibberish_word_min: i32,
    /// Check H: max vowel ratio (×100) below which a long word is gibberish.
    #[serde(default = "default_spam_gibberish_vowel_pct")]
    pub spam_gibberish_vowel_pct: i32,
    /// Punish posting again too soon after a user's last message.
    #[serde(default)]
    pub frequency: FrequencyRule,
    // ── Viewership multiplier ───────────────────────────────────────────
    /// Scale the score delta by a viewership factor: small streams reward
    /// interaction more (multiplier = max_viewers / viewers, clamped to
    /// `viewership_multiplier_max`), so large streamers get more room to
    /// breathe. Disabled by default.
    #[serde(default)]
    pub viewership_toggle: bool,
    /// The reference viewer count where the multiplier is 1.0 (the streamer's
    /// "full house"). Fewer viewers → a higher multiplier.
    #[serde(default = "default_viewership_max_viewers")]
    pub viewership_max_viewers: i32,
    /// The largest multiplier ever applied (clamp). 0 viewers (offline) also
    /// uses this ceiling.
    #[serde(default = "default_viewership_multiplier_max")]
    pub viewership_multiplier_max: f32,
    /// How often (seconds) the module re-queries the engine for viewer counts.
    #[serde(default = "default_viewership_poll_secs")]
    pub viewership_poll_secs: u32,
    // ── Tunable scalars (defaults match the game's algorithm) ───────────
    #[serde(default = "default_run_on_chars")]
    pub run_on_chars: i32,
    #[serde(default = "default_grouping_size")]
    pub grouping_size: usize,
    #[serde(default = "default_space_ratio_pct")]
    pub space_ratio_pct: i32,
    /// Check A: the letter-body ceiling under which the ending-punctuation
    /// reward is granted (and the trigram padding length).
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Check A: points per punctuation mark that is followed (within 3 chars)
    /// by a capital letter.
    #[serde(default = "default_punct_capital_bonus")]
    pub punct_capital_bonus: i32,
    /// Check A: points per punctuation mark NOT followed (within 3 chars) by a
    /// capital letter.
    #[serde(default = "default_punct_capital_penalty")]
    pub punct_capital_penalty: i32,
    /// Check C: the penalty when the leading non-space char is not a capital.
    #[serde(default = "default_capital_penalty")]
    pub capital_penalty: i32,
    /// Check D: how many sequential repeats of a letter trigger the penalty.
    #[serde(default = "default_repeating_threshold")]
    pub repeating_threshold: i32,
    /// Check F: the run-on scan only evaluates while more than this many chars
    /// remain in the message.
    #[serde(default = "default_run_on_window")]
    pub run_on_window: i32,
    #[serde(default = "default_frequency_interval_floor_secs")]
    pub frequency_interval_floor_secs: u32,
    #[serde(default = "default_last_msg_cap")]
    pub last_msg_cap: u32,
    #[serde(default = "default_prompt_timeout_secs")]
    pub prompt_timeout_secs: u32,
    #[serde(default = "default_reconnect_base_secs")]
    pub reconnect_base_secs: u32,
    #[serde(default = "default_reconnect_max_secs")]
    pub reconnect_max_secs: u32,
}

fn default_punctuation() -> Rule {
    Rule { toggle: true, score: 30 }
}
fn default_trigram() -> Rule {
    Rule { toggle: true, score: 8 }
}
fn default_capital() -> Rule {
    Rule { toggle: true, score: 30 }
}
fn default_repeating() -> Rule {
    Rule { toggle: true, score: 50 }
}
fn default_space_ratio() -> Rule {
    Rule { toggle: true, score: 25 }
}
fn default_run_on() -> Rule {
    Rule { toggle: true, score: 150 }
}
fn default_grouping() -> Rule {
    Rule { toggle: true, score: 20 }
}
fn default_emoji() -> Rule {
    Rule { toggle: true, score: 15 }
}
fn default_spam() -> Rule {
    Rule { toggle: true, score: 100 }
}

pub fn default_spam_emoji_max() -> i32 {
    5
}
pub fn default_spam_symbol_ratio_pct() -> i32 {
    30
}
pub fn default_spam_gibberish_word_min() -> i32 {
    8
}
pub fn default_spam_gibberish_vowel_pct() -> i32 {
    25
}

pub fn default_space_ratio_penalty() -> i32 {
    -5
}

pub fn default_viewership_max_viewers() -> i32 {
    1000
}
pub fn default_viewership_multiplier_max() -> f32 {
    5.0
}
pub fn default_viewership_poll_secs() -> u32 {
    30
}

pub fn default_run_on_chars() -> i32 {
    RUN_ON_CHARS
}
pub fn default_grouping_size() -> usize {
    GROUPING_SIZE
}
pub fn default_max_chars() -> usize {
    MAX_CHARS
}
pub fn default_punct_capital_bonus() -> i32 {
    20
}
pub fn default_punct_capital_penalty() -> i32 {
    0
}
pub fn default_capital_penalty() -> i32 {
    0
}
pub fn default_repeating_threshold() -> i32 {
    3
}
pub fn default_run_on_window() -> i32 {
    76
}
pub fn default_space_ratio_pct() -> i32 {
    SPACE_RATIO_PERCENT
}
pub fn default_frequency_interval_floor_secs() -> u32 {
    1
}
pub fn default_last_msg_cap() -> u32 {
    10_000
}
pub fn default_prompt_timeout_secs() -> u32 {
    60
}
pub fn default_reconnect_base_secs() -> u32 {
    1
}
pub fn default_reconnect_max_secs() -> u32 {
    30
}

pub fn default_config() -> Config {
    Config {
        punctuation: Rule { toggle: true, score: 30 },
        trigram: default_trigram(),
        trigram_table_path: String::new(),
        capital: default_capital(),
        repeating: default_repeating(),
        space_ratio: default_space_ratio(),
        space_ratio_penalty: default_space_ratio_penalty(),
        run_on: default_run_on(),
        grouping: default_grouping(),
        emoji: default_emoji(),
        spam: default_spam(),
        spam_emoji_max: default_spam_emoji_max(),
        spam_symbol_ratio_pct: default_spam_symbol_ratio_pct(),
        spam_gibberish_word_min: default_spam_gibberish_word_min(),
        spam_gibberish_vowel_pct: default_spam_gibberish_vowel_pct(),
        frequency: FrequencyRule::default(),
        viewership_toggle: false,
        viewership_max_viewers: default_viewership_max_viewers(),
        viewership_multiplier_max: default_viewership_multiplier_max(),
        viewership_poll_secs: default_viewership_poll_secs(),
        run_on_chars: default_run_on_chars(),
        grouping_size: default_grouping_size(),
        space_ratio_pct: default_space_ratio_pct(),
        max_chars: default_max_chars(),
        punct_capital_bonus: default_punct_capital_bonus(),
        punct_capital_penalty: default_punct_capital_penalty(),
        capital_penalty: default_capital_penalty(),
        repeating_threshold: default_repeating_threshold(),
        run_on_window: default_run_on_window(),
        frequency_interval_floor_secs: default_frequency_interval_floor_secs(),
        last_msg_cap: default_last_msg_cap(),
        prompt_timeout_secs: default_prompt_timeout_secs(),
        reconnect_base_secs: default_reconnect_base_secs(),
        reconnect_max_secs: default_reconnect_max_secs(),
    }
}

// ── config parsing helpers (identical to the live module's semantics) ───

/// Parse a toggle value that may be a JSON bool, number, or string like
/// "1"/"true". Unparseable garbage yields None so callers fall back to a
/// default instead of panicking.
pub fn parse_toggle(v: Option<&serde_json::Value>) -> Option<bool> {
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
pub fn parse_score(v: Option<&serde_json::Value>) -> Option<i64> {
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
pub fn parse_u64(v: Option<&serde_json::Value>) -> Option<u64> {
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
pub fn parse_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Parse a usize value that may be a JSON number or string.
pub fn parse_usize(v: Option<&serde_json::Value>) -> Option<usize> {
    parse_u64(v).and_then(|u| usize::try_from(u).ok())
}

/// Parse a u32 value that may be a JSON number or string.
pub fn parse_u32(v: Option<&serde_json::Value>) -> Option<u32> {
    parse_u64(v).and_then(|u| u32::try_from(u).ok())
}

/// Overlay the engine's flat module_specific credential keys onto a Config.
pub fn apply_flat_overlay(mut cfg: Config, ms: &serde_json::Map<String, serde_json::Value>) -> Config {
    macro_rules! apply_rule {
        ($field:ident, $prefix:literal) => {
            if let Some(t) = parse_toggle(ms.get(concat!($prefix, "_toggle"))) {
                cfg.$field.toggle = t;
            }
            if let Some(s) = parse_score(ms.get(concat!($prefix, "_score"))) {
                cfg.$field.score = s as i32;
            }
        };
    }
    apply_rule!(punctuation, "punctuation");
    apply_rule!(trigram, "trigram");
    if let Some(p) = ms.get("trigram_table_path").and_then(|v| v.as_str()) {
        cfg.trigram_table_path = p.to_string();
    }
    apply_rule!(capital, "capital");
    apply_rule!(repeating, "repeating");
    apply_rule!(space_ratio, "space_ratio");
    if let Some(v) = parse_i64(ms.get("space_ratio_penalty")) {
        cfg.space_ratio_penalty = v as i32;
    }
    apply_rule!(run_on, "run_on");
    apply_rule!(grouping, "grouping");
    apply_rule!(emoji, "emoji");
    apply_rule!(spam, "spam");
    if let Some(v) = parse_usize(ms.get("spam_emoji_max")) {
        cfg.spam_emoji_max = v as i32;
    }
    if let Some(v) = parse_i64(ms.get("spam_symbol_ratio_pct")) {
        cfg.spam_symbol_ratio_pct = v as i32;
    }
    if let Some(v) = parse_usize(ms.get("spam_gibberish_word_min")) {
        cfg.spam_gibberish_word_min = v as i32;
    }
    if let Some(v) = parse_i64(ms.get("spam_gibberish_vowel_pct")) {
        cfg.spam_gibberish_vowel_pct = v as i32;
    }
    apply_rule!(frequency, "frequency");
    if let Some(t) = parse_toggle(ms.get("viewership_toggle")) {
        cfg.viewership_toggle = t;
    }
    if let Some(v) = parse_score(ms.get("viewership_max_viewers")) {
        cfg.viewership_max_viewers = v as i32;
    }
    if let Some(v) = parse_f64(ms.get("viewership_multiplier_max")) {
        cfg.viewership_multiplier_max = v as f32;
    }
    if let Some(v) = parse_u64(ms.get("viewership_poll_secs")) {
        cfg.viewership_poll_secs = v as u32;
    }
    if let Some(i) = parse_u64(ms.get("frequency_interval_secs")) {
        cfg.frequency.interval_secs = i as u32;
    }
    if let Some(v) = parse_usize(ms.get("run_on_chars")) {
        cfg.run_on_chars = v as i32;
    }
    if let Some(v) = parse_usize(ms.get("grouping_size")) {
        cfg.grouping_size = v;
    }
    if let Some(v) = parse_i64(ms.get("space_ratio_pct")) {
        cfg.space_ratio_pct = v as i32;
    }
    if let Some(v) = parse_usize(ms.get("max_chars")) {
        cfg.max_chars = v;
    }
    if let Some(v) = parse_i64(ms.get("punct_capital_bonus")) {
        cfg.punct_capital_bonus = v as i32;
    }
    if let Some(v) = parse_i64(ms.get("punct_capital_penalty")) {
        cfg.punct_capital_penalty = v as i32;
    }
    if let Some(v) = parse_i64(ms.get("capital_penalty")) {
        cfg.capital_penalty = v as i32;
    }
    if let Some(v) = parse_usize(ms.get("repeating_threshold")) {
        cfg.repeating_threshold = v as i32;
    }
    if let Some(v) = parse_usize(ms.get("run_on_window")) {
        cfg.run_on_window = v as i32;
    }
    if let Some(v) = parse_u64(ms.get("frequency_interval_floor_secs")) {
        cfg.frequency_interval_floor_secs = v as u32;
    }
    if let Some(v) = parse_usize(ms.get("last_msg_cap")) {
        cfg.last_msg_cap = v as u32;
    }
    if let Some(v) = parse_u32(ms.get("prompt_timeout_secs")) {
        cfg.prompt_timeout_secs = v;
    }
    if let Some(v) = parse_u64(ms.get("reconnect_base_secs")) {
        cfg.reconnect_base_secs = v as u32;
    }
    if let Some(v) = parse_u64(ms.get("reconnect_max_secs")) {
        cfg.reconnect_max_secs = v as u32;
    }
    cfg
}

fn parse_i64(v: Option<&serde_json::Value>) -> Option<i64> {
    parse_score(v)
}

/// Build the flat module_specific map for a Config (defaults first, so every
/// tunable key always exists and is editable in place).
pub fn flat_map_for_write(
    cfg: &Config,
    _root: &serde_json::Value,
    ms: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    macro_rules! put_rule {
        ($field:ident, $prefix:literal) => {
            let toggle_key = concat!($prefix, "_toggle");
            let score_key = concat!($prefix, "_score");
            let toggle = parse_toggle(ms.get(toggle_key)).unwrap_or(cfg.$field.toggle);
            let score = parse_score(ms.get(score_key)).map(|s| s as i32).unwrap_or(cfg.$field.score);
            out.insert(toggle_key.to_string(), serde_json::Value::Bool(toggle));
            out.insert(score_key.to_string(), serde_json::json!(score));
        };
    }
    put_rule!(punctuation, "punctuation");
    put_rule!(trigram, "trigram");
    out.insert(
        "trigram_table_path".to_string(),
        serde_json::json!(ms.get("trigram_table_path")
            .and_then(|v| v.as_str())
            .unwrap_or(&cfg.trigram_table_path)),
    );
    put_rule!(capital, "capital");
    put_rule!(repeating, "repeating");
    put_rule!(space_ratio, "space_ratio");
    out.insert(
        "space_ratio_penalty".to_string(),
        serde_json::json!(
            parse_score(ms.get("space_ratio_penalty")).map(|s| s as i32).unwrap_or(cfg.space_ratio_penalty)
        ),
    );
    put_rule!(run_on, "run_on");
    put_rule!(grouping, "grouping");
    put_rule!(emoji, "emoji");
    put_rule!(spam, "spam");
    out.insert(
        "spam_emoji_max".to_string(),
        serde_json::json!(
            parse_score(ms.get("spam_emoji_max")).unwrap_or(cfg.spam_emoji_max as i64)
        ),
    );
    out.insert(
        "spam_symbol_ratio_pct".to_string(),
        serde_json::json!(
            parse_score(ms.get("spam_symbol_ratio_pct")).map(|s| s as i32).unwrap_or(cfg.spam_symbol_ratio_pct)
        ),
    );
    out.insert(
        "spam_gibberish_word_min".to_string(),
        serde_json::json!(
            parse_score(ms.get("spam_gibberish_word_min")).unwrap_or(cfg.spam_gibberish_word_min as i64)
        ),
    );
    out.insert(
        "spam_gibberish_vowel_pct".to_string(),
        serde_json::json!(
            parse_score(ms.get("spam_gibberish_vowel_pct")).map(|s| s as i32).unwrap_or(cfg.spam_gibberish_vowel_pct)
        ),
    );
    put_rule!(frequency, "frequency");
    out.insert(
        "viewership_toggle".to_string(),
        serde_json::Value::Bool(
            parse_toggle(ms.get("viewership_toggle")).unwrap_or(cfg.viewership_toggle),
        ),
    );
    out.insert(
        "viewership_max_viewers".to_string(),
        serde_json::json!(
            parse_score(ms.get("viewership_max_viewers")).map(|s| s as i32).unwrap_or(cfg.viewership_max_viewers)
        ),
    );
    out.insert(
        "viewership_multiplier_max".to_string(),
        serde_json::json!(
            parse_f64(ms.get("viewership_multiplier_max")).map(|v| v as f32).unwrap_or(cfg.viewership_multiplier_max)
        ),
    );
    out.insert(
        "viewership_poll_secs".to_string(),
        serde_json::json!(
            parse_u64(ms.get("viewership_poll_secs")).map(|v| v as u32).unwrap_or(cfg.viewership_poll_secs)
        ),
    );
    let interval = parse_u64(ms.get("frequency_interval_secs")).map(|v| v as u32).unwrap_or(cfg.frequency.interval_secs);
    out.insert("frequency_interval_secs".to_string(), serde_json::json!(interval));
    macro_rules! put_scalar {
        ($key:literal, $parse:ident, $cfg_val:expr) => {{
            let v = $parse(ms.get($key)).unwrap_or($cfg_val);
            out.insert($key.to_string(), serde_json::json!(v));
        }};
    }
    put_scalar!("run_on_chars", parse_usize, cfg.run_on_chars as usize);
    put_scalar!("grouping_size", parse_usize, cfg.grouping_size);
    put_scalar!("space_ratio_pct", parse_i64, cfg.space_ratio_pct as i64);
    put_scalar!("max_chars", parse_usize, cfg.max_chars);
    put_scalar!("punct_capital_bonus", parse_i64, cfg.punct_capital_bonus as i64);
    put_scalar!("punct_capital_penalty", parse_i64, cfg.punct_capital_penalty as i64);
    put_scalar!("capital_penalty", parse_i64, cfg.capital_penalty as i64);
    put_scalar!("repeating_threshold", parse_usize, cfg.repeating_threshold as usize);
    put_scalar!("run_on_window", parse_usize, cfg.run_on_window as usize);
    put_scalar!(
        "frequency_interval_floor_secs",
        parse_u64,
        cfg.frequency_interval_floor_secs as u64
    );
    put_scalar!("last_msg_cap", parse_usize, cfg.last_msg_cap as usize);
    put_scalar!("prompt_timeout_secs", parse_u32, cfg.prompt_timeout_secs);
    put_scalar!("reconnect_base_secs", parse_u64, cfg.reconnect_base_secs as u64);
    put_scalar!("reconnect_max_secs", parse_u64, cfg.reconnect_max_secs as u64);
    out
}

/// Resolve a Config from a config.json root the same way the live module does.
pub fn config_from_root(root: &serde_json::Value) -> Config {
    if let Some(ms) = root.get("module_specific").and_then(|v| v.as_object()) {
        let legacy = serde_json::from_value::<Config>(root.clone())
            .unwrap_or_else(|_| default_config());
        return apply_flat_overlay(legacy, ms);
    }
    if let Ok(cfg) = serde_json::from_value::<Config>(root.clone()) {
        return cfg;
    }
    default_config()
}

// ── the seven AC checks ─────────────────────────────────────────────────

/// Common emoji Unicode ranges (chat-specific extra, not an AC check).
pub fn is_emoji(c: char) -> bool {
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

/// Check A — Punctuation.
///
/// +20 if the (whitespace-trimmed) text ends with `.`/`!`/`?` and is under the
/// 192-char max. Then scan every punctuation mark: if a capital letter appears
/// within the next 3 characters, +10; otherwise −10. Scan position advances
/// past the found capital (or 4 chars) so each occurrence is scored once.
fn check_a(input: &str, cfg: &Config) -> i32 {
    let trimmed: String = input.trim_matches([' ', '\t']).to_string();
    let mut score = 0;

    // The ending-punctuation reward is for real sentences: only grant it when
    // the message actually contains letters/digits. A symbol-only wall (morse
    // code, ".........") ends in a `.` but is not a sentence.
    if trimmed.chars().count() < cfg.max_chars
        && matches!(trimmed.chars().last(), Some('.') | Some('!') | Some('?'))
        && input.chars().any(|c| c.is_alphanumeric())
    {
        score += cfg.punctuation.score;
    }

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let Some(rel) = chars[i..].iter().position(|c| matches!(c, '.' | '!' | '?')) else {
            break;
        };
        let punct = i + rel;
        // Nothing non-space after the punctuation: stop scanning.
        if chars[punct + 1..].iter().all(|c| c.is_whitespace()) {
            break;
        }
        if punct + 3 >= chars.len() {
            break;
        }
        let next3 = &chars[punct + 1..punct + 4];
        if let Some(cap_rel) = next3.iter().position(|c| c.is_ascii_uppercase()) {
            score += cfg.punct_capital_bonus;
            i = punct + 1 + cap_rel + 1;
        } else {
            score -= cfg.punct_capital_penalty;
            i = punct + 4;
        }
    }
    score
}

/// Is this char a word separator for the trigram scan? (whitespace or .,!?)
fn is_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '.' | ',' | '!' | '?')
}

/// Extract every leading-3-chars-of-a-word trigram from the (space-padded)
/// text, mirroring the game's word scanner.
fn get_all_trigrams(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut trigrams = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && is_separator(chars[i]) {
            i += 1;
        }
        if i < chars.len() {
            let end = (i + 3).min(chars.len());
            trigrams.push(chars[i..end].iter().collect());
            while i < chars.len() && !is_separator(chars[i]) {
                i += 1;
            }
        }
    }
    trigrams
}

/// The game's bugged trigram tables (North American / Australian bug: each
/// table is much longer than intended — a suffix is accepted even when it
/// falls in a later letter's section).
static TRIGRAMS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

fn trigram_lines() -> &'static [&'static str] {
    TRIGRAMS.get_or_init(|| {
        let data = include_str!("data/trigrams_bugged.txt");
        data.lines().collect()
    })
}

/// A loaded external trigram table: `first_letter` → the 2-char suffixes that
/// form a valid trigram. Unlike the embedded table (which replicates the game's
/// section-scan bug), an external table matches strictly within its own
/// letter's section — predictable for user-imported data.
#[derive(Debug, Clone, Default)]
pub struct TrigramTable {
    suffixes: std::collections::HashMap<char, std::collections::HashSet<String>>,
}

impl TrigramTable {
    /// Load a trigram table from a file. Supported formats (chosen by
    /// extension):
    ///
    /// - `.json` — either an array of full 3-char trigrams
    ///   `["the", "and", ...]`, or an object mapping a letter to its 2-char
    ///   suffixes `{"t": ["he", "hr"], ...}`.
    /// - `.csv` — rows of `letter,suffix` pairs, or full 3-char trigrams
    ///   (single column). A header row is skipped.
    /// - anything else (`.txt`, the game's own format) — `~X~` section
    ///   headers followed by one 2-char suffix per line.
    pub fn load(path: &str) -> Result<Self, String> {
        let data = std::fs::read_to_string(path)
            .map_err(|e| format!("read trigram table {}: {e}", path))?;
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".json") {
            Self::from_json(&data)
        } else if lower.ends_with(".csv") {
            Self::from_csv(&data)
        } else {
            Self::from_text(&data)
        }
    }

    /// Parse table content given a filename (used by tests without touching
    /// the filesystem).
    pub fn load_from_str(data: &str, filename: &str) -> Result<Self, String> {
        let lower = filename.to_ascii_lowercase();
        if lower.ends_with(".json") {
            Self::from_json(data)
        } else if lower.ends_with(".csv") {
            Self::from_csv(data)
        } else {
            Self::from_text(data)
        }
    }

    fn from_json(data: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(data).map_err(|e| format!("invalid JSON trigram table: {e}"))?;
        let mut table = Self::default();
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    let s = item.as_str().ok_or("JSON trigram table rows must be strings")?;
                    table.insert_trigram(s)?;
                }
            }
            serde_json::Value::Object(map) => {
                for (letter, suffixes) in map {
                    let letter = letter
                        .trim()
                        .chars()
                        .next()
                        .ok_or("empty letter key in JSON trigram table")?
                        .to_ascii_uppercase();
                    let suffixes = suffixes
                        .as_array()
                        .ok_or("JSON letter values must be arrays of suffixes")?;
                    for s in suffixes {
                        let suffix = s
                            .as_str()
                            .ok_or("JSON suffix values must be strings")?
                            .trim()
                            .to_string();
                        table.suffixes.entry(letter).or_default().insert(suffix);
                    }
                }
            }
            _ => return Err("JSON trigram table must be an array or object".to_string()),
        }
        Ok(table)
    }

    fn from_csv(data: &str) -> Result<Self, String> {
        let mut table = Self::default();
        for (i, line) in data.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(',').map(|p| p.trim()).collect();
            if parts.len() == 1 && i == 0 && parts[0].eq_ignore_ascii_case("trigram") {
                continue; // header row: "trigram"
            }
            if parts.len() == 2 && i == 0
                && parts[0].eq_ignore_ascii_case("letter")
                && parts[1].eq_ignore_ascii_case("suffix")
            {
                continue; // header row: "letter,suffix"
            }
            if parts.len() >= 2 {
                let letter = parts[0]
                    .chars()
                    .next()
                    .ok_or("empty letter in CSV trigram row")?
                    .to_ascii_uppercase();
                let suffix = parts[1].to_string();
                table.suffixes.entry(letter).or_default().insert(suffix);
            } else if let Some(tri) = parts.first() {
                table.insert_trigram(tri)?;
            }
        }
        Ok(table)
    }

    fn from_text(data: &str) -> Result<Self, String> {
        let mut table = Self::default();
        let mut current: Option<char> = None;
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('~') {
                current = line
                    .trim_matches('~')
                    .trim()
                    .chars()
                    .next()
                    .map(|c| c.to_ascii_uppercase());
                continue;
            }
            let Some(letter) = current else {
                continue;
            };
            table.suffixes.entry(letter).or_default().insert(line.to_string());
        }
        Ok(table)
    }

    fn insert_trigram(&mut self, trigram: &str) -> Result<(), String> {
        let trigram = trigram.trim();
        let chars: Vec<char> = trigram.chars().collect();
        if chars.len() != 3 {
            return Err(format!("trigram entries must be exactly 3 chars, got {trigram:?}"));
        }
        let letter = chars[0].to_ascii_uppercase();
        let suffix: String = chars[1..].iter().collect();
        self.suffixes.entry(letter).or_default().insert(suffix);
        Ok(())
    }

    fn contains(&self, first: char, suffix: &str) -> bool {
        self.suffixes
            .get(&first.to_ascii_uppercase())
            .map(|set| set.contains(suffix))
            .unwrap_or(false)
    }
}

/// External trigram tables are cached by path (they're read-only on disk; the
/// file could be huge and we must not re-parse it on every message).
static EXTERNAL_TRIGRAMS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<TrigramTable>>>,
> = std::sync::OnceLock::new();

fn external_trigram_table(path: &str) -> Option<std::sync::Arc<TrigramTable>> {
    let cache = EXTERNAL_TRIGRAMS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut cache = cache.lock().unwrap();
    if let Some(t) = cache.get(path) {
        return Some(t.clone());
    }
    match TrigramTable::load(path) {
        Ok(t) => {
            let t = std::sync::Arc::new(t);
            cache.insert(path.to_string(), t.clone());
            Some(t)
        }
        Err(e) => {
            eprintln!("[score-messages] failed to load trigram table {path}: {e}");
            None
        }
    }
}

/// Check B — Trigrams.
///
/// +`score` per valid trigram (the first 3 chars of each word). Uses the
/// embedded game table by default (with the NA/AU section-scan bug), or a
/// user-supplied external table when `trigram_table_path` is set.
fn check_b(input: &str, cfg: &Config) -> i32 {
    let mut padded = input.to_string();
    padded.push_str(&" ".repeat(cfg.max_chars.saturating_sub(padded.chars().count())));
    let all_trigrams = get_all_trigrams(&padded);

    let external = if cfg.trigram_table_path.trim().is_empty() {
        None
    } else {
        external_trigram_table(cfg.trigram_table_path.trim())
    };

    let mut counter = 0;
    for trigram in &all_trigrams {
        if trigram.chars().count() < 3 {
            break;
        }
        let first = trigram.chars().next().unwrap();
        let final_two: String = trigram.chars().skip(1).take(2).collect();
        let valid = match &external {
            Some(table) => table.contains(first, &final_two),
            None => {
                // Embedded bugged table: `~X~`-sectioned; the bug scans past
                // later `~` headers instead of stopping at the next section.
                let key = format!("~{}~", first.to_ascii_uppercase());
                let lines = trigram_lines();
                let Some(index) = lines.iter().position(|l| l.starts_with(&key)) else {
                    continue;
                };
                let mut matched = false;
                for line in &lines[index + 1..] {
                    // The bug: keep scanning past later section headers.
                    if line.starts_with('~') {
                        continue;
                    }
                    if line.contains(&final_two) {
                        matched = true;
                        break;
                    }
                }
                matched
            }
        };
        if valid {
            counter += 1;
        }
    }
    counter * cfg.trigram.score
}

/// Check C — Leading capital.
fn check_c(input: &str, cfg: &Config) -> i32 {
    match input.chars().find(|c| !c.is_whitespace()) {
        Some(c) if c.is_ascii_uppercase() => cfg.capital.score,
        Some(_) => -cfg.capital_penalty,
        None => 0,
    }
}

/// Check D — Repeating characters. −50 if any letter repeats 3+ times
/// sequentially (spaces removed first, per the game).
fn check_d(input: &str, cfg: &Config) -> i32 {
    let trimmed: String = input.trim_matches([' ', '\t']).chars().filter(|c| *c != ' ').collect();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut run = 0u32;
    let mut prev: Option<char> = None;
    for c in chars {
        if c.is_ascii_alphabetic() {
            if prev == Some(c) {
                run += 1;
            } else {
                run = 1;
                prev = Some(c);
            }
            if run >= cfg.repeating_threshold as u32 {
                return -cfg.repeating.score;
            }
        } else {
            run = 0;
            prev = None;
        }
    }
    0
}

/// Check E — Space ratio. +`space_ratio` if spaces are ≥ `space_ratio_pct`% of
/// non-space chars, else −`space_ratio_penalty`. `space_ratio_penalty` is
/// negative by default, so the else-branch is a small *reward* (`-(-5)` = +5):
/// short space-less messages like "gg" are encouraged, not punished.
fn check_e(input: &str, cfg: &Config) -> i32 {
    let num_spaces = input.chars().filter(|c| *c == ' ').count() as i32;
    let total = input.chars().count() as i32;
    let num_non = total - num_spaces;
    if num_non > 0 && (num_spaces * 100) / num_non >= cfg.space_ratio_pct {
        cfg.space_ratio.score
    } else {
        -cfg.space_ratio_penalty
    }
}

/// Check F — Run-on sentence. −150 if, after a punctuation mark, a run of
/// `run_on_chars`+ sequential chars has no punctuation.
fn check_f(input: &str, cfg: &Config) -> i32 {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    // The game only evaluates while more than `run_on_window` chars remain.
    while len.saturating_sub(i) > cfg.run_on_window as usize {
        if matches!(chars[i], '.' | '?' | '!') {
            let mut sentence_len = 0i32;
            i += 1;
            while i < len && !matches!(chars[i], '.' | '?' | '!') {
                sentence_len += 1;
                if sentence_len >= cfg.run_on_chars {
                    break;
                }
                i += 1;
            }
            if sentence_len >= cfg.run_on_chars {
                return -cfg.run_on.score;
            }
        }
        i += 1;
    }
    0
}

/// Check G — 32-char space groupings. −20 per 32-char group with no space.
fn check_g(input: &str, cfg: &Config) -> i32 {
    let trimmed: Vec<char> = input.trim_matches([' ', '\t']).chars().collect();
    let total = trimmed.len();
    let size = cfg.grouping_size.max(1);
    let groups = total / size;
    let mut score = 0;
    for g in 0..groups {
        let group = &trimmed[g * size..(g + 1) * size];
        if !group.contains(&' ') {
            score -= cfg.grouping.score;
        }
    }
    score
}

/// Check H — Spam & gibberish (chat-specific anti-exploit rule).
///
/// Punishes messages that look like exploitation rather than chat:
///   - **Emoji spam**: more than `spam_emoji_max` emoji in one message.
///   - **Symbol/morse spam**: `spam_symbol_ratio_pct`%+ of the non-space chars
///     are symbols (`.`, `-`, `#`, `|`, …) — the classic morse-code wall
///     (`.... . .-.. .-.. ---`) and symbol-spam patterns.
///   - **Gibberish**: a word of `spam_gibberish_word_min`+ letters whose vowel
///     share is below `spam_gibberish_vowel_pct`% (keyboard mash like
///     "shgekskshsjs").
///
/// Each detected signal contributes `spam.score` to the penalty, so a message
/// that stacks signals is punished proportionally harder. This is the counter
/// to the positive bias: normal chat is rewarded, exploitation is not.
fn check_h(input: &str, cfg: &Config) -> i32 {
    let mut penalty = 0i32;

    let emoji_count = input.chars().filter(|c| is_emoji(*c)).count() as i32;
    if emoji_count > cfg.spam_emoji_max {
        penalty += cfg.spam.score;
    }

    let non_space: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if !non_space.is_empty() {
        let symbols = non_space
            .iter()
            .filter(|c| !c.is_alphanumeric() && !is_emoji(**c))
            .count();
        let pct = (symbols as i32 * 100) / non_space.len() as i32;
        if pct >= cfg.spam_symbol_ratio_pct {
            penalty += cfg.spam.score;
        }
    }

    // Gibberish words: split on non-letters, check vowel share per word.
    let mut word = String::new();
    for c in input.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphabetic() {
            word.push(c);
        } else if !word.is_empty() {
            if word.chars().count() as i32 >= cfg.spam_gibberish_word_min {
                let vowels = word
                    .chars()
                    .filter(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
                    .count();
                let vowel_pct = (vowels as i32 * 100) / word.chars().count() as i32;
                if vowel_pct < cfg.spam_gibberish_vowel_pct {
                    penalty += cfg.spam.score;
                }
            }
            word.clear();
        }
    }

    -penalty
}

/// Score a message with the seven AC checks (plus the emoji and spam extras)
/// and return the total delta plus the rules that fired. Positive = reward,
/// negative = punishment.
pub fn score_message(message: &str, cfg: &Config) -> (i32, Vec<String>) {
    let mut delta = 0i32;
    let mut notes = Vec::new();

    if cfg.punctuation.toggle {
        let d = check_a(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("punctuation".into());
        }
    }
    if cfg.trigram.toggle {
        let d = check_b(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("trigram".into());
        }
    }
    if cfg.capital.toggle {
        let d = check_c(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("capital".into());
        }
    }
    if cfg.repeating.toggle {
        let d = check_d(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("repeating".into());
        }
    }
    if cfg.space_ratio.toggle {
        let d = check_e(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("space_ratio".into());
        }
    }
    if cfg.run_on.toggle {
        let d = check_f(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("run_on".into());
        }
    }
    if cfg.grouping.toggle {
        let d = check_g(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("grouping".into());
        }
    }
    if cfg.emoji.toggle && message.chars().any(is_emoji) {
        delta += cfg.emoji.score;
        notes.push("emoji".into());
    }
    if cfg.spam.toggle {
        let d = check_h(message, cfg);
        if d != 0 {
            delta += d;
            notes.push("spam".into());
        }
    }

    (delta, notes)
}

/// The viewership multiplier for a score delta: `max_viewers / viewers`,
/// clamped to `[1.0, viewership_multiplier_max]`. Fewer viewers → a higher
/// multiplier, so interaction on a small stream is rewarded more and a large
/// stream gets more breathing room. `viewers == 0` (channel offline) uses the
/// ceiling. Returns `1.0` when the viewership modifier is disabled.
pub fn viewership_multiplier(cfg: &Config, viewers: i32) -> f32 {
    if !cfg.viewership_toggle {
        return 1.0;
    }
    let max_viewers = cfg.viewership_max_viewers.max(1);
    let viewers = viewers.max(1); // 0/offline treated as the smallest audience
    let raw = max_viewers as f32 / viewers as f32;
    raw.clamp(1.0, cfg.viewership_multiplier_max.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_off() -> Config {
        let mut c = default_config();
        for r in [
            &mut c.punctuation,
            &mut c.trigram,
            &mut c.capital,
            &mut c.repeating,
            &mut c.space_ratio,
            &mut c.run_on,
            &mut c.grouping,
            &mut c.emoji,
            &mut c.spam,
        ] {
            r.toggle = false;
        }
        c
    }

    #[test]
    fn check_a_rewards_ending_punctuation_and_capitals_after() {
        let mut c = all_off();
        c.punctuation.toggle = true;
        // Ends with "!" and no capital after (nothing after): +30 only.
        let (delta, notes) = score_message("Hello there!", &c);
        assert_eq!(delta, 30);
        assert!(notes.contains(&"punctuation".to_string()));
        // Ends with "." and a capital within the next 3 chars after the "."
        let (delta, _) = score_message("Hello there. Welcome back.", &c);
        assert!(delta >= 30, "expected punctuation reward, got {delta}");
        // No punctuation: 0.
        let (delta, _) = score_message("hello there", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_b_counts_valid_trigrams() {
        let mut c = all_off();
        c.trigram.toggle = true;
        // "hello" → trigram "hel"; "th" is in the table section.
        let (delta, notes) = score_message("hello", &c);
        assert!(delta >= 3, "expected a trigram reward, got {delta}");
        assert!(notes.contains(&"trigram".to_string()));
        // A short word with an unlisted trigram scores 0 (or none valid).
        let (delta, _) = score_message("zz", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn external_trigram_table_loads_json_array() {
        let table = TrigramTable::load_from_str(
            r#"["the", "and", "hel"]"#,
            "table.json",
        )
        .unwrap();
        assert!(table.contains('t', "he"), "the -> t+he");
        assert!(table.contains('a', "nd"), "and -> a+nd");
        assert!(table.contains('h', "el"), "hel -> h+el");
        assert!(!table.contains('z', "zz"), "unlisted");
        // Case-insensitive first letter.
        assert!(table.contains('T', "he"));
    }

    #[test]
    fn external_trigram_table_loads_json_object() {
        let table = TrigramTable::load_from_str(
            r#"{"t": ["he", "hr"], "a": ["nd", "re"]}"#,
            "table.json",
        )
        .unwrap();
        assert!(table.contains('t', "he"));
        assert!(table.contains('t', "hr"));
        assert!(table.contains('a', "re"));
        assert!(!table.contains('t', "nd"));
    }

    #[test]
    fn external_trigram_table_loads_csv() {
        // Rows of letter,suffix plus a full-trigram column, with a header row.
        let table = TrigramTable::load_from_str(
            "letter,suffix\nt,he\na,nd\nthr\n",
            "table.csv",
        )
        .unwrap();
        assert!(table.contains('t', "he"));
        assert!(table.contains('a', "nd"));
        assert!(table.contains('t', "hr"), "thr -> t+hr");
        assert!(!table.contains('b', "ad"));
    }

    #[test]
    fn external_trigram_table_loads_text_sections() {
        let table = TrigramTable::load_from_str("~T~\nhe\nhr\n~A~\nnd\n", "table.txt")
            .unwrap();
        assert!(table.contains('t', "he"));
        assert!(table.contains('a', "nd"));
        assert!(!table.contains('t', "nd"), "strict per-letter matching");
    }

    #[test]
    fn external_trigram_table_drives_check_b() {
        // A tiny external table that recognises only "the" -> t+he.
        let path = "/tmp/cockatiel_probe_trigram_test.csv";
        std::fs::write(path, "letter,suffix\nt,he\n").unwrap();
        let mut c = all_off();
        c.trigram.toggle = true;
        c.trigram_table_path = path.to_string();
        // "the" word → trigram "the" → t+he → valid (trigram score 8).
        let (delta, notes) = score_message("the", &c);
        assert_eq!(delta, 8, "external table should reward 'the'");
        assert!(notes.contains(&"trigram".to_string()));
        // A word not in the tiny table scores 0.
        let (delta, _) = score_message("zzz", &c);
        assert_eq!(delta, 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bad_external_trigram_table_is_ignored_gracefully() {
        let path = "/tmp/cockatiel_probe_trigram_missing.json";
        let _ = std::fs::remove_file(path);
        let mut c = all_off();
        c.trigram.toggle = true;
        c.trigram_table_path = path.to_string();
        // A missing file means no external table: check_b falls back to the
        // embedded (bugged) table, so a known word still scores.
        let (delta, _) = score_message("hello", &c);
        assert!(delta >= 3, "missing external table falls back to embedded");
    }

    #[test]
    fn check_c_rewards_leading_capital() {
        let mut c = all_off();
        c.capital.toggle = true;
        let (delta, notes) = score_message("Hello", &c);
        assert_eq!(delta, 30);
        assert!(notes.contains(&"capital".to_string()));
        // Lowercase is never penalised (capital_penalty is 0).
        let (delta, _) = score_message("hello", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_d_punishes_repeating_chars() {
        let mut c = all_off();
        c.repeating.toggle = true;
        let (delta, notes) = score_message("aaaaaaa", &c);
        assert_eq!(delta, -50);
        assert!(notes.contains(&"repeating".to_string()));
        let (delta, _) = score_message("hello", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_e_rewards_good_space_ratio() {
        let mut c = all_off();
        c.space_ratio.toggle = true;
        // 6 spaces / 7 non-space = 85% >= 10% → +25.
        let (delta, _) = score_message("a b c d e f g", &c);
        assert_eq!(delta, 25);
        // 0 spaces → −space_ratio_penalty = −(−5) = +5. Short messages like
        // "gg" are rewarded, not punished.
        let (delta, _) = score_message("aaaaaaaaaaaa", &c);
        assert_eq!(delta, 5);
    }

    #[test]
    fn check_f_punishes_run_on() {
        let mut c = all_off();
        c.run_on.toggle = true;
        let long = "The stream was great. ".to_string() + &"a".repeat(80);
        let (delta, notes) = score_message(&long, &c);
        assert_eq!(delta, -150);
        assert!(notes.contains(&"run_on".to_string()));
    }

    #[test]
    fn check_g_punishes_32_char_groups_without_spaces() {
        let mut c = all_off();
        c.grouping.toggle = true;
        // 33 chars, no spaces: one full 32-char group → −20.
        let no_space = "a".repeat(33);
        let (delta, _) = score_message(&no_space, &c);
        assert_eq!(delta, -20);
        // 63 chars with a space in the first 32 but none in the second: only
        // ONE full 32-char group is checked (floor(63/32)=1), and it has a
        // space, so no penalty.
        let spaced = "a".repeat(31) + " " + &"b".repeat(31);
        let (delta, _) = score_message(&spaced, &c);
        assert_eq!(delta, 0);
        // 65 chars, space only in the first group: 2 full groups, second has no
        // space → −20.
        let spaced2 = "a".repeat(31) + " " + &"b".repeat(33);
        let (delta, _) = score_message(&spaced2, &c);
        assert_eq!(delta, -20);
        // A well-spaced short message: no group reaches 32 → 0.
        let (delta, _) = score_message("hi there", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn emoji_is_optional_extra() {
        let mut c = all_off();
        c.emoji.toggle = true;
        let (delta, notes) = score_message("nice stream 👍", &c);
        assert_eq!(delta, 15);
        assert!(notes.contains(&"emoji".to_string()));
    }

    #[test]
    fn flat_overlay_round_trips_tunables() {
        let cfg = default_config();
        let root = serde_json::json!({});
        let written = flat_map_for_write(&cfg, &root, &serde_json::Map::new());
        assert_eq!(written["run_on_chars"], 75);
        assert_eq!(written["grouping_size"], 32);
        assert_eq!(written["space_ratio_pct"], 10);
        assert_eq!(written["max_chars"], 192);
        assert_eq!(written["punct_capital_bonus"], 20);
        assert_eq!(written["punct_capital_penalty"], 0);
        assert_eq!(written["capital_penalty"], 0);
        assert_eq!(written["space_ratio_penalty"], -5);
        assert_eq!(written["repeating_threshold"], 3);
        assert_eq!(written["run_on_window"], 76);
        assert_eq!(written["prompt_timeout_secs"], 60);
        assert_eq!(written["viewership_toggle"], false);
        assert_eq!(written["viewership_max_viewers"], 1000);
        assert_eq!(written["viewership_multiplier_max"], 5.0);
        assert_eq!(written["viewership_poll_secs"], 30);
        let restored = apply_flat_overlay(default_config(), &written);
        assert_eq!(restored.run_on_chars, 75);
        assert_eq!(restored.grouping_size, 32);
        assert_eq!(restored.space_ratio_pct, 10);
        assert_eq!(restored.max_chars, 192);
        assert_eq!(restored.punct_capital_bonus, 20);
        assert_eq!(restored.capital_penalty, 0);
        assert_eq!(restored.space_ratio_penalty, -5);
        assert_eq!(restored.repeating_threshold, 3);
        assert_eq!(restored.run_on_window, 76);
        assert!(!restored.viewership_toggle);
        assert_eq!(restored.viewership_max_viewers, 1000);
        assert_eq!(restored.viewership_multiplier_max, 5.0);
        assert_eq!(restored.viewership_poll_secs, 30);
        // Overrides survive a round trip.
        let mut overridden = written.clone();
        overridden.insert("run_on_chars".to_string(), serde_json::json!(60));
        overridden.insert("space_ratio_pct".to_string(), serde_json::json!(30));
        overridden.insert("punct_capital_bonus".to_string(), serde_json::json!(25));
        overridden.insert("repeating_threshold".to_string(), serde_json::json!(5));
        overridden.insert("viewership_toggle".to_string(), serde_json::json!(true));
        overridden.insert("viewership_max_viewers".to_string(), serde_json::json!(500));
        overridden.insert("viewership_multiplier_max".to_string(), serde_json::json!(2.5));
        let restored = apply_flat_overlay(default_config(), &overridden);
        assert_eq!(restored.run_on_chars, 60);
        assert_eq!(restored.space_ratio_pct, 30);
        assert_eq!(restored.punct_capital_bonus, 25);
        assert_eq!(restored.repeating_threshold, 5);
        assert!(restored.viewership_toggle);
        assert_eq!(restored.viewership_max_viewers, 500);
        assert_eq!(restored.viewership_multiplier_max, 2.5);
    }

    #[test]
    fn tunables_drive_scoring() {
        let mut c = all_off();
        // Lower the space-ratio bar: 1 space in 20 chars (5%) now earns +25.
        c.space_ratio.toggle = true;
        c.space_ratio_pct = 5;
        let (delta, notes) = score_message("a bbbbbbbbbbbbbbbbbbb", &c);
        assert_eq!(delta, 25);
        assert!(notes.contains(&"space_ratio".to_string()));
        // Raise the grouping window to 8 so "hello world" (11 chars) hits a group.
        let mut g = all_off();
        g.grouping.toggle = true;
        g.grouping_size = 8;
        let (delta, _) = score_message("hello world", &g);
        assert_eq!(delta, 0); // first 8 chars contain a space → no penalty
        let (delta, _) = score_message("helloworld", &g);
        assert_eq!(delta, -20);
        // Raise the repeating threshold: "aaa" (3 repeats) is now allowed.
        let mut r = all_off();
        r.repeating.toggle = true;
        r.repeating_threshold = 4;
        let (delta, _) = score_message("aaa", &r);
        assert_eq!(delta, 0, "3 repeats under a threshold of 4 should pass");
        let (delta, _) = score_message("aaaa", &r);
        assert_eq!(delta, -50);
        // The capital penalty is independently tunable from the reward.
        let mut cp = all_off();
        cp.capital.toggle = true;
        cp.capital_penalty = 3;
        let (delta, _) = score_message("hello", &cp);
        assert_eq!(delta, -3);
    }

    #[test]
    fn viewership_multiplier_is_disabled_by_default() {
        let cfg = default_config();
        assert_eq!(viewership_multiplier(&cfg, 0), 1.0);
        assert_eq!(viewership_multiplier(&cfg, 5000), 1.0);
    }

    #[test]
    fn viewership_multiplier_rewards_small_streams() {
        let mut cfg = default_config();
        cfg.viewership_toggle = true;
        cfg.viewership_max_viewers = 1000;
        cfg.viewership_multiplier_max = 5.0;
        // At "full house" the multiplier is 1.0.
        assert_eq!(viewership_multiplier(&cfg, 1000), 1.0);
        // Half the audience → 2x.
        assert_eq!(viewership_multiplier(&cfg, 500), 2.0);
        // A tenth of the audience → 10x, clamped to the ceiling of 5.
        assert_eq!(viewership_multiplier(&cfg, 100), 5.0);
        // Offline (0 viewers) → the ceiling.
        assert_eq!(viewership_multiplier(&cfg, 0), 5.0);
    }

    #[test]
    fn viewership_multiplier_clamps_and_respects_ceiling() {
        let mut cfg = default_config();
        cfg.viewership_toggle = true;
        cfg.viewership_max_viewers = 2000;
        cfg.viewership_multiplier_max = 3.0;
        // More viewers than max → the multiplier never dips below 1.0.
        assert_eq!(viewership_multiplier(&cfg, 9000), 1.0);
        // Large audience still clamps to the ceiling at the low end.
        assert_eq!(viewership_multiplier(&cfg, 2000), 1.0);
        // A small audience clamps to the configured ceiling, not an unlimited one.
        assert_eq!(viewership_multiplier(&cfg, 1), 3.0);
        // Negative viewers (defensive) behave like offline → ceiling.
        assert_eq!(viewership_multiplier(&cfg, -5), 3.0);
    }

    #[test]
    fn check_h_punishes_emoji_spam() {
        let mut c = all_off();
        c.spam.toggle = true;
        // 6 emoji > spam_emoji_max (5) → −100 (symbol wall excludes emoji, so
        // only the emoji-spam signal fires).
        let (delta, notes) = score_message("🔥🔥🔥🔥🔥🔥", &c);
        assert_eq!(delta, -100);
        assert!(notes.contains(&"spam".to_string()));
        // A couple of emoji in real chat is fine (not > max, low symbol share).
        let (delta, _) = score_message("gg wp 👍👏", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_h_punishes_morse_symbol_walls() {
        let mut c = all_off();
        c.spam.toggle = true;
        // Morse: 100% of non-space chars are symbols → −100.
        let (delta, notes) = score_message(".... . .-.. .-.. ---", &c);
        assert_eq!(delta, -100);
        assert!(notes.contains(&"spam".to_string()));
        // Normal punctuation is below the 30% symbol threshold.
        let (delta, _) = score_message("hello world!", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_h_punishes_gibberish_words() {
        let mut c = all_off();
        c.spam.toggle = true;
        // Keyboard mash: 10+ letters, vowel share < 25% → −100.
        let (delta, notes) = score_message("shgekskshsjs", &c);
        assert_eq!(delta, -100);
        assert!(notes.contains(&"spam".to_string()));
        // A real long word with normal vowels is not gibberish.
        let (delta, _) = score_message("watermelon", &c);
        assert_eq!(delta, 0);
    }

    #[test]
    fn check_h_stacks_signals() {
        let mut c = all_off();
        c.spam.toggle = true;
        // Morse wall + emoji spam: both signals → −200.
        let (delta, notes) = score_message(".... . .-.. .-.. --- 👍👍👍👍👍👍", &c);
        assert_eq!(delta, -200);
        assert!(notes.contains(&"spam".to_string()));
    }

    #[test]
    fn spam_overlay_round_trips() {
        let cfg = default_config();
        let written = flat_map_for_write(&cfg, &serde_json::json!({}), &serde_json::Map::new());
        assert_eq!(written["spam_score"], 100);
        assert_eq!(written["spam_emoji_max"], 5);
        assert_eq!(written["spam_symbol_ratio_pct"], 30);
        assert_eq!(written["spam_gibberish_word_min"], 8);
        assert_eq!(written["spam_gibberish_vowel_pct"], 25);
        let mut overridden = written.clone();
        overridden.insert("spam_score".to_string(), serde_json::json!(80));
        overridden.insert("spam_emoji_max".to_string(), serde_json::json!(3));
        let restored = apply_flat_overlay(default_config(), &overridden);
        assert_eq!(restored.spam.score, 80);
        assert_eq!(restored.spam_emoji_max, 3);
    }
}