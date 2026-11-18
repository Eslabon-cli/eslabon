# Eslabon — test baseline

Taken 2026-11-18 on branch `merge/0.170` at `0f40ac4430` (Codex 0.170.0 + Eslabon patch),
on the Mac mini (macOS arm64), rustc 1.95.0, with
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 RUST_MIN_STACK=8388608` and `--test-threads=4`.

| Crate | Command | Result |
|---|---|---|
| eslabon-config | `cargo test -p eslabon-config` | 21 passed |
| codex-cli | `cargo test -p codex-cli --bin codex` | 330 passed (1 ignored) |
| codex-core | `cargo test -p codex-core --lib config::` | 619 passed |
| codex-tui | `cargo test -p codex-tui --lib eslabon` | 39 passed |
| codex-models-manager | `cargo test -p codex-models-manager` | 63 passed |
| codex-http-client | `cargo test -p codex-http-client` | 142 passed |
| codex-exec | `cargo test -p codex-exec --lib` | 64 passed |

No failures. The `codex-exec` integration suite (`--test all`) also passed 174 tests, but the
baseline watches the library suite only to keep the gate reproducible on hosts without the
integration fixtures.

Known upstream failures that do **not** count as regressions: the `codex-core` integration suites
fail on this Mac also on upstream (no V8 prebuilt for `codex-code-mode-host`); they are out of the
baseline on purpose.
