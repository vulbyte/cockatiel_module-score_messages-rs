//! The pure scoring core for the score-messages module.
//!
//! Everything here is a deterministic function of its inputs (a message + a
//! config), with no network, no state, and no side effects — which is exactly
//! what makes it unit-testable and, crucially, **probeable**: the tuning probe
//! (`src/bin/score_probe.rs`) runs real chat sentences through this same code
//! to visualise how rule weights shape the distribution of message scores.
//!
//! The live module (`main.rs`) calls the same functions, so a tuning decision
//! made against the probe is guaranteed to match production behaviour.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub toggle: bool,
    pub score: i64,
}

impl Default for Rule {
    fn default() -> Self {
        Self { toggle: true, score: 1 }
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
    #[serde(default)]
    pub punctuation: Rule,
    #[serde(default)]
    pub question: Rule,
    #[serde(default)]
    pub length: Rule,
    #[serde(default)]
    pub spam: Rule,
    #[serde(default)]
    pub no_spacing: Rule,
    #[serde(default)]
    pub wordless: Rule,
    #[serde(default)]
    pub emoji: Rule,
    #[serde(default)]
    pub frequency: FrequencyRule,
    #[serde(default = "default_length_min_chars")]
    pub length_min_chars: i64,
    #[serde(default = "default_no_spacing_min_len")]
    pub no_spacing_min_len: i64,
    #[serde(default = "default_spam_min_token_len")]
    pub spam_min_token_len: i64,
    #[serde(default = "default_keyboard_mash_min_len")]
    pub keyboard_mash_min_len: i64,
    #[serde(default = "default_wordless_vowel_ratio")]
    pub wordless_vowel_ratio: f64,
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

pub fn default_length_min_chars() -> i64 {
    40
}

pub fn default_no_spacing_min_len() -> i64 {
    20
}

pub fn default_spam_min_token_len() -> i64 {
    4
}

pub fn default_keyboard_mash_min_len() -> i64 {
    5
}

pub fn default_wordless_vowel_ratio() -> f64 {
    0.5
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

/// Overlay the engine's flat module_specific credential keys onto a Config,
/// falling back to the given legacy values for any key that is absent or
/// unparseable.
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
pub fn flat_map_for_write(
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

/// Resolve a Config from a config.json root the same way the live module does:
/// flat `module_specific` overlay first, legacy nested top-level Config second,
/// code defaults last. Used by both `main.rs` and the tuning probe so they
/// always agree on the effective config.
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

/// Is this token made of repeated characters (spam: bbbbb, asdlfkjsqaldkfja)?
pub fn looks_like_spam_token(token: &str, min_token_len: i64, mash_min_len: i64) -> bool {
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

pub fn is_wordless(message: &str, vowel_ratio: f64) -> bool {
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

/// Score a message and return the total delta plus the rules that fired.
/// Positive = reward, negative = punishment.
pub fn score_message(message: &str, cfg: &Config) -> (i64, Vec<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    pub fn all_off() -> Config {
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
}