//! Score tuning probe — an offline dev tool for the score-messages rules.
//!
//! Runs a corpus of real chat sentences through the EXACT production scoring
//! function (`score_messages_rs::score_message`) and visualises how the rule
//! weights shape the distribution of message scores. The whole point: make a
//! scoring tweak, re-run the probe, and SEE how the high/low/average and the
//! per-rule contribution shift — so tuning is grounded in data, not vibes.
//!
//! Usage (from the repo root):
//!   cargo run --release --features probe --bin score_probe
//!   cargo run --release --features probe --bin score_probe -- --config path/to/config.json
//!   cargo run --release --features probe --bin score_probe -- --diff other/config.json
//!   cargo run --release --features probe --bin score_probe -- --corpus my-chat.txt
//!
//! Output: a terminal ASCII histogram + per-rule breakdown, and an SVG chart
//! (`score_distribution.svg`) written to the current directory.

use std::collections::BTreeMap;
use std::path::Path;

use plotters::prelude::*;
use score_messages_rs::{config_from_root, default_config, score_message, Config};

/// One corpus sentence tagged with the rule it is meant to exercise. A sentence
/// may fire several rules; the tag is the *primary* intent so the per-rule
/// breakdown has a meaningful denominator.
struct CorpusEntry {
    rule: &'static str,
    text: &'static str,
}

/// Builds one corpus entry (keeps the array readable). Upper-case by design to
/// read like a tag at every call site.
#[allow(non_snake_case)]
const fn Entry(rule: &'static str, text: &'static str) -> CorpusEntry {
    CorpusEntry { rule, text }
}

