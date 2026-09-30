# score-messages (Rust)

Cockatiel module — chat message scorer based on the **Animal Crossing (GCN)
letter-scoring algorithm** ([hunter-r.com/ac-letter-scorer](https://hunter-r.com/ac-letter-scorer/),
[source](https://github.com/HunterRDev/AC-Letter-Scorer)). A `preprocess`
module that scores each chat message through seven checks and applies the
delta to the user's score:

| Check | What it rewards / punishes | Default points |
|-------|----------------------------|----------------|
| A. Punctuation | ends with `.`/`!`/`?` (+20); capital within 3 chars after a punct (+10) or not (−10) | 20 |
| B. Trigrams | valid word-start trigrams (game's bugged tables) | +3 each |
| C. Leading capital | first non-space char is a capital (+20) or not (−10) | 20 |
| D. Repeating chars | any letter repeated 3+ times sequentially | −50 |
| E. Space ratio | spaces ≥ 20% of non-spaces (+20) or not (−20) | 20 |
| F. Run-on sentence | 75+ chars without punctuation after a punct mark | −150 |
| G. 32-char groupings | each 32-char group with no space | −20 each |

Plus an optional chat-specific **emoji** bonus (+1, toggleable) and the
time-based **frequency** anti-spam rule.

## Dependencies

- [cockatiel-client](https://github.com/vulbyte/cockatiel_client-rs) (pinned by git rev)
- Tokio, tokio-tungstenite, prost, serde, tracing, uuid

## Build

```sh
cargo build --release
```

## Runtime config

Runtime settings live in `config.json` (not tracked in git). The engine
connection details are supplied by the Cockatiel engine at launch. Each check
has a `<check>_toggle` / `<check>_score` pair, and the scalar thresholds are:

| Key | Default | What it controls |
|-----|---------|------------------|
| `max_chars` | 192 | Check A ceiling for the end-punctuation reward; trigram padding length |
| `punct_capital_bonus` | 10 | Check A: +pts per punctuation followed (≤3 chars) by a capital |
| `punct_capital_penalty` | 10 | Check A: −pts per punctuation NOT followed by a capital |
| `capital_penalty` | 10 | Check C: −pts when the leading non-space char isn't a capital |
| `repeating_threshold` | 3 | Check D: sequential repeats of a letter needed to trigger the penalty |
| `run_on_chars` | 75 | Check F: no-punctuation run length that triggers the run-on penalty |
| `run_on_window` | 76 | Check F: the scan only evaluates while more than this many chars remain |
| `grouping_size` | 32 | Check G: the space-grouping window |
| `space_ratio_pct` | 20 | Check E: spaces ÷ non-spaces (%) threshold for the reward |

All are adjustable in `config.json` — use the `score_probe` tool to see how
changes shift the distribution before applying them live.

## Tuning probe (`score_probe`)

An offline dev tool for tuning the scoring checks. It runs a corpus of 100+
real chat sentences through the *exact* production scoring function and
visualises how the check weights shape the distribution of message scores, so a
tweak's effect on high/low/average and per-check contribution is visible in one
run. It is gated behind the `probe` feature and never ships in the module
binary (`cargo build --release` without the feature builds the module only).

```sh
# Probe the production defaults (no engine needed — pure local computation):
cargo run --release --features probe --bin score_probe

# Probe a specific config.json (e.g. the live module's):
cargo run --release --features probe --bin score_probe -- --config config.json

# A/B: baseline vs a tweaked config, side-by-side with per-check shift:
cargo run --release --features probe --bin score_probe -- --diff other/config.json

# Use your own corpus (one sentence per line):
cargo run --release --features probe --bin score_probe -- --corpus my-chat.txt
```

Output: a terminal ASCII histogram (min/max/mean/median + bucket counts), a
per-check contribution breakdown (how many corpus sentences each check fired
on, and its total contribution), a by-tag hit rate, and an SVG chart
(`score_distribution.svg`) written to the current directory.