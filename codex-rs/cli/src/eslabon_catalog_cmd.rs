//! eslabon: `codex catalog` — generate `model_catalog_json` files from models.dev
//! or from a running local OpenAI-compatible server.

use anyhow::Context;
use clap::Parser;
use clap::Subcommand;
use codex_core::config::find_codex_home;
use eslabon_catalog::MODELS_DEV_URL;
use eslabon_catalog::ProviderDescriptor;
use eslabon_catalog::catalog_from_local_server;
use eslabon_catalog::catalog_from_models_dev;
use eslabon_catalog::fetch_json;
use eslabon_catalog::validate_provider_id;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct CatalogCommand {
    #[command(subcommand)]
    action: CatalogAction,
}

#[derive(Debug, Subcommand)]
enum CatalogAction {
    /// Build a catalog for a hosted provider listed on models.dev (e.g. `novita-ai`, `deepseek`).
    Sync {
        /// models.dev provider id.
        provider: String,
        /// Where to write the catalog. Defaults to `$CODEX_HOME/catalogs/<provider>.json`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Build a catalog from a running OpenAI-compatible server (llama.cpp, vLLM, LM Studio).
    Local {
        /// Provider id to use in config.toml (e.g. `heretic`).
        id: String,
        /// Base URL including `/v1`, e.g. `http://127.0.0.1:11434/v1`.
        base_url: String,
        #[arg(long)]
        output: Option<PathBuf>,
        /// Mark the models as accepting images (servers without llama.cpp's `/props`).
        #[arg(long)]
        vision: bool,
    },
    /// List provider ids available on models.dev.
    Providers,
    /// Rebuild the merged catalog (what /model lists) from the per-provider catalogs.
    Rebuild,
}

pub async fn run(command: CatalogCommand) -> anyhow::Result<()> {
    // eslabon: `catalog` never loads Config; point the shared policy at this
    // home so the PHI switch file (and any origin rule) still applies to its
    // requests instead of the transport's unrestricted default.
    let codex_home = find_codex_home()?.into_path_buf();
    codex_http_client::eslabon_policy::set_switch_dir(&codex_home);
    match command.action {
        CatalogAction::Rebuild => {
            let codex_home = find_codex_home()?.into_path_buf();
            let (models, providers) = rebuild_merged(&codex_home)?;
            println!("/model now lists {models} models from {providers} providers.");
        }
        CatalogAction::Providers => {
            let models_dev = fetch_json(MODELS_DEV_URL).await?;
            let mut ids: Vec<&String> = models_dev
                .as_object()
                .map(|o| o.keys().collect())
                .unwrap_or_default();
            ids.sort();
            for id in ids {
                println!("{id}");
            }
        }
        CatalogAction::Sync { provider, output } => {
            validate_provider_id(&provider)?;
            let models_dev = fetch_json(MODELS_DEV_URL).await?;
            let (descriptor, catalog) = catalog_from_models_dev(&models_dev, &provider)?;
            let first = catalog.models.first().map(|m| m.slug.clone());
            let path = write_catalog(&provider, output, &catalog)?;
            report(&descriptor, &path, catalog.models.len(), first.as_deref());
        }
        CatalogAction::Local {
            id,
            base_url,
            output,
            vision,
        } => {
            validate_provider_id(&id)?;
            let base_url = base_url.trim_end_matches('/').to_string();
            let models = fetch_json(&format!("{base_url}/models")).await?;
            // llama.cpp serves /props next to /v1; other servers simply 404.
            let props_url = format!("{}/props", base_url.trim_end_matches("/v1"));
            let props = fetch_json(&props_url).await.ok();
            let catalog = catalog_from_local_server(&models, props.as_ref(), vision)?;
            let first = catalog.models.first().map(|m| m.slug.clone());
            let descriptor = ProviderDescriptor {
                name: id.clone(),
                id: id.clone(),
                base_url,
                env_key: None,
            };
            let path = write_catalog(&id, output, &catalog)?;
            report(&descriptor, &path, catalog.models.len(), first.as_deref());
        }
    }
    Ok(())
}

fn write_catalog(
    id: &str,
    output: Option<PathBuf>,
    catalog: &codex_protocol::openai_models::ModelsResponse,
) -> anyhow::Result<PathBuf> {
    let path = match output {
        Some(path) => path,
        None => catalogs_dir()?.join(format!("{id}.json")),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(catalog)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn catalogs_dir() -> anyhow::Result<PathBuf> {
    Ok(find_codex_home()?.into_path_buf().join("catalogs"))
}

/// Registers the provider in config.toml, then rebuilds the merged catalog that
/// `/model` shows and the model → provider index eslabon uses to switch providers.
fn report(
    descriptor: &ProviderDescriptor,
    path: &std::path::Path,
    count: usize,
    _first: Option<&str>,
) {
    println!("Wrote {count} models to {}", path.display());
    match register(descriptor) {
        Ok((models, providers)) => println!(
            "Registered provider `{}`. /model now lists {models} models from {providers} providers.",
            descriptor.id
        ),
        Err(err) => println!("Could not update the merged catalog: {err:#}"),
    }
}

fn register(descriptor: &ProviderDescriptor) -> anyhow::Result<(usize, usize)> {
    use codex_core::config::edit::ConfigEdit;
    use toml_edit::value;
    let codex_home = find_codex_home()?.into_path_buf();
    let table = |key: &str| {
        vec![
            "model_providers".to_string(),
            descriptor.id.clone(),
            key.to_string(),
        ]
    };
    let mut edits = vec![
        ConfigEdit::SetPath {
            segments: table("name"),
            value: value(descriptor.name.clone()),
        },
        ConfigEdit::SetPath {
            segments: table("base_url"),
            value: value(descriptor.base_url.clone()),
        },
        ConfigEdit::SetPath {
            segments: table("wire_api"),
            value: value("chat"),
        },
    ];
    if let Some(env_key) = &descriptor.env_key {
        edits.push(ConfigEdit::SetPath {
            segments: table("env_key"),
            value: value(env_key.clone()),
        });
    }
    codex_core::config::edit::apply_blocking(&codex_home, &edits)?;
    rebuild_merged(&codex_home)
}

/// Merges every per-provider catalog (catalogs/<provider>.json) into catalogs/all.json,
/// writes the model → provider index and points `model_catalog_json` at the merge.
fn rebuild_merged(codex_home: &std::path::Path) -> anyhow::Result<(usize, usize)> {
    use codex_core::config::edit::ConfigEdit;
    use toml_edit::value;
    let dir = codex_home.join("catalogs");
    let mut merged = Vec::new();
    let mut index = serde_json::Map::new();
    let mut providers = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    for path in entries {
        let Some(provider) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if matches!(provider, "all" | "providers-index") {
            continue;
        }
        let Ok(catalog) = serde_json::from_slice::<codex_protocol::openai_models::ModelsResponse>(
            &std::fs::read(&path)?,
        ) else {
            continue;
        };
        providers += 1;
        // Hand-added models (catalogs/_extras/<provider>.json) survive `catalog sync`
        // and come first, so a provider's preferred default leads its list.
        let extras = std::fs::read(dir.join("_extras").join(format!("{provider}.json")))
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice::<codex_protocol::openai_models::ModelsResponse>(&bytes).ok()
            })
            .map(|extras| extras.models)
            .unwrap_or_default();
        for mut model in extras.into_iter().chain(catalog.models) {
            if index.contains_key(&model.slug) {
                eprintln!(
                    "skipping duplicate model `{}` from `{provider}`",
                    model.slug
                );
                continue;
            }
            index.insert(
                model.slug.clone(),
                serde_json::Value::String(provider.to_string()),
            );
            model.description = Some(match model.description.take() {
                Some(description) => format!("[{provider}] {description}"),
                None => format!("[{provider}]"),
            });
            model.priority = i32::try_from(merged.len()).unwrap_or(i32::MAX);
            merged.push(model);
        }
    }
    // Keep the built-in OpenAI models (ChatGPT login / API key) selectable too.
    if let Ok(bundled) = codex_models_manager::bundled_models_response() {
        providers += 1;
        for mut model in bundled.models {
            if index.contains_key(&model.slug) {
                continue;
            }
            index.insert(
                model.slug.clone(),
                serde_json::Value::String("openai".to_string()),
            );
            model.description = Some(match model.description.take() {
                Some(description) => format!("[openai] {description}"),
                None => "[openai]".to_string(),
            });
            model.priority = i32::try_from(merged.len()).unwrap_or(i32::MAX);
            merged.push(model);
        }
    }
    let count = merged.len();
    let all = dir.join("all.json");
    std::fs::write(
        &all,
        serde_json::to_vec_pretty(&codex_protocol::openai_models::ModelsResponse {
            models: merged,
        })?,
    )?;
    std::fs::write(
        dir.join("providers-index.json"),
        serde_json::to_vec_pretty(&index)?,
    )?;
    codex_core::config::edit::apply_blocking(
        codex_home,
        &[ConfigEdit::SetPath {
            segments: vec!["model_catalog_json".to_string()],
            value: value(all.display().to_string()),
        }],
    )?;
    Ok((count, providers))
}
