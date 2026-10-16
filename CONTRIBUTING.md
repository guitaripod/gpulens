# Contributing

Bug reports with the output of `gpulens doctor` are the most useful thing you can send, especially from GPUs and setups this project has not seen (multi-GPU, MIG, containers, Ollama or vLLM as the helper).

## Build and test

```sh
cargo test                       # unit tests, render snapshots and CLI tests; no GPU needed
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run -- --demo              # work on the interface against a scripted GPU
```

Render snapshots live in `src/ui/snapshots`. When a change legitimately alters the screen, review the diff and then run `INSTA_UPDATE=always cargo test`.

## Style

- Explain through well-named functions with `///` doc comments rather than inline comments.
- Prose and docs use commas and periods, not em dashes.
- New behaviour comes with a test. Interface changes come with a snapshot or a render assertion.

The code is split by layer (see the Development section of the README), so most changes touch one module and its tests.
