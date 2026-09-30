//! The pure scoring core for the score-messages module.
//!
//! Scoring follows the Animal Crossing (GCN) letter-scoring algorithm (see
//! https://hunter-r.com/ac-letter-scorer/ and
//! https://github.com/HunterRDev/AC-Letter-Scorer), ported to chat messages.
//! A message is scored through seven checks (A–G) whose points sum to the
//! delta applied to the user's score:
//!
//!   A. Punctuation        — +20 if the text ends with `.`/`!`/`?` (and is
//!                           under the 192-char max); then for every
//!                           punctuation mark, +10 if a capital letter appears
//!                           within the next 3 chars, else −10.
//!   B. Trigrams           — +3 per valid leading-3-chars-of-each-word
//!                           trigram found in the game's (bugged) trigram
//!                           tables.
//!   C. Leading capital    — +20 if the first non-space char is a capital,
//!                           else −10.
//!   D. Repeating chars    — −50 if any letter is repeated 3+ times
//!                           sequentially (spaces ignored).
//!   E. Space ratio        — +20 if spaces are ≥ 20% of non-space chars,
//!                           else −20.
//!   F. Run-on sentence    — −150 if a run of 75+ chars has no punctuation
//!                           after a punctuation mark.
//!   G. 32-char groupings  — −20 per 32-char group that contains no space.
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
pub const RUN_ON_CHARS: usize = 75;

/// The 32-char space-grouping window.
pub const GROUPING_SIZE: usize = 32;

/// The space-ratio threshold (spaces ÷ non-spaces, ×100).
pub const SPACE_RATIO_PERCENT: i64 = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub toggle: bool,
    pub score: i64,
}

