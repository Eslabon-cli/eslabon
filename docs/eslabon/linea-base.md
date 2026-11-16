# Eslabon — test baseline

Taken 2026-11-12 on branch `publico` at `dcd23e5d88` (Codex 0.167.2 + Eslabon patch),
macOS arm64, rustc 1.95.0, with
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 RUST_MIN_STACK=8388608` and `--test-threads=4`.

| Crate | Command | Result |
|---|---|---|
| eslabon-config | `cargo test -p eslabon-config` | 8 passed |
| eslabon-catalog | `cargo test -p eslabon-catalog` | 4 passed |
| codex-api | `cargo test -p codex-api` | 252 passed (223 unit + 29 across 6 integration binaries) |
| codex-http-client | `cargo test -p codex-http-client` | 137 passed (127 + 10) |
| codex-tui | `cargo test -p codex-tui eslabon` | 28 passed (Eslabon-filtered) |

No failures. Any failure in these crates after a change is a regression of that change.

Known upstream failures that do **not** count as regressions: the `codex-core` integration suites
fail on this Mac also on upstream (no V8 prebuilt for `codex-code-mode-host`); they are out of the
baseline on purpose.