/// 100+ curated sentences covering every rule and the neutral baseline.
const CORPUS: &[CorpusEntry] = &[
    // Neutral — plain messages, no rule fires.
    Entry("neutral", "hey"),
    Entry("neutral", "yo"),
    Entry("neutral", "hi"),
    Entry("wordless", "gg"),
    Entry("neutral", "lol"),
    Entry("neutral", "nice"),
    Entry("neutral", "cool"),
    Entry("neutral", "wow"),
    Entry("neutral", "ok"),
    Entry("neutral", "sick"),
    Entry("neutral", "pog"),
    Entry("neutral", "hi chat"),
    Entry("neutral", "good game"),
    Entry("neutral", "lets go"),
    Entry("neutral", "well played"),
    // Punctuation — end with . or !.
    Entry("punctuation", "hello world."),
    Entry("punctuation", "good morning!"),
    Entry("punctuation", "what a play."),
    Entry("punctuation", "see you tomorrow."),
    Entry("punctuation", "have a good one!"),
    Entry("punctuation", "this is great."),
    Entry("punctuation", "that was wild!"),
    Entry("punctuation", "cant wait for the next one."),
    Entry("punctuation", "you are doing awesome!"),
    Entry("punctuation", "the stream is popping off."),
    Entry("punctuation", "im actually enjoying this a lot."),
    Entry("punctuation", "that clip is going to be legendary."),
    // Question — end with ? (also fires punctuation).
    Entry("question", "how are you?"),
    Entry("question", "any plans tonight?"),
    Entry("question", "when does the raid start?"),
    Entry("question", "can we get a round of applause?"),
    Entry("question", "did you see that clutch?"),
    Entry("question", "who is winning right now?"),
    Entry("question", "whats the music playing in the background?"),
    Entry("question", "will you play this game again tomorrow?"),
    Entry("question", "how long have you been streaming for?"),
    Entry("question", "are we doing the giveaway after this game?"),
    // Length — 40+ chars.
    Entry("length", "this is a fairly long message that definitely exceeds forty characters by a bit"),
    Entry("length", "just wanted to say that this has been one of the most fun streams i have watched in a while now"),
    Entry("length", "the way you explained that mechanic really helped me understand the game much better than before"),
    Entry("length", "i have been lurking for a while but this is genuinely the first time i felt like chatting in the stream"),
    Entry("length", "your consistency with streaming at the same time every day is honestly really impressive to me"),
    Entry("length", "that play at the end of the match was so clean it made my jaw drop for a solid couple of seconds"),
    Entry("length", "the community here seems so welcoming compared to some of the other channels i have visited lately"),
    Entry("length", "hopefully we get to see you try out that new indie game soon because it looks right up your alley"),
    Entry("length", "i appreciate how you actually read chat and answer questions instead of just ignoring everyone"),
    Entry("length", "this has to be one of the best comebacks i have seen in competitive play in quite a long time"),
    // Long + punctuation (two rewards).
    Entry("length", "that was an absolutely incredible comeback and i am so glad i stayed up to watch the whole thing!"),
    Entry("length", "the patience you showed while explaining the build order to the new players in chat was great."),
    // Spam — repeated chars / keyboard mash.
    Entry("spam", "aaaaaaaaaa"),
    Entry("spam", "bbbbbbbbbbbb"),
    Entry("spam", "ccccccccccccccc"),
    Entry("spam", "ddddddddddddddd"),
    Entry("spam", "eeeeeeeeeeeeeeee"),
    Entry("spam", "ffffffffffff"),
    Entry("spam", "ggggggggggggg"),
    Entry("spam", "hhhhhhhhhhhhhh"),
    Entry("spam", "jjjjjjjjjjj"),
    Entry("spam", "kkkkkkkkkkkkk"),
    Entry("spam", "asdfghjklqwertyuiopzxcvbnm"),
    Entry("spam", "qwertyuiopasdfghjkl"),
    Entry("spam", "zxcvbnmasdfghjklqwertyuiop"),
    Entry("spam", "poiuytrewqlkjhgfdsa"),
    Entry("spam", "mnbvcxzlkjhgfdsapoiuytrewq"),
    // No-spacing — long message, zero spaces.
    Entry("no_spacing", "thisisareallylongmessagewithnospaceswhatsoeverintheentirething"),
    Entry("no_spacing", "cantbelieveiwrotethiswithnospacesatallthatscrazy"),
    Entry("no_spacing", "nospacesinthelongmessageatyalljustonegiantrunonsentence"),
    Entry("no_spacing", "anothergreatexampleofamessagethathasabsolutelynospaces"),
    Entry("no_spacing", "thestreamisabsolutelyonfiretonightwithzerochanceofslowingdown"),
    Entry("no_spacing", "iknowthisisawholerunonmessagebutitstillcountsaschatigues"),
    Entry("no_spacing", "notasinglespaceistobefoundinthisentirelyunbrokenstringoftext"),
    Entry("no_spacing", "chatmoveoverbecauseiamabouttotypethelongestwordeverwithoutanybreaks"),
    Entry("no_spacing", "ifeltlikewritingamessagewithnospacesjusttoseehowitwouldbescored"),
    Entry("no_spacing", "definitelynotusinganyspacesinthisoneforthesakeoftheexperiment"),
    // Wordless — mostly vowel-less tokens.
    Entry("wordless", "tr th s"),
    Entry("wordless", "qvx zjk"),
    Entry("wordless", "bdf ghj klm"),
    Entry("wordless", "qwr tyu iop"),
    Entry("wordless", "zxv nm"),
    Entry("wordless", "rty fgh vbn"),
    Entry("wordless", "lkj hgf dsa"),
    Entry("wordless", "plm okn ijb"),
    Entry("wordless", "wqt sdr fgr"),
    Entry("wordless", "hnj mjk bvc"),
    // Emoji — includes an emoji.
    Entry("emoji", "nice stream 👍"),
    Entry("emoji", "lol 😂"),
    Entry("emoji", "poggers 🚀"),
    Entry("emoji", "this is hype 🔥"),
    Entry("emoji", "great play 🎉"),
    Entry("emoji", "heart ❤️"),
    Entry("emoji", "gg wp 👏"),
    Entry("emoji", "lets gooo 💪"),
    Entry("emoji", "that was insane 🤯"),
    Entry("emoji", "good vibes ✨"),
    // Emoji + punctuation (two rewards).
    Entry("emoji", "absolutely insane play tonight 🔥!"),
    Entry("emoji", "cant wait for tomorrows stream 🎉."),
    // Combos — fire several rules at once.
    Entry("length", "hey everyone just wanted to say the stream has been fantastic tonight and i am really enjoying the energy here!"),
    Entry("question", "does anyone else think the new update to the game is actually really good and well worth trying out?"),
    Entry("emoji", "what a night for the community 🔥 what a night for the community 🔥 what a night for the community!"),
    Entry("wordless", "k"),
    Entry("wordless", "kk"),
    Entry("neutral", "ye"),
    Entry("neutral", "yea"),
    Entry("neutral", "nope"),
    Entry("neutral", "maybe"),
    Entry("neutral", "sure"),
    Entry("neutral", "thanks"),
    Entry("wordless", "ty"),
    Entry("wordless", "thx"),
    Entry("neutral", "sup"),
    Entry("neutral", "later"),
    Entry("wordless", "brb"),
    Entry("wordless", "gtg"),
];

