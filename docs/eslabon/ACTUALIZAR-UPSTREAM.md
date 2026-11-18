# Updating the Eslabon tree from upstream Codex

`scripts/eslabon/upstream-sync.sh` automates the merge of an upstream release
tag into `publico` and runs the acceptance gate.

## Steps

1. `bash scripts/eslabon/upstream-sync.sh rust-v0.170.0 --dry-run`
   Checks disk and the tag, and simulates the merge with
   `git merge --no-commit --no-ff`, then aborts it. It exits 0 and prints the
   conflict groups if there are any.
2. `bash scripts/eslabon/upstream-sync.sh rust-v0.170.0`
   Creates `merge/<versión>` from `publico` (or `ESLABON_UPSTREAM_BASE`) and
   merges. On conflicts it prints the files grouped by owner and exits 2
   leaving the merge open.
3. Resolve the conflicts and re-run `upstream-sync.sh --continue`.
4. The script builds `codex-cli` (dev, lean), compares the test counts with
   `scripts/eslabon/upstream-baseline.txt`, and runs
   `scripts/eslabon/humo/humo.sh`. It prints `PASA` or `FALLA` and the detail
   per suite.
5. On acceptance, refresh the numbers with
   `bash scripts/eslabon/upstream-sync.sh --update-baseline` and commit it.

The script never pushes; publishing is a separate step.

## Conflict resolution criterion

- Eslabon-owned files (`codex-rs/eslabon-*`, `scripts/eslabon/`,
  `docs/eslabon/`, the `eslabon_*` modules) keep Eslabon's behavior.
- In upstream files, take upstream's version and re-apply only the minimal
  Eslabon hook. The hook is marked with `// eslabon:` comments and usually
  guards a default, a visible string or an added field.
- If upstream changes an API our hook uses, adapt the hook, never upstream.
- Deleted-but-modified files (the old `.github/` CI that F6 removed) stay
  deleted.

## Reading the summary

Each data line in the baseline is `crate/target <passed>` (where:
`codex-cli/bin-codex` runs `cargo test -p codex-cli --bin codex`;
`codex-core/lib:config::` runs the library tests filtered by `config::`;
`all` runs the whole crate). The gate prints one line per entry:

```
  OK    eslabon-config/all: 21 (ignoradas 0)
  FALLA codex-exec/lib: 60 de 64, 0 fallos
```

`OK` means the passed count equals the baseline and there are no failures.
`FALLA` means either the count changed (a new test from upstream, or a
regression) or the suite reported failures. Failed test details are in
`/tmp/eslabon-test-<entry>.log`.