impl Default for Rule {
    fn default() -> Self {
        Self { toggle: true, score: 20 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrequencyRule {
    pub toggle: bool,
    pub score: i64,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
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
pub struct Config {
    // ── The seven AC letter checks ──────────────────────────────────────
    /// Check A: punctuation.
    #[serde(default)]
    pub punctuation: Rule,
    /// Check B: valid trigrams.
    #[serde(default = "default_trigram")]
    pub trigram: Rule,
    /// Check C: leading capital.
    #[serde(default = "default_capital")]
    pub capital: Rule,
    /// Check D: repeating characters.
    #[serde(default = "default_repeating")]
    pub repeating: Rule,
    /// Check E: space ratio.
    #[serde(default = "default_space_ratio")]
    pub space_ratio: Rule,
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
    /// Punish posting again too soon after a user's last message.
    #[serde(default)]
    pub frequency: FrequencyRule,
    // ── Tunable scalars (defaults match the game's algorithm) ───────────
    #[serde(default = "default_run_on_chars")]
    pub run_on_chars: usize,
    #[serde(default = "default_grouping_size")]
    pub grouping_size: usize,
    #[serde(default = "default_space_ratio_pct")]
    pub space_ratio_pct: i64,
    /// Check A: the letter-body ceiling under which the ending-punctuation
    /// reward is granted (and the trigram padding length).
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Check A: points per punctuation mark that is followed (within 3 chars)
    /// by a capital letter.
    #[serde(default = "default_punct_capital_bonus")]
    pub punct_capital_bonus: i64,
    /// Check A: points per punctuation mark NOT followed (within 3 chars) by a
    /// capital letter.
    #[serde(default = "default_punct_capital_penalty")]
    pub punct_capital_penalty: i64,
    /// Check C: the penalty when the leading non-space char is not a capital.
    #[serde(default = "default_capital_penalty")]
    pub capital_penalty: i64,
    /// Check D: how many sequential repeats of a letter trigger the penalty.
    #[serde(default = "default_repeating_threshold")]
    pub repeating_threshold: usize,
    /// Check F: the run-on scan only evaluates while more than this many chars
    /// remain in the message.
    #[serde(default = "default_run_on_window")]
    pub run_on_window: usize,
    #[serde(default = "default_frequency_interval_floor_secs")]
    pub frequency_interval_floor_secs: u64,
    #[serde(default = "default_last_msg_cap")]
    pub last_msg_cap: usize,
    #[serde(default = "default_prompt_timeout_secs")]
    pub prompt_timeout_secs: u32,
    #[serde(default = "default_reconnect_base_secs")]
    pub reconnect_base_secs: u64,
    #[serde(default = "default_reconnect_max_secs")]
    pub reconnect_max_secs: u64,
}

fn default_trigram() -> Rule {
    Rule { toggle: true, score: 3 }
}
fn default_capital() -> Rule {
    Rule { toggle: true, score: 20 }
}
fn default_repeating() -> Rule {
    Rule { toggle: true, score: 50 }
}
fn default_space_ratio() -> Rule {
    Rule { toggle: true, score: 20 }
}
fn default_run_on() -> Rule {
    Rule { toggle: true, score: 150 }
}
fn default_grouping() -> Rule {
    Rule { toggle: true, score: 20 }
}
fn default_emoji() -> Rule {
    Rule { toggle: true, score: 1 }
}

pub fn default_run_on_chars() -> usize {
    RUN_ON_CHARS
}
pub fn default_grouping_size() -> usize {
    GROUPING_SIZE
}
pub fn default_max_chars() -> usize {
    MAX_CHARS
}
pub fn default_punct_capital_bonus() -> i64 {
    10
}
pub fn default_punct_capital_penalty() -> i64 {
    10
}
pub fn default_capital_penalty() -> i64 {
    10
}
pub fn default_repeating_threshold() -> usize {
    3
}
pub fn default_run_on_window() -> usize {
    76
}
pub fn default_space_ratio_pct() -> i64 {
    SPACE_RATIO_PERCENT
}
pub fn default_frequency_interval_floor_secs() -> u64 {
    1
}
pub fn default_last_msg_cap() -> usize {
    10_000
}
pub fn default_prompt_timeout_secs() -> u32 {
    60
}
pub fn default_reconnect_base_secs() -> u64 {
    1
}
pub fn default_reconnect_max_secs() -> u64 {
    30
}

pub fn default_config() -> Config {
    Config {
        punctuation: Rule { toggle: true, score: 20 },
        trigram: default_trigram(),
        capital: default_capital(),
        repeating: default_repeating(),
        space_ratio: default_space_ratio(),
        run_on: default_run_on(),
        grouping: default_grouping(),
        emoji: default_emoji(),
        frequency: FrequencyRule::default(),
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
                cfg.$field.score = s;
            }
        };
    }
    apply_rule!(punctuation, "punctuation");
    apply_rule!(trigram, "trigram");
    apply_rule!(capital, "capital");
    apply_rule!(repeating, "repeating");
    apply_rule!(space_ratio, "space_ratio");
    apply_rule!(run_on, "run_on");
    apply_rule!(grouping, "grouping");
    apply_rule!(emoji, "emoji");
    apply_rule!(frequency, "frequency");
    if let Some(i) = parse_u64(ms.get("frequency_interval_secs")) {
        cfg.frequency.interval_secs = i;
    }
    if let Some(v) = parse_usize(ms.get("run_on_chars")) {
        cfg.run_on_chars = v;
    }
    if let Some(v) = parse_usize(ms.get("grouping_size")) {
        cfg.grouping_size = v;
    }
    if let Some(v) = parse_i64(ms.get("space_ratio_pct")) {
        cfg.space_ratio_pct = v;
    }
    if let Some(v) = parse_usize(ms.get("max_chars")) {
        cfg.max_chars = v;
    }
    if let Some(v) = parse_i64(ms.get("punct_capital_bonus")) {
        cfg.punct_capital_bonus = v;
    }
    if let Some(v) = parse_i64(ms.get("punct_capital_penalty")) {
        cfg.punct_capital_penalty = v;
    }
    if let Some(v) = parse_i64(ms.get("capital_penalty")) {
        cfg.capital_penalty = v;
    }
    if let Some(v) = parse_usize(ms.get("repeating_threshold")) {
        cfg.repeating_threshold = v;
    }
    if let Some(v) = parse_usize(ms.get("run_on_window")) {
        cfg.run_on_window = v;
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
            let score = parse_score(ms.get(score_key)).unwrap_or(cfg.$field.score);
            out.insert(toggle_key.to_string(), serde_json::Value::Bool(toggle));
            out.insert(score_key.to_string(), serde_json::json!(score));
        };
    }
    put_rule!(punctuation, "punctuation");
    put_rule!(trigram, "trigram");
    put_rule!(capital, "capital");
    put_rule!(repeating, "repeating");
    put_rule!(space_ratio, "space_ratio");
    put_rule!(run_on, "run_on");
    put_rule!(grouping, "grouping");
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
    put_scalar!("run_on_chars", parse_usize, cfg.run_on_chars);
    put_scalar!("grouping_size", parse_usize, cfg.grouping_size);
    put_scalar!("space_ratio_pct", parse_i64, cfg.space_ratio_pct);
    put_scalar!("max_chars", parse_usize, cfg.max_chars);
    put_scalar!("punct_capital_bonus", parse_i64, cfg.punct_capital_bonus);
    put_scalar!("punct_capital_penalty", parse_i64, cfg.punct_capital_penalty);
    put_scalar!("capital_penalty", parse_i64, cfg.capital_penalty);
    put_scalar!("repeating_threshold", parse_usize, cfg.repeating_threshold);
    put_scalar!("run_on_window", parse_usize, cfg.run_on_window);
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
fn check_a(input: &str, cfg: &Config) -> i64 {
    let trimmed: String = input.trim_matches([' ', '\t']).to_string();
    let mut score = 0;

    if trimmed.chars().count() < cfg.max_chars && matches!(trimmed.chars().last(), Some('.') | Some('!') | Some('?')) {
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

/// Check B — Trigrams.
///
/// +`score` per valid trigram (the first 3 chars of each word) found in the
/// bugged trigram tables. The table is `~X~`-sectioned with 2-char suffixes;
/// the bug `continue`s past later `~` headers (a `break` would fix it).
fn check_b(input: &str, cfg: &Config) -> i64 {
    let mut padded = input.to_string();
    padded.push_str(&" ".repeat(cfg.max_chars.saturating_sub(padded.chars().count())));
    let all_trigrams = get_all_trigrams(&padded);
    let lines = trigram_lines();

    let mut counter = 0;
    for trigram in &all_trigrams {
        if trigram.chars().count() < 3 {
            break;
        }
        let first = trigram.chars().next().unwrap().to_ascii_uppercase();
        let key = format!("~{first}~");
        let Some(index) = lines.iter().position(|l| l.starts_with(&key)) else {
            continue;
        };
        let final_two: String = trigram.chars().skip(1).take(2).collect();
        for line in &lines[index + 1..] {
            // The bug: continue past later section headers instead of breaking.
            if line.starts_with('~') {
                continue;
            }
            if line.contains(&final_two) {
                counter += 1;
                break;
            }
        }
    }
    counter * cfg.trigram.score
}

/// Check C — Leading capital.
fn check_c(input: &str, cfg: &Config) -> i64 {
    match input.chars().find(|c| !c.is_whitespace()) {
        Some(c) if c.is_ascii_uppercase() => cfg.capital.score,
        Some(_) => -cfg.capital_penalty,
        None => 0,
    }
}

/// Check D — Repeating characters. −50 if any letter repeats 3+ times
/// sequentially (spaces removed first, per the game).
fn check_d(input: &str, cfg: &Config) -> i64 {
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

/// Check E — Space ratio. +20 if spaces are ≥ 20% of non-space chars, else −20.
fn check_e(input: &str, cfg: &Config) -> i64 {
    let num_spaces = input.chars().filter(|c| *c == ' ').count() as i64;
    let total = input.chars().count() as i64;
    let num_non = total - num_spaces;
    if num_non > 0 && (num_spaces * 100) / num_non >= cfg.space_ratio_pct {
        cfg.space_ratio.score
    } else {
        -cfg.space_ratio.score
    }
}

/// Check F — Run-on sentence. −150 if, after a punctuation mark, a run of
/// `run_on_chars`+ sequential chars has no punctuation.
fn check_f(input: &str, cfg: &Config) -> i64 {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    // The game only evaluates while more than `run_on_window` chars remain.
    while len.saturating_sub(i) > cfg.run_on_window {
        if matches!(chars[i], '.' | '?' | '!') {
            let mut sentence_len = 0usize;
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
fn check_g(input: &str, cfg: &Config) -> i64 {
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

/// Score a message with the seven AC checks (plus the emoji extra) and return
/// the total delta plus the rules that fired. Positive = reward, negative =
/// punishment.
pub fn score_message(message: &str, cfg: &Config) -> (i64, Vec<String>) {
    let mut delta = 0i64;
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

    (delta, notes)
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
        ] {
            r.toggle = false;
        }
        c
    }

    #[test]
    fn check_a_rewards_ending_punctuation_and_capitals_after() {
        let mut c = all_off();
        c.punctuation.toggle = true;
        // Ends with "!" and no capital after (nothing after): +20 only.
        let (delta, notes) = score_message("Hello there!", &c);
        assert_eq!(delta, 20);
        assert!(notes.contains(&"punctuation".to_string()));
        // Ends with "." and a capital within the next 3 chars after the "."
        let (delta, _) = score_message("Hello there. Welcome back.", &c);
        assert!(delta >= 20, "expected punctuation reward, got {delta}");
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
    fn check_c_rewards_leading_capital() {
        let mut c = all_off();
        c.capital.toggle = true;
        let (delta, notes) = score_message("Hello", &c);
        assert_eq!(delta, 20);
        assert!(notes.contains(&"capital".to_string()));
        let (delta, _) = score_message("hello", &c);
        assert_eq!(delta, -10);
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
        // 5 spaces / 20 non-space = 25% >= 20% → +20.
        let (delta, _) = score_message("a b c d e f g", &c);
        assert_eq!(delta, 20);
        // 0 spaces → −20.
        let (delta, _) = score_message("aaaaaaaaaaaa", &c);
        assert_eq!(delta, -20);
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
        assert_eq!(delta, 1);
        assert!(notes.contains(&"emoji".to_string()));
    }

    #[test]
    fn flat_overlay_round_trips_tunables() {
        let cfg = default_config();
        let root = serde_json::json!({});
        let written = flat_map_for_write(&cfg, &root, &serde_json::Map::new());
        assert_eq!(written["run_on_chars"], 75);
        assert_eq!(written["grouping_size"], 32);
        assert_eq!(written["space_ratio_pct"], 20);
        assert_eq!(written["max_chars"], 192);
        assert_eq!(written["punct_capital_bonus"], 10);
        assert_eq!(written["punct_capital_penalty"], 10);
        assert_eq!(written["capital_penalty"], 10);
        assert_eq!(written["repeating_threshold"], 3);
        assert_eq!(written["run_on_window"], 76);
        assert_eq!(written["prompt_timeout_secs"], 60);
        let restored = apply_flat_overlay(default_config(), &written);
        assert_eq!(restored.run_on_chars, 75);
        assert_eq!(restored.grouping_size, 32);
        assert_eq!(restored.space_ratio_pct, 20);
        assert_eq!(restored.max_chars, 192);
        assert_eq!(restored.punct_capital_bonus, 10);
        assert_eq!(restored.capital_penalty, 10);
        assert_eq!(restored.repeating_threshold, 3);
        assert_eq!(restored.run_on_window, 76);
        // Overrides survive a round trip.
        let mut overridden = written.clone();
        overridden.insert("run_on_chars".to_string(), serde_json::json!(60));
        overridden.insert("space_ratio_pct".to_string(), serde_json::json!(30));
        overridden.insert("punct_capital_bonus".to_string(), serde_json::json!(25));
        overridden.insert("repeating_threshold".to_string(), serde_json::json!(5));
        let restored = apply_flat_overlay(default_config(), &overridden);
        assert_eq!(restored.run_on_chars, 60);
        assert_eq!(restored.space_ratio_pct, 30);
        assert_eq!(restored.punct_capital_bonus, 25);
        assert_eq!(restored.repeating_threshold, 5);
    }

    #[test]
    fn tunables_drive_scoring() {
        let mut c = all_off();
        // Lower the space-ratio bar: 1 space in 20 chars (5%) now earns +20.
        c.space_ratio.toggle = true;
        c.space_ratio_pct = 5;
        let (delta, notes) = score_message("a bbbbbbbbbbbbbbbbbbb", &c);
        assert_eq!(delta, 20);
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
}