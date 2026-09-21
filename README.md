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