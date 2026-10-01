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
//!   cargo run --release --features probe --bin score_probe -- --corpus a.txt --corpus b.txt
//!
//! Output: a per-corpus comparison table (one column per file — the embedded
//! "regular" chat corpus plus the bundled `corpus_spam.txt` of gibberish /
//! morse / emoji spam by default, or any files passed via `--corpus`), a
//! terminal ASCII histogram, a per-rule breakdown, and an SVG chart
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

/// 100+ curated sentences covering every AC check and the neutral baseline.
const CORPUS: &[CorpusEntry] = &[
    // Neutral — no AC check fires meaningfully (no capital, no punct, no long runs).
    Entry("neutral", "hey"),
    Entry("neutral", "yo"),
    Entry("neutral", "hi"),
    Entry("neutral", "lol"),
    Entry("neutral", "nice"),
    Entry("neutral", "cool"),
    Entry("neutral", "wow"),
    Entry("neutral", "ok"),
    Entry("neutral", "sick"),
    Entry("neutral", "pog"),
    Entry("neutral", "gg"),
    Entry("neutral", "ye"),
    Entry("neutral", "yea"),
    Entry("neutral", "nope"),
    Entry("neutral", "maybe"),
    Entry("neutral", "sure"),
    Entry("neutral", "thanks"),
    Entry("neutral", "sup"),
    Entry("neutral", "later"),
    Entry("neutral", "k"),
    Entry("neutral", "kk"),
    // Check A — punctuation: ends with .!? (under 192 chars) and/or capitals after punct.
    Entry("punctuation", "Hello there."),
    Entry("punctuation", "Good morning!"),
    Entry("punctuation", "What a play."),
    Entry("punctuation", "See you tomorrow."),
    Entry("punctuation", "Have a good one!"),
    Entry("punctuation", "This is great."),
    Entry("punctuation", "That was wild!"),
    Entry("punctuation", "Cant wait for the next one."),
    Entry("punctuation", "You are doing awesome!"),
    Entry("punctuation", "The stream is popping off."),
    Entry("punctuation", "Im actually enjoying this a lot."),
    Entry("punctuation", "That clip is going to be legendary."),
    Entry("punctuation", "How are you?"),
    Entry("punctuation", "Any plans tonight?"),
    Entry("punctuation", "Did you see that clutch?"),
    Entry("punctuation", "Who is winning right now?"),
    // Check B — trigrams: common word-start trigrams score +3 each.
    Entry("trigram", "hello"),
    Entry("trigram", "great"),
    Entry("trigram", "stream"),
    Entry("trigram", "thank"),
    Entry("trigram", "welcome"),
    Entry("trigram", "please"),
    Entry("trigram", "better"),
    Entry("trigram", "little"),
    Entry("trigram", "really"),
    Entry("trigram", "people"),
    Entry("trigram", "think"),
    Entry("trigram", "coming"),
    Entry("trigram", "world"),
    Entry("trigram", "night"),
    Entry("trigram", "music"),
    Entry("trigram", "games"),
    Entry("trigram", "house"),
    Entry("trigram", "friend"),
    Entry("trigram", "happy"),
    Entry("trigram", "water"),
    // Check C — leading capital: +20 if first non-space char is a capital, else -10.
    Entry("capital", "Hello"),
    Entry("capital", "Welcome"),
    Entry("capital", "Great"),
    Entry("capital", "Awesome"),
    Entry("capital", "Perfect"),
    Entry("capital", "Amazing"),
    Entry("capital", "Nice"),
    Entry("capital", "Cool"),
    Entry("capital", "Thanks"),
    Entry("capital", "Good"),
    Entry("capital", "hello"),
    Entry("capital", "welcome"),
    Entry("capital", "great"),
    Entry("capital", "awesome"),
    Entry("capital", "perfect"),
    Entry("capital", "amazing"),
    Entry("capital", "nice"),
    Entry("capital", "cool"),
    Entry("capital", "thanks"),
    Entry("capital", "good"),
    // Check D — repeating characters: any letter 3+ times sequentially, -50.
    Entry("repeating", "aaaaaa"),
    Entry("repeating", "bbbbbbbb"),
    Entry("repeating", "ccccccccc"),
    Entry("repeating", "dddddd"),
    Entry("repeating", "eeeeeee"),
    Entry("repeating", "ffffff"),
    Entry("repeating", "ggggg"),
    Entry("repeating", "hhhhhh"),
    Entry("repeating", "jjjjjj"),
    Entry("repeating", "kkkkkk"),
    Entry("repeating", "abcaaa"),
    Entry("repeating", "zzzzzz"),
    // Check E — space ratio: spaces >= 20% of non-spaces, +20, else -20.
    Entry("space_ratio", "a b c d e f g h i j k l m n o p q r s t u v w x y z"),
    Entry("space_ratio", "hi there how are you doing today my friend"),
    Entry("space_ratio", "one two three four five six seven eight nine ten"),
    Entry("space_ratio", "the quick brown fox jumps over the lazy dog again"),
    Entry("space_ratio", "lorem ipsum dolor sit amet consectetur adipiscing elit"),
    Entry("space_ratio", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    Entry("space_ratio", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    Entry("space_ratio", "ccccccccccccccccccccccccccccc"),
    Entry("space_ratio", "ddddddddddddddddddddddddddddd"),
    Entry("space_ratio", "eeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
    // Check F — run-on: 75+ chars without punctuation after a punct mark, -150.
    Entry("run_on", "That was amazing. aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    Entry("run_on", "Great stream. bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    // Check G — 32-char groupings: each 32-char group without a space, -20.
    Entry("grouping", "thisisareallylongmessagewithnospaceswhatsoeverintheentirething"),
    Entry("grouping", "cantbelieveiwrotethiswithnospacesatallthatscrazy"),
    Entry("grouping", "nospacesinthelongmessageatyalljustonegiantrunonsentence"),
    Entry("grouping", "anothergreatexampleofamessagethathasabsolutelynospaces"),
    Entry("grouping", "thestreamisabsolutelyonfiretonightwithzerochanceofslowingdown"),
    Entry("grouping", "iknowthisisawholerunonmessagebutitstillcountsaschatigues"),
    Entry("grouping", "notasinglespaceistobefoundinthisentirelyunbrokenstringoftext"),
    Entry("grouping", "chatmoveoverbecauseiamabouttotypethelongestwordeverwithoutanybreaks"),
    Entry("grouping", "ifeltlikewritingamessagewithnospacesjusttoseehowitwouldbescored"),
    Entry("grouping", "definitelynotusinganyspacesinthisoneforthesakeoftheexperiment"),
    // Emoji — chat extra (+1).
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
    // Well-formed messages (combos): several checks fire positively.
    Entry("punctuation", "Hello everyone. Welcome to the stream!"),
    Entry("punctuation", "That was an absolutely incredible comeback and I am so glad I stayed up to watch!"),
    Entry("punctuation", "The patience you showed while explaining the build order was great."),
    Entry("punctuation", "Does anyone else think the new update is really good and worth trying out?"),
];


/// A scored corpus sentence.
struct Scored {
    rule: &'static str,
    delta: i32,
    notes: Vec<String>,
}

/// Run a corpus (tagged entries or raw lines) through the real scoring
/// function. A raw line that matches an embedded entry inherits its rule tag;
/// anything else is tagged `custom` (or `spam` when it comes from a spam
/// corpus file).
fn run_corpus(config: &Config, sentences: &[String], tag: &str) -> Vec<Scored> {
    sentences
        .iter()
        .map(|text| {
            let (delta, notes) = score_message(text, config);
            let rule = if tag == "spam" {
                "spam"
            } else {
                CORPUS
                    .iter()
                    .find(|e| e.text == text)
                    .map(|e| e.rule)
                    .unwrap_or("custom")
            };
            Scored {
                rule,
                delta,
                notes,
            }
        })
        .collect()
}

/// Basic distribution stats over the deltas.
fn stats(deltas: &[i32]) -> (i32, i32, f32, f32) {
    let n = deltas.len();
    if n == 0 {
        return (0, 0, 0.0, 0.0);
    }
    let min = *deltas.iter().min().unwrap();
    let max = *deltas.iter().max().unwrap();
    let mean = deltas.iter().sum::<i32>() as f32 / n as f32;
    let mut sorted = deltas.to_vec();
    sorted.sort_unstable();
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) as f32 / 2.0
    } else {
        sorted[n / 2] as f32
    };
    (min, max, mean, median)
}

/// Terminal ASCII histogram of the delta distribution.
fn print_histogram(label: &str, scored: &[Scored]) {
    println!("\n=== {label} ===");
    let deltas: Vec<i32> = scored.iter().map(|s| s.delta).collect();
    let (min, max, mean, median) = stats(&deltas);
    println!(
        "count={}  min={}  max={}  mean={mean:.2}  median={median:.1}",
        deltas.len(),
        min,
        max
    );

    // Bucket by integer delta value.
    let mut buckets: BTreeMap<i32, usize> = BTreeMap::new();
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
        ("punctuation", "ends with .!? / caps after punct".to_string(), config.punctuation.toggle),
        ("trigram", "valid word-start trigrams".to_string(), config.trigram.toggle),
        ("capital", "leading capital".to_string(), config.capital.toggle),
        ("repeating", "letter repeated 3+ times".to_string(), config.repeating.toggle),
        ("space_ratio", format!("spaces >= {}% of non-space", config.space_ratio_pct), config.space_ratio.toggle),
        ("run_on", format!("{}+ chars w/o punctuation", config.run_on_chars), config.run_on.toggle),
        ("grouping", format!("{}-char group w/o space", config.grouping_size), config.grouping.toggle),
        ("emoji", "includes emoji".to_string(), config.emoji.toggle),
        ("spam", "emoji wall / morse / gibberish".to_string(), config.spam.toggle),
    ];
    println!(
        "{:<14} {:<34} {:>6} {:>7} {:>8}",
        "rule", "fires when", "toggle", "hit #", "contrib"
    );
    for (name, when, toggle) in rules {
        let fired: Vec<&Scored> = scored.iter().filter(|s| s.notes.iter().any(|n| n == name)).collect();
        let contrib: i32 = fired.iter().map(|s| s.delta).sum();
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
    let deltas: Vec<i32> = scored.iter().map(|s| s.delta).collect();
    let mut buckets: BTreeMap<i32, usize> = BTreeMap::new();
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

/// Load one corpus file (one sentence per line) or the embedded corpus.
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

/// A named corpus: the display label plus its sentences.
struct NamedCorpus {
    label: String,
    tag: &'static str,
    sentences: Vec<String>,
}

/// Build the set of corpora to compare. With no `--corpus` flags the default
/// is the embedded "regular" corpus plus the bundled `corpus_spam.txt`; each
/// `--corpus <path>` adds a column named after the file stem.
fn load_corpora(paths: &[String]) -> Vec<NamedCorpus> {
    let mut corpora: Vec<NamedCorpus> = Vec::new();
    if paths.is_empty() {
        corpora.push(NamedCorpus {
            label: "regular".to_string(),
            tag: "regular",
            sentences: embedded_sentences(),
        });
        match std::fs::read_to_string("corpus_spam.txt") {
            Ok(data) => {
                let lines: Vec<String> = data
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                if !lines.is_empty() {
                    corpora.push(NamedCorpus {
                        label: "spam".to_string(),
                        tag: "spam",
                        sentences: lines,
                    });
                } else {
                    eprintln!("warning: corpus_spam.txt is empty — omitting spam column");
                }
            }
            Err(e) => eprintln!("warning: could not read corpus_spam.txt: {e} — omitting spam column"),
        }
    } else {
        for p in paths {
            let stem = std::path::Path::new(p)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone());
            let tag = if stem.contains("spam") { "spam" } else { "regular" };
            corpora.push(NamedCorpus {
                label: stem,
                tag,
                sentences: load_corpus(Some(p)),
            });
        }
    }
    corpora
}

/// Print one column per corpus: count, min/max, mean, median, and % negative,
/// so each corpus's trend is visible side by side while tuning.
fn print_corpus_columns(config: &Config, corpora: &[NamedCorpus]) {
    println!("\n=== Corpus comparison (one column per file) ===");
    let scored: Vec<Vec<Scored>> = corpora
        .iter()
        .map(|c| run_corpus(config, &c.sentences, c.tag))
        .collect();

    let mut headers = format!("{:<18}", "metric");
    for c in corpora {
        headers.push_str(&format!("{:>14}", c.label));
    }
    println!("{headers}");

    type Row = (String, fn(&[Scored]) -> f32, usize);
    let rows: Vec<Row> = vec![
        ("count".to_string(), |s| s.len() as f32, 0),
        ("min".to_string(), |s| s.iter().map(|x| x.delta).min().unwrap_or(0) as f32, 0),
        ("max".to_string(), |s| s.iter().map(|x| x.delta).max().unwrap_or(0) as f32, 0),
        ("mean".to_string(), |s| s.iter().map(|x| x.delta).sum::<i32>() as f32 / s.len().max(1) as f32, 2),
        ("median".to_string(), |s| stats(&s.iter().map(|x| x.delta).collect::<Vec<_>>()).3, 1),
        (
            "% negative".to_string(),
            |s| {
                let n = s.iter().filter(|x| x.delta < 0).count();
                n as f32 / s.len().max(1) as f32 * 100.0
            },
            1,
        ),
    ];

    for (name, f, decimals) in rows {
        let mut line = format!("{name:<18}");
        for s in &scored {
            let v = f(s);
            let formatted = if decimals == 0 {
                format!("{:>14}", v as i32)
            } else {
                let s = format!("{v:.decimals$}");
                format!("{s:>14}")
            };
            line.push_str(&formatted);
        }
        println!("{line}");
    }
}

fn embedded_sentences() -> Vec<String> {
    CORPUS.iter().map(|e| e.text.to_string()).collect()
}

fn print_config_summary(config: &Config) {
    println!("--- config ---");
    println!(
        "punct={:+} trigram={:+} capital={:+} repeat={:+} space_ratio={:+} run_on={:+} grouping={:+} emoji={:+} freq={:+}",
        config.punctuation.score,
        config.trigram.score,
        config.capital.score,
        config.repeating.score,
        config.space_ratio.score,
        config.run_on.score,
        config.grouping.score,
        config.emoji.score,
        config.frequency.score
    );
    println!(
        "max_chars={} punct_cap=+{}/-{} capital_penalty={} repeat_threshold={} run_on={}/{} grouping={} space_ratio=+{}/+{} ({}%)",
        config.max_chars,
        config.punct_capital_bonus,
        config.punct_capital_penalty,
        config.capital_penalty,
        config.repeating_threshold,
        config.run_on_chars,
        config.run_on_window,
        config.grouping_size,
        config.space_ratio.score,
        -config.space_ratio_penalty,
        config.space_ratio_pct
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<String> = None;
    let mut diff_path: Option<String> = None;
    let mut corpus_paths: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next(),
            "--diff" => diff_path = args.next(),
            "--corpus" => {
                if let Some(p) = args.next() {
                    corpus_paths.push(p);
                }
            }
            "--help" | "-h" => {
                println!(
                    "score_probe: tune score-messages rule weights\n\n  --config <path>  config.json to probe\n  --diff <path>    second config.json to compare\n  --corpus <path>  corpus file (repeatable; each adds a comparison column)\n                  (default: embedded 'regular' + corpus_spam.txt)\n  --help           this message"
                );
                return;
            }
            other => eprintln!("warning: ignoring unknown arg {other}"),
        }
    }

    let corpora = load_corpora(&corpus_paths);
    let primary = &corpora[0];
    if primary.sentences.len() < 100 {
        eprintln!(
            "note: primary corpus has {} sentences (embedded corpus is 100+)",
            primary.sentences.len()
        );
    }

    let config = load_config(config_path.as_deref());
    print_config_summary(&config);
    print_corpus_columns(&config, &corpora);
    let scored = run_corpus(&config, &primary.sentences, primary.tag);
    print_histogram("Distribution (primary corpus)", &scored);
    print_per_rule(&scored, &config);
    if let Err(e) = render_svg(&scored, "score_distribution.svg") {
        eprintln!("warning: could not render SVG: {e}");
    }

    // A/B comparison against a second config, if requested.
    if let Some(diff) = diff_path {
        let diff_config = load_config(Some(&diff));
        println!("\n\n### DIFF: baseline vs {diff} ###");
        print_config_summary(&diff_config);
        let scored_b = run_corpus(&diff_config, &primary.sentences, primary.tag);
        let a: Vec<i32> = scored.iter().map(|s| s.delta).collect();
        let b: Vec<i32> = scored_b.iter().map(|s| s.delta).collect();
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
            "trigram",
            "capital",
            "repeating",
            "space_ratio",
            "run_on",
            "grouping",
            "emoji",
            "spam",
        ];
        println!("\n{:<14} {:>10} {:>10} {:>+10}", "rule", "baseline", diff, "contrib shift");
for rule in rules {
        let a_fired: Vec<&Scored> = scored.iter().filter(|s| s.notes.iter().any(|n| n == rule)).collect();
        let b_fired: Vec<&Scored> = scored_b.iter().filter(|s| s.notes.iter().any(|n| n == rule)).collect();
        let a_contrib: i32 = a_fired.iter().map(|s| s.delta).sum();
        let b_contrib: i32 = b_fired.iter().map(|s| s.delta).sum();
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
            "neutral", "punctuation", "trigram", "capital", "repeating",
            "space_ratio", "run_on", "grouping", "emoji",
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
    fn neutral_sentences_stay_positive_or_near_zero() {
        let cfg = default_config();
        // Tuned for modern chat: lowercase is never penalised, a space-less
        // short message like "gg" earns +5 from the space-ratio bonus (and
        // often a trigram or two at +8 each), so a plain lowercase message
        // lands positive or near zero — never deep in the negative.
        for e in CORPUS.iter().filter(|e| e.rule == "neutral") {
            let (delta, notes) = score_message(e.text, &cfg);
            assert!(
                delta >= 0,
                "neutral '{}' scored {delta} ({notes:?}), expected >= 0",
                e.text
            );
        }
    }

    #[test]
    fn production_defaults_punish_repeating_chars() {
        let cfg = default_config();
        let repeating = CORPUS.iter().find(|e| e.rule == "repeating").unwrap();
        let (delta, notes) = score_message(repeating.text, &cfg);
        assert!(delta < 0, "repeating '{}' should be punished, got {delta}", repeating.text);
        assert!(notes.contains(&"repeating".to_string()));
    }

    #[test]
    fn production_defaults_reward_punctuation() {
        let cfg = default_config();
        let punct = CORPUS.iter().find(|e| e.rule == "punctuation").unwrap();
        let (delta, notes) = score_message(punct.text, &cfg);
        assert!(delta > 0, "punctuation '{}' should be rewarded, got {delta}", punct.text);
        assert!(notes.contains(&"punctuation".to_string()));
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
            r#"{"ip":"127.0.0.1","module_specific":{"repeating_score":-60,"run_on_score":200}}"#,
        )
        .unwrap();
        let cfg = load_config(Some(path));
        assert_eq!(cfg.repeating.score, -60);
        assert_eq!(cfg.run_on.score, 200);
        // Unmentioned keys fall back to production defaults.
        assert_eq!(cfg.punctuation.score, 30);
        assert_eq!(cfg.trigram.score, 8);
        assert_eq!(cfg.capital.score, 30);
        assert_eq!(cfg.emoji.score, 15);
        assert_eq!(cfg.capital_penalty, 0);
        assert_eq!(cfg.punct_capital_penalty, 0);
        assert_eq!(cfg.space_ratio_penalty, -5);
        assert_eq!(cfg.space_ratio_pct, 10);
        assert_eq!(cfg.spam.score, 100);
        assert_eq!(cfg.spam_emoji_max, 5);
        assert_eq!(cfg.spam_symbol_ratio_pct, 30);
        assert_eq!(cfg.spam_gibberish_word_min, 8);
        assert_eq!(cfg.spam_gibberish_vowel_pct, 25);
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

    #[test]
    fn spam_corpus_is_punished_under_defaults() {
        // The bundled spam corpus must score negative under the production
        // defaults — exploitation is the counter to the positive bias.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus_spam.txt");
        let lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        assert!(!lines.is_empty(), "corpus_spam.txt should have entries");
        let cfg = default_config();
        for line in &lines {
            let (delta, notes) = score_message(line, &cfg);
            assert!(
                delta < 0,
                "spam '{}' scored {delta} ({notes:?}), expected < 0 under defaults",
                line
            );
        }
    }

    #[test]
    fn load_corpora_defaults_to_regular_and_spam() {
        let cwd = std::env::current_dir().unwrap();
        let _ = std::fs::create_dir_all(&cwd);
        // Write corpus_spam.txt into the cwd so the default pairing is found.
        let path = cwd.join("corpus_spam.txt");
        let had = path.exists();
        if !had {
            std::fs::write(&path, "shgekskshsjs\n.... . .-.. .-.. ---\n").unwrap();
        }
        let corpora = load_corpora(&[]);
        assert_eq!(corpora.len(), 2, "regular + spam columns");
        assert_eq!(corpora[0].label, "regular");
        assert_eq!(corpora[1].label, "spam");
        if !had {
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn run_corpus_tags_spam_entries() {
        let cfg = default_config();
        let scored = run_corpus(&cfg, &["shgekskshsjs".to_string()], "spam");
        assert_eq!(scored[0].rule, "spam");
        let scored = run_corpus(&cfg, &["hello".to_string()], "regular");
        assert_eq!(scored[0].rule, "trigram");
    }
}
