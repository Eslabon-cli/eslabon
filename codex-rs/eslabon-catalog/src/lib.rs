//! eslabon: build Codex model catalogs (`model_catalog_json`) from open sources.
//!
//! Two sources are supported:
//!
//! * [models.dev](https://models.dev) — the open catalog opencode uses; one entry
//!   per hosted provider with context limits, modalities and reasoning support.
//! * A local OpenAI-compatible server (llama.cpp, vLLM, LM Studio), queried for
//!   its loaded models and, on llama.cpp, the real runtime context size.
//!
//! Every [`ModelInfo`] starts from Codex's own fallback descriptor so fields that
//! upstream adds later keep their defaults, and only what the source actually
//! knows is overridden.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use serde_json::Value;
use std::time::Duration;

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Connection details for the generated `[model_providers.<id>]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub env_key: Option<String>,
}

impl ProviderDescriptor {
    /// Contents of `$CODEX_HOME/<id>.config.toml`: a Codex profile that selects this
    /// provider and its catalog. Top-level keys must precede the provider table, or
    /// TOML would attach them to it.
    pub fn profile_config(&self, catalog_path: &str, default_model: Option<&str>) -> String {
        let mut out = format!(
            "model_provider = {id}\nmodel_catalog_json = {catalog}\n",
            id = toml_string(&self.id),
            catalog = toml_string(catalog_path),
        );
        if let Some(model) = default_model {
            out.push_str(&format!("model = {}\n", toml_string(model)));
        }
        out.push_str(&format!(
            "\n[model_providers.{key}]\nname = {name}\nbase_url = {base_url}\nwire_api = \"chat\"\n",
            key = toml_string(&self.id),
            name = toml_string(&self.name),
            base_url = toml_string(&self.base_url),
        ));
        if let Some(env_key) = &self.env_key {
            out.push_str(&format!("env_key = {}\n", toml_string(env_key)));
        }
        out
    }
}

/// A TOML basic string; serde_json's escaping is a valid subset of TOML's.
fn toml_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Provider ids become file names and TOML keys; keep them to a safe alphabet.
pub fn validate_provider_id(id: &str) -> Result<()> {
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid provider id `{id}`: use letters, digits, `-` or `_`"
        ))
    }
}

pub async fn fetch_json(url: &str) -> Result<Value> {
    // eslabon: a denied destination fails with the lock's own message before any
    // connection; the shared transport checks again on every request and redirect.
    if let Some(message) = codex_http_client::eslabon_policy::egress_violation(url) {
        return Err(anyhow!(message));
    }
    let factory = codex_http_client::HttpClientFactory::new(
        codex_http_client::OutboundProxyPolicy::ReqwestDefault,
    );
    let client = factory
        .build_client(url, codex_http_client::ClientRouteClass::Other)
        .map_err(|err| anyhow!("building HTTP client for {url}: {err}"))?;
    let response = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    Ok(response.json().await?)
}

/// Converts one models.dev provider into a Codex catalog.
///
/// Models that cannot call tools are skipped: they cannot drive an agent turn.
pub fn catalog_from_models_dev(
    models_dev: &Value,
    provider_id: &str,
) -> Result<(ProviderDescriptor, ModelsResponse)> {
    let provider = models_dev
        .get(provider_id)
        .ok_or_else(|| anyhow!("provider `{provider_id}` not found on models.dev"))?;
    let base_url = provider
        .get("api")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("models.dev provider `{provider_id}` has no OpenAI-compatible `api` URL")
        })?
        .trim_end_matches('/')
        .to_string();
    let descriptor = ProviderDescriptor {
        id: provider_id.to_string(),
        name: provider
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(provider_id)
            .to_string(),
        base_url,
        env_key: provider
            .get("env")
            .and_then(Value::as_array)
            .and_then(|env| env.first())
            .and_then(Value::as_str)
            .map(str::to_string),
    };

    let mut entries: Vec<(&str, &Value)> = provider
        .get("models")
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .filter(|(_, m)| m.get("tool_call").and_then(Value::as_bool) == Some(true))
                .map(|(id, m)| (id.as_str(), m))
                .collect()
        })
        .unwrap_or_default();
    // Newest first, so the picker leads with current models.
    entries.sort_by(|(a_id, a), (b_id, b)| {
        let date = |m: &Value| m.get("release_date").and_then(Value::as_str).unwrap_or("").to_string();
        date(b).cmp(&date(a)).then_with(|| a_id.cmp(b_id))
    });

    let models = entries
        .into_iter()
        .enumerate()
        .map(|(rank, (id, model))| model_info_from_models_dev(id, model, rank))
        .collect();
    Ok((descriptor, ModelsResponse { models }))
}

fn model_info_from_models_dev(id: &str, model: &Value, rank: usize) -> ModelInfo {
    let mut info = base_model_info(id);
    info.display_name = model
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    info.description = model
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    info.priority = i32::try_from(rank).unwrap_or(i32::MAX);
    if let Some(context) = model
        .get("limit")
        .and_then(|l| l.get("context"))
        .and_then(Value::as_i64)
        .filter(|c| *c > 0)
    {
        info.context_window = Some(context);
        info.max_context_window = Some(context);
    }
    let accepts_images = model
        .get("modalities")
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .is_some_and(|input| input.iter().any(|v| v.as_str() == Some("image")));
    info.input_modalities = if accepts_images {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };
    if model.get("reasoning").and_then(Value::as_bool) == Some(true) {
        let levels = reasoning_levels(model);
        info.default_reasoning_level = levels
            .iter()
            .find(|p| p.effort == ReasoningEffort::Medium)
            .or_else(|| levels.last())
            .map(|p| p.effort.clone());
        info.supported_reasoning_levels = levels;
    }
    info
}

/// The chat adapter forwards only low/medium/high, so only those are offered, and
/// only when models.dev declares them: an undeclared level could make a provider
/// reject every request carrying `reasoning_effort`.
fn reasoning_levels(model: &Value) -> Vec<ReasoningEffortPreset> {
    let declared: Vec<String> = model
        .get("reasoning_options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        .filter_map(|option| option.get("values").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    [
        (ReasoningEffort::Low, "low", "Fast, shallow reasoning"),
        (ReasoningEffort::Medium, "medium", "Balanced reasoning"),
        (ReasoningEffort::High, "high", "Deep reasoning"),
    ]
    .into_iter()
    .filter(|(_, name, _)| declared.iter().any(|d| d == name))
    .map(|(effort, _, description)| ReasoningEffortPreset {
        effort,
        description: description.to_string(),
    })
    .collect()
}

/// Builds a catalog from a running OpenAI-compatible server.
///
/// `models` is the body of `GET {base_url}/models`; `props` is llama.cpp's
/// `GET /props` when available, whose `n_ctx` is the context actually allocated
/// (the model's training context is often far larger than what the server runs).
/// `vision` forces image input on for servers that cannot report it (vLLM, LM Studio).
pub fn catalog_from_local_server(
    models: &Value,
    props: Option<&Value>,
    vision: bool,
) -> Result<ModelsResponse> {
    let data = models
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("`/models` response has no `data` array"))?;
    let runtime_ctx = props
        .and_then(|p| p.get("default_generation_settings"))
        .and_then(|s| s.get("n_ctx"))
        .and_then(Value::as_i64)
        .filter(|n| *n > 0);
    let accepts_images = props
        .and_then(|p| p.get("modalities"))
        .and_then(|m| m.get("vision"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || vision;
    let models = data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .enumerate()
        .map(|(rank, id)| {
            let mut info = base_model_info(id);
            info.priority = i32::try_from(rank).unwrap_or(i32::MAX);
            if let Some(ctx) = runtime_ctx {
                info.context_window = Some(ctx);
                info.max_context_window = Some(ctx);
            }
            info.input_modalities = if accepts_images {
                vec![InputModality::Text, InputModality::Image]
            } else {
                vec![InputModality::Text]
            };
            info
        })
        .collect();
    Ok(ModelsResponse { models })
}

fn base_model_info(slug: &str) -> ModelInfo {
    let mut info = model_info_from_slug(slug);
    info.visibility = ModelVisibility::List;
    // The chat adapter exposes freeform tools as a function with an `input` string.
    info.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    // Chat Completions has no reasoning-summary parameter.
    info.supports_reasoning_summary_parameter = false;
    info.supported_reasoning_levels = Vec::new();
    info.default_reasoning_level = None;
    // MCP tools are loaded on demand through Codex's client-executed `tool_search`,
    // which the chat adapter exposes as a function: the first request stays small.
    info.supports_search_tool = true;
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "novita-ai": {
                "name": "NovitaAI",
                "api": "https://api.novita.ai/openai/",
                "env": ["NOVITA_API_KEY"],
                "models": {
                    "zai-org/glm-5.3-flash": {
                        "name": "GLM-5.3 Flash", "tool_call": true, "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": ["none", "high"]}],
                        "release_date": "2026-09-01",
                        "modalities": {"input": ["text"]},
                        "limit": {"context": 1000000, "output": 131072}
                    },
                    "old/vision-model": {
                        "name": "Old Vision", "tool_call": true, "reasoning": false,
                        "release_date": "2025-01-01",
                        "modalities": {"input": ["text", "image"]},
                        "limit": {"context": 128000}
                    },
                    "embed/no-tools": {"name": "Embedder", "tool_call": false}
                }
            }
        })
    }

    #[test]
    fn converts_models_dev_provider() {
        let (provider, catalog) = catalog_from_models_dev(&sample(), "novita-ai").expect("catalog");
        assert_eq!(
            provider,
            ProviderDescriptor {
                id: "novita-ai".to_string(),
                name: "NovitaAI".to_string(),
                base_url: "https://api.novita.ai/openai".to_string(),
                env_key: Some("NOVITA_API_KEY".to_string()),
            }
        );
        let slugs: Vec<&str> = catalog.models.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, vec!["zai-org/glm-5.3-flash", "old/vision-model"]);

        let glm = &catalog.models[0];
        assert_eq!(glm.context_window, Some(1_000_000));
        assert_eq!(glm.input_modalities, vec![InputModality::Text]);
        let efforts: Vec<ReasoningEffort> = glm
            .supported_reasoning_levels
            .iter()
            .map(|p| p.effort.clone())
            .collect();
        assert_eq!(efforts, vec![ReasoningEffort::High]);
        assert_eq!(glm.default_reasoning_level, Some(ReasoningEffort::High));
        assert_eq!(glm.visibility, ModelVisibility::List);

        let vision = &catalog.models[1];
        assert_eq!(
            vision.input_modalities,
            vec![InputModality::Text, InputModality::Image]
        );
        assert!(vision.supported_reasoning_levels.is_empty());
    }

    #[test]
    fn undeclared_reasoning_levels_are_not_invented() {
        let data = json!({"p": {"api": "https://x/v1", "models": {
            "m": {"name": "M", "tool_call": true, "reasoning": true}
        }}});
        let (_, catalog) = catalog_from_models_dev(&data, "p").expect("catalog");
        assert!(catalog.models[0].supported_reasoning_levels.is_empty());
        assert_eq!(catalog.models[0].default_reasoning_level, None);
        assert!(validate_provider_id("novita-ai").is_ok());
        assert!(validate_provider_id("a.b").is_err());
        assert!(validate_provider_id("../x").is_err());
    }

    #[test]
    fn local_server_uses_runtime_context() {
        let models = json!({"data": [{"id": "qwen3.6-35b-heretic", "meta": {"n_ctx_train": 262144}}]});
        let props = json!({"default_generation_settings": {"n_ctx": 65536}, "modalities": {"vision": false}});
        let catalog = catalog_from_local_server(&models, Some(&props), false).expect("catalog");
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].slug, "qwen3.6-35b-heretic");
        assert_eq!(catalog.models[0].context_window, Some(65_536));
    }

    #[test]
    fn profile_config_puts_top_level_keys_first() {
        let (provider, _) = catalog_from_models_dev(&sample(), "novita-ai").expect("catalog");
        let config = provider.profile_config("/tmp/novita-ai.json", Some("zai-org/glm-5.3-flash"));
        assert_eq!(
            config,
            "model_provider = \"novita-ai\"\n\
             model_catalog_json = \"/tmp/novita-ai.json\"\n\
             model = \"zai-org/glm-5.3-flash\"\n\
             \n\
             [model_providers.\"novita-ai\"]\n\
             name = \"NovitaAI\"\n\
             base_url = \"https://api.novita.ai/openai\"\n\
             wire_api = \"chat\"\n\
             env_key = \"NOVITA_API_KEY\"\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requested_destination_goes_through_the_shared_transport_policy() {
        struct OrigenActivo;

        impl Drop for OrigenActivo {
            fn drop(&mut self) {
                codex_http_client::eslabon_policy::set_origin_policy(false);
            }
        }

        // eslabon: activating the rule here proves `fetch_json` consults the
        // shared egress policy instead of a private reqwest client.
        let _origen = OrigenActivo;
        codex_http_client::eslabon_policy::set_origin_policy(true);
        let error = fetch_json("https://api.openai.com/v1/models")
            .await
            .expect_err("OpenAI hosts are denied while the origin rule is on");
        let message = format!("{error:#}");
        assert!(message.contains("eslabon origin policy"), "{message}");
        assert!(message.contains("permitir_openai"), "{message}");
    }
}
