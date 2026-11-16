//! Unattended homes: a separate `CODEX_HOME` for runs nobody watches (benchmarks,
//! batch jobs). It carries only the model, its provider and catalog; MCP servers,
//! hooks, plugins and ChatGPT connectors are off by construction, and there is no
//! global AGENTS.md. `eslabon doctor` checks that it stays that way.

use crate::Perfil;
use crate::perfiles;
use anyhow::Context;
use anyhow::bail;
use std::path::Path;
use toml::Table;
use toml::Value;

/// Keys copied from the source profile when present.
const COPIED_KEYS: [&str; 4] = [
    "model",
    "model_provider",
    "model_context_window",
    "model_reasoning_effort",
];

/// Creates `destino` as an unattended home from `origen` (optionally one of its
/// profiles). Returns the path of the config it wrote.
pub fn crear(origen: &Path, perfil: Option<&str>, destino: &Path) -> anyhow::Result<std::path::PathBuf> {
    let config_path = destino.join("config.toml");
    // Only a new or empty directory: nothing in it gets overwritten or followed.
    if std::fs::read_dir(destino).is_ok_and(|mut entries| entries.next().is_some()) {
        bail!(
            "{} ya existe y no está vacío: no se sobrescribe un home (elige otro destino)",
            destino.display()
        );
    }
    let fuente = buscar_perfil(origen, perfil)?;
    if fuente.str("model").is_none() {
        bail!("{} no define `model`", fuente.etiqueta());
    }

    let mut config = Table::new();
    for key in COPIED_KEYS {
        if let Some(value) = fuente.config.get(key) {
            config.insert(key.to_string(), value.clone());
        }
    }

    std::fs::create_dir_all(destino)
        .with_context(|| format!("no se pudo crear {}", destino.display()))?;
    if let Some(catalog) = fuente.str("model_catalog_json") {
        let copia = destino.join("catalogo.json");
        std::fs::copy(catalog, &copia)
            .with_context(|| format!("no se pudo copiar el catálogo {catalog}"))?;
        config.insert(
            "model_catalog_json".to_string(),
            Value::String(copia.display().to_string()),
        );
    }

    // A copy, so later edits to the source home do not change unattended runs.
    if let Some(instructions) = fuente.str("model_instructions_file") {
        let copia = destino.join("instrucciones.md");
        std::fs::copy(instructions, &copia)
            .with_context(|| format!("no se pudieron copiar las instrucciones {instructions}"))?;
        config.insert(
            "model_instructions_file".to_string(),
            Value::String(copia.display().to_string()),
        );
    }

    if let Some((id, provider)) = fuente.proveedor() {
        let mut providers = Table::new();
        providers.insert(id.to_string(), Value::Table(provider.clone()));
        config.insert("model_providers".to_string(), Value::Table(providers));
    }

    let mut features = Table::new();
    for key in ["hooks", "plugins", "apps", "remote_plugin"] {
        features.insert(key.to_string(), Value::Boolean(false));
    }
    config.insert("features".to_string(), Value::Table(features));

    let mut eslabon = Table::new();
    eslabon.insert("desatendido".to_string(), Value::Boolean(true));
    if let Some(policy) = fuente.tabla("eslabon") {
        for key in ["data_policy", "phi_providers"] {
            if let Some(value) = policy.get(key) {
                eslabon.insert(key.to_string(), value.clone());
            }
        }
    }
    config.insert("eslabon".to_string(), Value::Table(eslabon));

    let header = format!(
        "# Home desatendido creado por `eslabon desatendido` desde {} ({}).\n# Sin MCP, hooks, plugins ni AGENTS.md global. Verificar con `eslabon revisar`.\n# Uso: CODEX_HOME={} eslabon ...\n\n",
        origen.display(),
        fuente.etiqueta(),
        destino.display()
    );
    let body = toml::to_string_pretty(&config).context("no se pudo serializar la config")?;
    std::fs::write(&config_path, header + &body)
        .with_context(|| format!("no se pudo escribir {}", config_path.display()))?;
    Ok(config_path)
}