/// A scored corpus sentence.
struct Scored {
    rule: &'static str,
    delta: i64,
    notes: Vec<String>,
}

/// Run the corpus through the real scoring function.
fn run_corpus(config: &Config, sentences: &[String]) -> Vec<Scored> {
    sentences
        .iter()
        .map(|text| {
            let (delta, notes) = score_message(text, config);
            let rule = CORPUS
                .iter()
                .find(|e| e.text == text)
                .map(|e| e.rule)
                .unwrap_or("custom");
            Scored {
                rule,
                delta,
                notes,
            }
        })
        .collect()
}

/// Basic distribution stats over the deltas.
fn stats(deltas: &[i64]) -> (i64, i64, f64, f64) {
    let n = deltas.len();
    if n == 0 {
        return (0, 0, 0.0, 0.0);
    }
    let min = *deltas.iter().min().unwrap();
    let max = *deltas.iter().max().unwrap();
    let mean = deltas.iter().sum::<i64>() as f64 / n as f64;
    let mut sorted = deltas.to_vec();
    sorted.sort_unstable();
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) as f64 / 2.0
    } else {
        sorted[n / 2] as f64
    };
    (min, max, mean, median)
}

/// Terminal ASCII histogram of the delta distribution.
fn print_histogram(label: &str, scored: &[Scored]) {
    println!("\n=== {label} ===");
    let deltas: Vec<i64> = scored.iter().map(|s| s.delta).collect();
    let (min, max, mean, median) = stats(&deltas);
    println!(
        "count={}  min={}  max={}  mean={mean:.2}  median={median:.1}",
        deltas.len(),
        min,
        max
    );

    // Bucket by integer delta value.
    let mut buckets: BTreeMap<i64, usize> = BTreeMap::new();
    for d in &deltas {
        *buckets.entry(*d).or_insert(0) += 1;
    }
    let max_count = buckets.values().copied().max().unwrap_or(1);
    let width = 40;
    for (delta, count) in &buckets {
        let bar = (count * width) / max_count;
        println!("  {:>4} | {} ({})", delta, "#".repeat(bar), count);
    }
}

