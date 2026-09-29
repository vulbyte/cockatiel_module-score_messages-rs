# score-messages (Rust)

Cockatiel module — contextual chat message scorer. A `preprocess` module that
scores chat messages contextually (punctuation, questions, length, spam,
no-spacing, wordless) and applies the delta to the user's score.

## Dependencies

- [cockatiel-client](https://github.com/vulbyte/cockatiel_client-rs) (pinned by git rev)
- Tokio, tokio-tungstenite, prost, serde, tracing, uuid

## Build

```sh
cargo build --release
```

## Runtime config

Runtime settings live in `config.json` (not tracked in git). The engine
connection details are supplied by the Cockatiel engine at launch.

## Tuning probe (`score_probe`)

An offline dev tool for tuning the scoring rules. It runs a corpus of 100+
real chat sentences through the *exact* production scoring function and
visualises how the rule weights shape the distribution of message scores, so a
tweak's effect on high/low/average and per-rule contribution is visible in one
run. It is gated behind the `probe` feature and never ships in the module
binary (`cargo build --release` without the feature builds the module only).

```sh
# Probe the production defaults (no engine needed — pure local computation):
cargo run --release --features probe --bin score_probe

# Probe a specific config.json (e.g. the live module's):
cargo run --release --features probe --bin score_probe -- --config config.json

# A/B: baseline vs a tweaked config, side-by-side with per-rule shift:
cargo run --release --features probe --bin score_probe -- --diff other/config.json

# Use your own corpus (one sentence per line):
cargo run --release --features probe --bin score_probe -- --corpus my-chat.txt
```

Output: a terminal ASCII histogram (min/max/mean/median + bucket counts), a
per-rule contribution breakdown (how many corpus sentences each rule fired on,
and its total contribution), a by-tag hit rate, and an SVG chart
(`score_distribution.svg`) written to the current directory.