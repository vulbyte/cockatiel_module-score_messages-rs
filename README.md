# score-messages (Rust)

Cockatiel module — chat message scorer based on the **Animal Crossing (GCN)
letter-scoring algorithm** ([hunter-r.com/ac-letter-scorer](https://hunter-r.com/ac-letter-scorer/),
[source](https://github.com/HunterRDev/AC-Letter-Scorer)). A `preprocess`
module that scores each chat message through seven checks and applies the
delta to the user's score:

| Check | What it rewards / punishes | Default points |
|-------|----------------------------|----------------|
| A. Punctuation | ends with `.`/`!`/`?` (+30); capital within 3 chars after a punct (+20) or not (0) | 30 |
| B. Trigrams | valid word-start trigrams (game's bugged tables) | +8 each |
| C. Leading capital | first non-space char is a capital (+30) or not (0) | 30 |
| D. Repeating chars | any letter repeated 3+ times sequentially | −50 |
| E. Space ratio | spaces ≥ 10% of non-spaces (+25) or not (+5) | 25 |
| F. Run-on sentence | 75+ chars without punctuation after a punct mark | −150 |
| G. 32-char groupings | each 32-char group with no space | −20 each |
| H. Spam & gibberish | `spam_emoji_max`+ emoji, a symbol-ratio wall (morse), or a vowel-less long word | −100 per signal |

Plus an optional chat-specific **emoji** bonus (+15, toggleable), the
time-based **frequency** anti-spam rule, and a **viewership multiplier** that
scales the final delta by how small the current audience is.

> Tuned for modern chat with a positive bias: lowercase is never penalised
> (C/A penalties are 0), the space-ratio check *rewards* short space-less
> messages (+5, via a negative penalty) instead of hammering them, and the
> pure rewards (emoji, trigrams, punctuation, leading capital) are the main
> driver of a user's score. A plain `gg` scores +5; `lol`, `pog`, `hey` score
> +13. Exploitation is still punished harshly by Check H: emoji walls,
> morse-code symbol walls, and gibberish keyboard mash all score deeply
> negative.

## External trigram table

Check B normally uses the built-in game-accurate (bugged) trigram tables. To
import your own, set `trigram_table_path` in the module's `config.json` under
`module_specific` to a CSV, JSON, or text file (relative to the module's
working directory). The file is read at startup and cached by path.

**Where to paste the path:** edit `config.json` in this directory (open the
score-messages module row in the TUI and press `e`, or edit the file directly)
and set:

```json
"module_specific": {
  "trigram_table_path": "./trigrams.csv",
  "trigram_toggle": true,
  "trigram_score": 8
}
```

`config.example.json` shows the full file with the path filled in, and
`trigrams.example.csv` is a ready-made table you can copy and extend. The path
is relative to the module's directory, or absolute (e.g. `/home/you/trigrams.json`).

Supported formats (chosen by file extension):

- **`.json`** — an array of 3-char trigrams `["the", "and", "hel"]`, or an
  object mapping a letter to its 2-char suffixes `{"t": ["he", "hr"]}`.
- **`.csv`** — rows of `letter,suffix` pairs, or a single column of 3-char
  trigrams. A `letter,suffix` / `trigram` header row is skipped.
- **`.txt`** (or anything else) — the game's own `~X~`-sectioned format: a
  `~T~` header followed by one 2-char suffix per line.

External tables match strictly within their own letter's section (predictable
for imported data), unlike the built-in table which replicates the game's
section-scan bug. Leave `trigram_table_path` empty to keep the built-in table.

## Viewership multiplier

When enabled, the module polls the engine's `channel_viewers` surface every
`viewership_poll_secs` and uses the **peak** viewer count across all platforms
(e.g. youtube=7, twitch=3, discord=5 → 7). The score delta is scaled by:

```
multiplier = clamp(max_viewers / viewers, 1.0, multiplier_max)
```

So a small stream rewards interaction more (fewer viewers → a higher
multiplier, up to `viewership_multiplier_max`), giving large streamers more
room to breathe. `viewers = 0` (offline) uses the ceiling. Disabled by default;
enable with `viewership_toggle` in `config.json`.

| Key | Default | What it controls |
|-----|---------|------------------|
| `viewership_toggle` | `false` | enable/disable the multiplier |
| `viewership_max_viewers` | `1000` | viewer count where the multiplier is 1.0 |
| `viewership_multiplier_max` | `5.0` | largest multiplier ever applied (clamp) |
| `viewership_poll_secs` | `30` | how often to re-query the engine for viewer counts |

## Dependencies

- [cockatiel-client](https://github.com/vulbyte/cockatiel_client-rs) (pinned by git rev)
- Tokio, tokio-tungstenite, prost, serde, tracing, uuid

## Build

```sh
cargo build --release
```

## Testing

```sh
# All tests (24 lib + 2 module + 11 probe):
cargo test --features probe --all-targets

# Just the scoring lib (fastest — the pure AC-check + multiplier logic):
cargo test --lib

# A specific test:
cargo test --lib viewership
cargo test --lib check_g
```

The viewership multiplier only kicks in at runtime (it needs a live viewer
count from the engine) — the `score_probe` tool shows the pre-multiplier
`sum(A–G)` distribution, which is what you'd be tuning.

## Runtime config

Runtime settings live in `config.json` (not tracked in git). The engine
connection details are supplied by the Cockatiel engine at launch. Each check
has a `<check>_toggle` / `<check>_score` pair, and the scalar thresholds are:

| Key | Default | What it controls |
|-----|---------|------------------|
| `max_chars` | 192 | Check A ceiling for the end-punctuation reward; trigram padding length |
| `punct_capital_bonus` | 20 | Check A: +pts per punctuation followed (≤3 chars) by a capital |
| `punct_capital_penalty` | 0 | Check A: −pts per punctuation NOT followed by a capital |
| `capital_penalty` | 0 | Check C: −pts when the leading non-space char isn't a capital |
| `repeating_threshold` | 3 | Check D: sequential repeats of a letter needed to trigger the penalty |
| `run_on_chars` | 75 | Check F: no-punctuation run length that triggers the run-on penalty |
| `run_on_window` | 76 | Check F: the scan only evaluates while more than this many chars remain |
| `grouping_size` | 32 | Check G: the space-grouping window |
| `space_ratio_pct` | 10 | Check E: spaces ÷ non-spaces (%) threshold for the reward |
| `space_ratio_penalty` | −5 | Check E: points applied below the threshold — negative, so the else-branch *adds* +5 to short space-less messages (decoupled from the +25 reward) |
| `spam_score` | 100 | Check H: points per spam signal (emoji wall, morse/symbol wall, gibberish word) |
| `spam_emoji_max` | 5 | Check H: emoji count above this triggers the emoji-spam signal |
| `spam_symbol_ratio_pct` | 30 | Check H: % of non-space chars that are symbols (non-letter/non-emoji) that triggers the morse/symbol-wall signal |
| `spam_gibberish_word_min` | 8 | Check H: min word length checked for gibberish |
| `spam_gibberish_vowel_pct` | 25 | Check H: vowel share (%) below which a long word is gibberish |

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
# Probe the production defaults (no engine needed — pure local computation).
# Prints a comparison table with one column per corpus: the embedded "regular"
# corpus (real chat) vs corpus_spam.txt (gibberish, morse, emoji spam), so you
# can see how each trend moves as you tune.
cargo run --release --features probe --bin score_probe

# Probe a specific config.json (e.g. the live module's):
cargo run --release --features probe --bin score_probe -- --config config.json

# A/B: baseline vs a tweaked config, side-by-side with per-check shift:
cargo run --release --features probe --bin score_probe -- --diff other/config.json

# Use your own corpus files — each --corpus adds a comparison column:
cargo run --release --features probe --bin score_probe -- --corpus my-chat.txt --corpus corpus_spam.txt
```

Output: a per-corpus comparison table (count/min/max/mean/median/% negative,
one column per file), a terminal ASCII histogram (min/max/mean/median + bucket
counts), a per-check contribution breakdown (how many corpus sentences each
check fired on, and its total contribution), a by-tag hit rate, and an SVG
chart (`score_distribution.svg`) written to the current directory.

The bundled `corpus_spam.txt` is the anti-exploit benchmark: gibberish keyboard
mash, morse code, and emoji spam. Under the production defaults every entry
scores negative — if a tuning pass starts leaking positive, the spam column
flips and you know the weights went too far.