fn buscar_perfil(origen: &Path, perfil: Option<&str>) -> anyhow::Result<Perfil> {
    let todos = perfiles(origen)?;
    let encontrado = todos
        .into_iter()
        .find(|candidate| candidate.nombre.as_deref() == perfil);
    match (encontrado, perfil) {
        (Some(found), _) => Ok(found),
        (None, Some(nombre)) => bail!(
            "no existe el perfil `{nombre}` en {} (busca {nombre}.config.toml)",
            origen.display()
        ),
        (None, None) => bail!("{} no tiene config.toml", origen.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::Nivel;
    use crate::doctor::revisar;
    use crate::sondeo::Sondeo;
    use pretty_assertions::assert_eq;

    struct SinRed;
    impl Sondeo for SinRed {
        fn get(&self, _url: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn unattended_home_keeps_the_model_and_drops_everything_else() {
        let origen = tempfile::tempdir().expect("origen");
        let catalog = origen.path().join("catalog.json");
        std::fs::write(&catalog, r#"{"models":[{"slug":"qwen-coder"}]}"#).expect("catalog");
        let instructions = origen.path().join("instrucciones-base.md");
        std::fs::write(&instructions, "Sé breve.").expect("instructions");
        std::fs::write(
            origen.path().join("config.toml"),
            r#"
model = "glm"
model_provider = "novita"
[model_providers.novita]
base_url = "https://api.novita.ai/openai"
wire_api = "chat"
[model_providers.local]
base_url = "http://127.0.0.1:11434/v1"
wire_api = "chat"
stream_idle_timeout_ms = 1800000
[mcp_servers.vps]
command = "vps"
[features]
hooks = true
"#,
        )
        .expect("config");
        std::fs::write(
            origen.path().join("flashnext.config.toml"),
            format!(
                "model = \"qwen-coder\"\nmodel_provider = \"local\"\nmodel_context_window = 131072\nmodel_catalog_json = \"{}\"\nmodel_instructions_file = \"{}\"\n",
                catalog.display(),
                instructions.display()
            ),
        )
        .expect("profile");
        std::fs::write(origen.path().join("AGENTS.md"), "reglas globales").expect("agents");

        let destino = tempfile::tempdir().expect("destino");
        let home = destino.path().join("p5");
        let written = crear(origen.path(), Some("flashnext"), &home).expect("crear");
        let config = crate::leer_toml(&written).expect("toml");

        assert_eq!(config["model"].as_str(), Some("qwen-coder"));
        assert_eq!(config["model_provider"].as_str(), Some("local"));
        assert_eq!(config["model_context_window"].as_integer(), Some(131072));
        let providers = config["model_providers"].as_table().expect("providers");
        assert_eq!(providers.keys().collect::<Vec<_>>(), vec!["local"]);
        assert!(config.get("mcp_servers").is_none());
        assert_eq!(config["eslabon"]["desatendido"].as_bool(), Some(true));
        assert!(home.join("catalogo.json").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("instrucciones.md")).expect("copia"),
            "Sé breve."
        );
        assert_eq!(
            config["model_instructions_file"].as_str(),
            Some(home.join("instrucciones.md").display().to_string().as_str())
        );
        assert!(!home.join("AGENTS.md").exists());

        // The doctor finds nothing to fix in what we just built.
        let hallazgos = revisar(&home, &SinRed);
        assert!(
            hallazgos.iter().all(|h| h.nivel == Nivel::Info),
            "{hallazgos:?}"
        );

        assert!(crear(origen.path(), Some("flashnext"), &home).is_err(), "no sobrescribe");
        assert!(crear(origen.path(), Some("inexistente"), &destino.path().join("x")).is_err());
        let ocupado = destino.path().join("ocupado");
        std::fs::create_dir_all(&ocupado).expect("dir");
        std::fs::write(ocupado.join("catalogo.json"), "mío").expect("file");
        assert!(crear(origen.path(), Some("flashnext"), &ocupado).is_err(), "no pisa archivos");
        assert_eq!(
            std::fs::read_to_string(ocupado.join("catalogo.json")).expect("intacto"),
            "mío"
        );
    }
}
