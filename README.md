Eslabon is a fork of [OpenAI Codex](https://github.com/openai/codex). It is not affiliated with, endorsed by, or sponsored by OpenAI.

# Eslabon

Eslabon is a coding agent for the terminal. It runs on your computer, drives your shell and edits
your files, and talks to any model server that exposes an OpenAI-compatible API. It is built on
[OpenAI Codex](https://github.com/openai/codex), but it is meant to run on local and self-hosted
models first: Ollama, LM Studio, llama.cpp, vLLM, or any hosted provider you configure.

Eslabon keeps its own home in `~/.eslabon` (override it with `ESLABON_HOME`). An exported
`CODEX_HOME` is respected as a fallback, so an existing Codex home can be shared; the default
`~/.eslabon` is created on demand with `0700` permissions. In Eslabon mode the startup defaults
turn off update checks, analytics and feedback. The origin rule denies OpenAI-family hosts once the
configured provider is not one of OpenAI's (unless a stored OpenAI login or
`[eslabon] permitir_openai = true` opts in).

## Install

Releases are published on [GitHub Releases](https://github.com/Eslabon-cli/eslabon/releases) for
macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64). The install script downloads the
tarball for your machine, verifies its SHA-256 checksum and puts `eslabon` in
`${ESLABON_INSTALL_DIR:-$HOME/.local/bin}`. It uses no `sudo` and does not modify your shell
profile. Read the script before you run it:

```shell
curl -fsSL https://github.com/Eslabon-cli/eslabon/raw/main/scripts/eslabon/install-release.sh -o eslabon-install.sh
less eslabon-install.sh
sh eslabon-install.sh
```

The same in one line, pin the release with `ESLABON_VERSION=0.167.2`, and install somewhere else
with `ESLABON_INSTALL_DIR`:

```shell
curl -fsSL https://github.com/Eslabon-cli/eslabon/raw/main/scripts/eslabon/install-release.sh | sh
ESLABON_VERSION=0.167.2 ESLABON_INSTALL_DIR=/usr/local/bin sh eslabon-install.sh
```

If the script warns that the directory is not in `PATH`, add it and then run `eslabon init`.

### Homebrew

The tap ships the same release tarballs (the formula is `packaging/homebrew/eslabon.rb` in this
repository):

```shell
brew tap Eslabon-cli/homebrew-tap
brew install eslabon
```

## Install from source

You need Rust. Build the binary from the workspace:

```shell
git clone https://github.com/Eslabon-cli/eslabon
cd eslabon/codex-rs
cargo build -p codex-cli
./target/debug/eslabon
```
<!-- eslabon: URL provisional -->

The build produces two binaries over the same code: `codex` (upstream behaviour) and `eslabon`
(Eslabon mode; see [differences](#differences-from-codex)). On macOS and Linux you can also use the
project launcher, which builds a release binary and puts `eslabon` in `~/.local/bin`:

```shell
bash scripts/eslabon/install.sh
```

## First run with a local server

Start your local server and run:

```bash
eslabon init
```

With a terminal it probes `127.0.0.1` on the ports local servers usually listen on (Ollama
`11434`, LM Studio `1234`, llama.cpp `8080`, vLLM `8000`), lists the servers and models it
finds, and writes `~/.eslabon/config.toml` for you. If nothing is running, it asks for an
OpenAI-compatible URL and the name of the environment variable that holds the API key (it never
asks for, prints or stores the key itself).

Without a terminal, or to skip the questions, pass the values as flags:

```bash
eslabon init --base-url http://127.0.0.1:11434/v1 --model qwen3:8b
eslabon init --base-url http://127.0.0.1:1234/v1 --model my-model --api-key-env MY_KEY
```

`init` does not overwrite an existing `config.toml` unless you pass `--force`, and then it keeps
the previous file as `config.toml.bak`. At the end it prints what it wrote (never a key) and the
next step, `eslabon`.

The same config by hand, in `~/.eslabon/config.toml` (the model id must be one your server
actually serves; the example uses a common Ollama tag):

```toml
model = "qwen3:8b"
model_provider = "local"

[model_providers.local]
name = "Local OpenAI-compatible server"
base_url = "http://127.0.0.1:11434/v1"
wire_api = "chat"
stream_idle_timeout_ms = 900000
```

Change the port for LM Studio (`1234`), llama.cpp (`8080`) or vLLM (`8000`) and keep the `/v1`
suffix. Then run `eslabon` from the directory you want it to work in. Because the provider does
not require OpenAI authentication, the composer opens without any login step: no account is
needed, and model requests go only to the server you configured.

## Differences from Codex

Eslabon mode (`ESLABON_LAUNCHER=1` or an `eslabon` binary name) changes these things:

- **Own home, own name.** The config and state live in `~/.eslabon` (or `ESLABON_HOME`), not
  `~/.codex`, and headers show `eslabon`. Resolution order: `ESLABON_HOME`, then `CODEX_HOME`, then
  `~/.eslabon`.
- **Chat Completions wire API.** `wire_api = "chat"` is accepted again and sends requests to
  `{base_url}/chat/completions`, which is what most local servers actually implement.
- **Origin rule.** With no OpenAI provider and no stored OpenAI login, OpenAI-family hosts are
  denied for model requests and redirects. Opt out per session with
  `[eslabon] permitir_openai = true`.
- **Data lock.** `[eslabon] data_policy = "phi"` with `[eslabon] phi_providers` restricts model
  requests to the listed providers, checked in the shared HTTP transport on every request and
  redirect. `eslabon phi on|off|status|default` switches it at runtime, and an unreadable switch
  file blocks everything (fail-closed).
- **Named profiles.** `<name>.config.toml` files are layered on top of `config.toml` as Codex
  merges them, and all of a home's profiles can be checked at once.
- **Config verification.** `eslabon revisar` checks a home's profiles, providers, catalogs and
  local servers without loading or running a model. Exit status is non-zero when it finds errors.
- **Catalog tooling.** `eslabon catalog local <id> <base_url>` builds a `model_catalog_json` from a
  running server (llama.cpp, vLLM, LM Studio); `sync`, `providers` and `rebuild` cover hosted
  providers from models.dev and the merged `/model` list.
- **Unattended homes.** `eslabon desatendido <dir>` creates a separate home for runs nobody
  watches: only model, provider and catalog, with no MCP servers, hooks, plugins, connectors or a
  global `AGENTS.md`.
- **Compact TUI.** A right-hand sidebar (model, context usage, cache hits, MCP server state) and
  opencode-style one-line tool rows. `ESLABON_SIDEBAR` and `ESLABON_STREAM` override them.

## Documentation

- [Codex CLI upstream README](docs/upstream/README.codex.md)
- [Codex CLI docs](docs/getting-started.md), [configuration](docs/config.md),
  [sandbox and approvals](docs/sandbox.md)

## License

Apache-2.0, see [LICENSE](LICENSE).
