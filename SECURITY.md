# Security Policy

Eslabon is a fork of [OpenAI Codex](https://github.com/openai/codex) and is not affiliated with
OpenAI. This policy covers Eslabon's own changes; problems that also exist in upstream Codex
should be reported to OpenAI as well (see [Upstream Codex](#upstream-codex)).

## Reporting a vulnerability

Report security issues through GitHub private vulnerability reporting in this repository
(the Security tab, or the repository's `/security/advisories/new` page). Please do not open a
public issue, do not include protected data in the report, and keep the reproduction minimal.
<!-- eslabon: URL provisional -->

## Scope

Eslabon's own security work is the data lock and the origin rule. Everything else — the sandbox,
approvals and network controls — is upstream Codex's model and is unchanged.

- **Data lock.** `[eslabon] data_policy = "phi"` (with `[eslabon] phi_providers`) restricts model
  requests to the listed providers. `eslabon phi on|off|status|default` writes a runtime switch in
  the Eslabon home that overrides the config default; an unreadable switch file blocks every
  request (fail-closed), and the check lives in the shared HTTP transport so model clients,
  extensions, remote MCP servers and telemetry cannot route around it.
- **Origin rule.** In Eslabon mode with no OpenAI provider and no stored OpenAI login, requests to
  OpenAI-family hosts are denied. `[eslabon] permitir_openai = true` lifts it for a session that
  legitimately uses OpenAI.
- **Sandbox and approvals.** Commands still run under the same sandbox and approval settings as
  Codex. Eslabon does not add a bypass.

## Upstream Codex

If a vulnerability is in code inherited from Codex, it affects Codex users too. Report it to
OpenAI's security program as described in the
[upstream security policy](https://github.com/openai/codex/blob/main/SECURITY.md); Eslabon will
pick up the upstream fix when it syncs.

## How to operate safely

Eslabon keeps Codex's security boundaries: sandboxing, approvals and network controls apply
exactly as they do upstream, so the upstream documentation applies equally — see
[Agent approvals & security](https://developers.openai.com/codex/agent-approvals-security) and
[sandbox](docs/sandbox.md). Eslabon's own first-run behavior and the minimal config for a local
server are described in the README.