/// Per-rule breakdown: how many sentences (by tag) fired, and each rule's
/// total contribution across the whole corpus.
fn print_per_rule(scored: &[Scored], config: &Config) {
    println!("\n=== Per-rule contribution (across the whole corpus) ===");
    let rules = [
        ("punctuation", "end with . ! ?".to_string(), config.punctuation.toggle),
        ("question", "end with ?".to_string(), config.question.toggle),
        ("length", format!(">= {} chars", config.length_min_chars), config.length.toggle),
        ("spam", "repeated chars / mash".to_string(), config.spam.toggle),
        ("no_spacing", format!(">= {} chars, no spaces", config.no_spacing_min_len), config.no_spacing.toggle),
        ("wordless", "mostly vowel-less".to_string(), config.wordless.toggle),
        ("emoji", "includes emoji".to_string(), config.emoji.toggle),
    ];
    println!(
        "{:<14} {:<34} {:>6} {:>7} {:>8}",
        "rule", "fires when", "toggle", "hit #", "contrib"
    );
    for (name, when, toggle) in rules {
        let fired: Vec<&Scored> = scored.iter().filter(|s| s.notes.iter().any(|n| n == name)).collect();
        let contrib: i64 = fired.iter().map(|s| s.delta).sum();
        println!(
            "{:<14} {:<34} {:>6} {:>7} {:>8}",
            name,
            when,
            if toggle { "on" } else { "off" },
            fired.len(),
            contrib
        );
    }
    // By-tag hit rate: for each corpus tag, how many of its sentences actually
    // fired their primary rule.
    println!("\n=== By-tag hit rate (primary rule fired?) ===");
    let mut by_rule: BTreeMap<&'static str, Vec<&Scored>> = BTreeMap::new();
    for s in scored {
        by_rule.entry(s.rule).or_default().push(s);
    }
    println!("{:<14} {:>8} {:>8}", "tag", "corpus", "hit");
    for (rule, entries) in &by_rule {
        let hit = entries
            .iter()
            .filter(|s| s.notes.iter().any(|n| n == rule))
            .count();
        println!("{:<14} {:>8} {:>8}", rule, entries.len(), hit);
    }
}

/// Render the delta distribution as an SVG chart via plotters.
fn render_svg(scored: &[Scored], out_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let deltas: Vec<i64> = scored.iter().map(|s| s.delta).collect();
    let mut buckets: BTreeMap<i64, usize> = BTreeMap::new();
    for d in &deltas {
        *buckets.entry(*d).or_insert(0) += 1;
    }
    let max_count = buckets.values().copied().max().unwrap_or(1) as u32;
    let min_d = *buckets.keys().next().unwrap();
    let max_d = *buckets.keys().last().unwrap();

    let root = SVGBackend::new(out_path, (900, 600)).into_drawing_area();
    root.fill(&WHITE)?;
    let mut chart = ChartBuilder::on(&root)
        .caption("Message score distribution (score-messages)", ("sans-serif", 24))
        .margin(20)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(min_d..max_d, 0..max_count)?;

    chart
        .configure_mesh()
        .x_desc("Delta")
        .y_desc("Sentence count")
        .draw()?;

    chart.draw_series(
        Histogram::vertical(&chart)
            .style(RED.mix(0.7).filled())
            .margin(4)
            .data(buckets.iter().map(|(d, c)| (*d, *c as u32))),
    )?;
    root.present()?;
    println!("\nSVG chart written to {out_path}");
    Ok(())
}

/// Load a config from a config.json path, or the production default config if
/// none is given (so a bare run reflects exactly what the module ships with).
fn load_config(path: Option<&str>) -> Config {
    match path {
        Some(p) if Path::new(p).exists() => {
            let root: serde_json::Value = std::fs::read_to_string(p)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            config_from_root(&root)
        }
        Some(p) => {
            eprintln!("warning: config {p} not found — using production defaults");
            default_config()
        }
        None => default_config(),
    }
}

/// Load a custom corpus file (one sentence per line) or fall back to the
/// embedded corpus.
fn load_corpus(path: Option<&str>) -> Vec<String> {
    match path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(data) => {
                let lines: Vec<String> = data
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                if lines.is_empty() {
                    eprintln!("warning: corpus file {p} is empty — using embedded corpus");
                    embedded_sentences()
                } else {
                    lines
                }
            }
            Err(e) => {
                eprintln!("warning: could not read {p}: {e} — using embedded corpus");
                embedded_sentences()
            }
        },
        None => embedded_sentences(),
    }
}

