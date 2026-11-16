//! eslabon: configuration layer on top of Codex's TOML.
//!
//! Codex keeps loading `config.toml` and `<profile>.config.toml` as usual; this crate
//! reads the same files to verify them (`eslabon doctor`) and to build homes with safe
//! defaults (unattended homes without MCP servers, hooks or plugins). It never replaces
//! the Codex loader, so upstream merges stay cheap.

pub mod desatendido;
pub mod detector;
pub mod doctor;
pub mod sondeo;
pub mod servidor_local;

use anyhow::Context;
use std::path::Path;
use std::path::PathBuf;
use toml::Table;
use toml::Value;

/// Base config plus one profile layered on top, as Codex merges them: tables merge
/// key by key, any other value is replaced.
#[derive(Debug, Clone)]
pub struct Perfil {
    /// `None` for the base `config.toml`.
    pub nombre: Option<String>,
    pub config: Table,
}

impl Perfil {
    pub fn etiqueta(&self) -> String {
        match &self.nombre {
            Some(nombre) => format!("perfil `{nombre}`"),
            None => "config.toml".to_string(),
        }
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(Value::as_str)
    }

    pub fn tabla(&self, key: &str) -> Option<&Table> {
        self.config.get(key).and_then(Value::as_table)
    }

    /// The `[model_providers.<id>]` table of the selected provider, if it is configured
    /// here (built-in providers such as `openai` have none).
    pub fn proveedor(&self) -> Option<(&str, &Table)> {
        let id = self.str("model_provider")?;
        let tabla = self.tabla("model_providers")?.get(id)?.as_table()?;
        Some((id, tabla))
    }

    /// `[eslabon] desatendido = true`: the home or profile runs unattended.
    pub fn desatendido(&self) -> bool {
        self.tabla("eslabon")
            .and_then(|eslabon| eslabon.get("desatendido"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

pub fn leer_toml(path: &Path) -> anyhow::Result<Table> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("no se pudo leer {}", path.display()))?;
    text.parse::<Table>()
        .with_context(|| format!("{} no es TOML válido", path.display()))
}

/// Deep-merges `encima` into `base`.
pub fn fusionar(base: &mut Table, encima: &Table) {
    for (key, value) in encima {
        match (base.get_mut(key), value) {
            (Some(Value::Table(existing)), Value::Table(overlay)) => fusionar(existing, overlay),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

/// The base config and every `<name>.config.toml` profile of a home, each merged.
pub fn perfiles(home: &Path) -> anyhow::Result<Vec<Perfil>> {
    let base_path = home.join("config.toml");
    let base = if base_path.exists() {
        leer_toml(&base_path)?
    } else {
        Table::new()
    };
    let mut perfiles = vec![Perfil {
        nombre: None,
        config: base.clone(),
    }];
    let mut nombres: Vec<(String, PathBuf)> = std::fs::read_dir(home)
        .with_context(|| format!("no se pudo leer el home {}", home.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let nombre = file_name.strip_suffix(".config.toml")?.to_string();
            (!nombre.is_empty()).then(|| (nombre, entry.path()))
        })
        .collect();
    nombres.sort();
    for (nombre, path) in nombres {
        let mut config = base.clone();
        fusionar(&mut config, &leer_toml(&path)?);
        perfiles.push(Perfil {
            nombre: Some(nombre),
            config,
        });
    }
    Ok(perfiles)
}

/// The home eslabon uses in Eslabon mode: `ESLABON_HOME` when set, otherwise
/// `CODEX_HOME` when set, otherwise `~/.eslabon`.
///
/// eslabon: delegates to the same resolver as `find_codex_home` so both always
/// agree; unlike `find_codex_home` it does not validate that the path exists.
pub fn home_por_defecto() -> Option<PathBuf> {
    codex_utils_home_dir::home_eslabon_bruto()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn profiles_merge_tables_and_replace_values() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join("config.toml"),
            "model = \"a\"\n[model_providers.x]\nbase_url = \"http://x\"\nwire_api = \"chat\"\n",
        )
        .expect("write");
        std::fs::write(
            home.path().join("rapido.config.toml"),
            "model = \"b\"\n[model_providers.x]\nwire_api = \"responses\"\n",
        )
        .expect("write");
        let perfiles = perfiles(home.path()).expect("perfiles");
        assert_eq!(perfiles.len(), 2);
        assert_eq!(perfiles[1].nombre.as_deref(), Some("rapido"));
        assert_eq!(perfiles[1].str("model"), Some("b"));
        let x = perfiles[1].tabla("model_providers").expect("providers")["x"]
            .as_table()
            .expect("x");
        assert_eq!(x["base_url"].as_str(), Some("http://x"));
        assert_eq!(x["wire_api"].as_str(), Some("responses"));
        assert_eq!(perfiles[0].str("model"), Some("a"));
    }

    #[test]
    fn home_por_defecto_coincide_con_la_resolucion_de_modo_eslabon() {
        let eslabon = std::env::var("ESLABON_HOME").ok();
        let codex = std::env::var("CODEX_HOME").ok();
        let home = codex_utils_home_dir::home_dir_usuario();
        let esperado = codex_utils_home_dir::resolver_home(
            /*modo_eslabon*/ true,
            eslabon.as_deref(),
            codex.as_deref(),
            home.as_deref(),
        )
        .ok()
        .map(|(path, _)| path);
        assert_eq!(home_por_defecto(), esperado);
    }
}