fn embedded_sentences() -> Vec<String> {
    CORPUS.iter().map(|e| e.text.to_string()).collect()
}

fn print_config_summary(config: &Config) {
    println!("--- config ---");
    println!(
        "punct={:+} q={:+} len={:+} spam={:+} nospace={:+} wordless={:+} emoji={:+} freq={:+}",
        config.punctuation.score,
        config.question.score,
        config.length.score,
        config.spam.score,
        config.no_spacing.score,
        config.wordless.score,
        config.emoji.score,
        config.frequency.score
    );
    println!(
        "length_min_chars={} no_spacing_min_len={} spam_min_token_len={} mash_min_len={} wordless_ratio={}",
        config.length_min_chars,
        config.no_spacing_min_len,
        config.spam_min_token_len,
        config.keyboard_mash_min_len,
        config.wordless_vowel_ratio
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<String> = None;
    let mut diff_path: Option<String> = None;
    let mut corpus_path: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next(),
            "--diff" => diff_path = args.next(),
            "--corpus" => corpus_path = args.next(),
            "--help" | "-h" => {
                println!(
                    "score_probe: tune score-messages rule weights\n\n  --config <path>  config.json to probe\n  --diff <path>    second config.json to compare\n  --corpus <path>  corpus file (one sentence per line)\n  --help           this message"
                );
                return;
            }
            other => eprintln!("warning: ignoring unknown arg {other}"),
        }
    }

    let corpus = load_corpus(corpus_path.as_deref());
    if corpus.len() < 100 {
        eprintln!(
            "note: corpus has {} sentences (embedded corpus is 100+)",
            corpus.len()
        );
    }

    let config = load_config(config_path.as_deref());
    print_config_summary(&config);
    let scored = run_corpus(&config, &corpus);
    print_histogram("Distribution", &scored);
    print_per_rule(&scored, &config);
    if let Err(e) = render_svg(&scored, "score_distribution.svg") {
        eprintln!("warning: could not render SVG: {e}");
    }

    // A/B comparison against a second config, if requested.
    if let Some(diff) = diff_path {
        let diff_config = load_config(Some(&diff));
        println!("\n\n### DIFF: baseline vs {diff} ###");
        print_config_summary(&diff_config);
        let scored_b = run_corpus(&diff_config, &corpus);
        let a: Vec<i64> = scored.iter().map(|s| s.delta).collect();
        let b: Vec<i64> = scored_b.iter().map(|s| s.delta).collect();
        let (min_a, max_a, mean_a, med_a) = stats(&a);
        let (min_b, max_b, mean_b, med_b) = stats(&b);
        println!("\n{:<22} {:>10} {:>10} {:>10}", "", "baseline", diff, "shift");
        println!(
            "{:<22} {:>10} {:>10} {:>10}",
            "min",
            min_a,
            min_b,
            b.iter().min().unwrap_or(&0) - a.iter().min().unwrap_or(&0)
        );
        println!(
            "{:<22} {:>10} {:>10} {:>10}",
            "max",
            max_a,
            max_b,
            b.iter().max().unwrap_or(&0) - a.iter().max().unwrap_or(&0)
        );
        println!(
            "{:<22} {:>10.2} {:>10.2} {:>+10.2}",
            "mean",
            mean_a,
            mean_b,
            mean_b - mean_a
        );
        println!(
            "{:<22} {:>10.1} {:>10.1} {:>+10.1}",
            "median",
            med_a,
            med_b,
            med_b - med_a
        );
        // Per-rule contribution delta.
        let rules = [
            "punctuation",
            "question",
            "length",
            "spam",
            "no_spacing",
            "wordless",
            "emoji",
        ];
        println!("\n{:<14} {:>10} {:>10} {:>+10}", "rule", "baseline", diff, "contrib shift");
for rule in rules {
        let a_fired: Vec<&Scored> = scored.iter().filter(|s| s.notes.iter().any(|n| n == rule)).collect();
        let b_fired: Vec<&Scored> = scored_b.iter().filter(|s| s.notes.iter().any(|n| n == rule)).collect();
        let a_contrib: i64 = a_fired.iter().map(|s| s.delta).sum();
        let b_contrib: i64 = b_fired.iter().map(|s| s.delta).sum();
        println!(
            "{:<14} {:>10} {:>10} {:>+10}",
            rule,
            a_contrib,
            b_contrib,
            b_contrib - a_contrib
        );
    }
}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_corpus_has_at_least_100_sentences() {
        assert!(CORPUS.len() >= 100, "corpus has {} entries", CORPUS.len());
    }

    #[test]
    fn every_corpus_tag_is_a_real_rule() {
        let known = [
            "neutral", "punctuation", "question", "length", "spam",
            "no_spacing", "wordless", "emoji",
        ];
        for e in CORPUS {
            assert!(
                known.contains(&e.rule),
                "unknown corpus tag {:?}",
                e.rule
            );
        }
    }

    #[test]
    fn neutral_sentences_score_zero_with_production_defaults() {
        let cfg = default_config();
        for e in CORPUS.iter().filter(|e| e.rule == "neutral") {
            let (delta, notes) = score_message(e.text, &cfg);
            assert_eq!(delta, 0, "neutral '{}' scored {delta} ({notes:?})", e.text);
        }
    }

    #[test]
    fn production_defaults_punish_spam_and_wordless() {
        let cfg = default_config();
        let spam = CORPUS.iter().find(|e| e.rule == "spam").unwrap();
        let (delta, notes) = score_message(spam.text, &cfg);
        assert!(delta < 0, "spam '{}' should be punished, got {delta}", spam.text);
        assert!(notes.contains(&"spam".to_string()));

        let wordless = CORPUS.iter().find(|e| e.rule == "wordless").unwrap();
        let (delta, notes) = score_message(wordless.text, &cfg);
        assert!(delta < 0, "wordless '{}' should be punished, got {delta}", wordless.text);
        assert!(notes.contains(&"wordless".to_string()));
    }

    #[test]
    fn stats_computes_min_max_mean_median() {
        let deltas = vec![1, 2, 2, 3, 3, 3];
        let (min, max, mean, median) = stats(&deltas);
        assert_eq!(min, 1);
        assert_eq!(max, 3);
        assert!((mean - 14.0 / 6.0).abs() < 1e-9);
        assert!((median - 2.5).abs() < 1e-9);

        let even = vec![1, 2, 3, 4];
        let (_, _, _, median) = stats(&even);
        assert!((median - 2.5).abs() < 1e-9);
        let odd = vec![1, 2, 3];
        let (_, _, _, median) = stats(&odd);
        assert!((median - 2.0).abs() < 1e-9);
    }

    #[test]
    fn load_config_reads_module_specific_overlay() {
        let path = "/tmp/probe_load_config_test.json";
        std::fs::write(
            path,
            r#"{"ip":"127.0.0.1","module_specific":{"spam_score":-12,"question_score":7}}"#,
        )
        .unwrap();
        let cfg = load_config(Some(path));
        assert_eq!(cfg.spam.score, -12);
        assert_eq!(cfg.question.score, 7);
        // Unmentioned keys fall back to production defaults.
        assert_eq!(cfg.emoji.score, 1);
        assert_eq!(cfg.length_min_chars, 40);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corpus_file_override_is_used() {
        let path = "/tmp/probe_corpus_test.txt";
        std::fs::write(path, "hello world\njust a test message here\n").unwrap();
        let corpus = load_corpus(Some(path));
        assert_eq!(corpus.len(), 2);
        assert_eq!(corpus[0], "hello world");
        let _ = std::fs::remove_file(path);
    }
}
