//! `eslabon doctor`: checks a home for the misconfigurations that have cost time,
//! without running a model. Local servers are only asked read-only questions.

use crate::Perfil;
use crate::perfiles;
use crate::sondeo::Sondeo;
use crate::sondeo::es_loopback;
use serde_json::Value as Json;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use toml::Value;

/// Prefill of ~100k tokens on the local host takes 10+ minutes; less idle time cancels it.
const MIN_LOCAL_STREAM_IDLE_MS: i64 = 900_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Nivel {
    Error,
    Aviso,
    Info,
}

impl fmt::Display for Nivel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Nivel::Error => "ERROR",
            Nivel::Aviso => "AVISO",
            Nivel::Info => "info",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hallazgo {
    pub nivel: Nivel,
    pub donde: String,
    pub mensaje: String,
}

impl fmt::Display for Hallazgo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:5} {}: {}", self.nivel, self.donde, self.mensaje)
    }
}

pub fn revisar(home: &Path, sondeo: &dyn Sondeo) -> Vec<Hallazgo> {
    let mut out = Vec::new();
    let home_label = home.display().to_string();
    if !home.join("config.toml").exists() {
        out.push(Hallazgo {
            nivel: Nivel::Error,
            donde: home_label,
            mensaje: "no hay config.toml en este home (¿CODEX_HOME apunta donde crees?)"
                .to_string(),
        });
        return out;
    }
    out.push(Hallazgo {
        nivel: Nivel::Info,
        donde: home_label.clone(),
        mensaje: "home en uso".to_string(),
    });
    let perfiles = match perfiles(home) {
        Ok(perfiles) => perfiles,
        Err(err) => {
            out.push(Hallazgo {
                nivel: Nivel::Error,
                donde: home_label,
                mensaje: format!("{err:#}"),
            });
            return out;
        }
    };
    // A server is asked once even if several profiles use it.
    let mut sondeados = BTreeSet::new();
    // Which profile asks for which model on each server: on a shared host, a server
    // serving another profile's model is expected, not suspicious.
    let mut pedidos: Vec<(String, String, String)> = Vec::new();
    for perfil in &perfiles {
        if let (Some(model), Some((_, provider))) = (perfil.str("model"), perfil.proveedor())
            && let Some(base_url) = provider.get("base_url").and_then(Value::as_str)
        {
            pedidos.push((
                base_url.trim_end_matches('/').to_string(),
                model.to_string(),
                perfil.etiqueta(),
            ));
        }
    }
    for perfil in &perfiles {
        revisar_perfil(home, perfil, sondeo, &pedidos, &mut sondeados, &mut out);
    }
    out
}

fn revisar_perfil(
    home: &Path,
    perfil: &Perfil,
    sondeo: &dyn Sondeo,
    pedidos: &[(String, String, String)],
    sondeados: &mut BTreeSet<(String, String)>,
    out: &mut Vec<Hallazgo>,
) {
    let donde = perfil.etiqueta();
    let mut push = |nivel, mensaje: String| {
        out.push(Hallazgo {
            nivel,
            donde: donde.clone(),
            mensaje,
        })
    };

    if perfil.desatendido() {
        revisar_desatendido(home, perfil, &mut push);
    }

    let Some(model) = perfil.str("model") else {
        return;
    };
    let provider_id = perfil.str("model_provider").unwrap_or("openai");
    let declared = perfil
        .tabla("model_providers")
        .is_some_and(|providers| providers.contains_key(provider_id));
    if !declared && provider_id != "openai" && provider_id != "oss" {
        push(
            Nivel::Error,
            format!("model_provider `{provider_id}` no está definido en [model_providers]"),
        );
        return;
    }
    let provider = perfil.proveedor().map(|(_, table)| table);
    let base_url = provider
        .and_then(|table| table.get("base_url"))
        .and_then(Value::as_str);
    let wire = provider
        .and_then(|table| table.get("wire_api"))
        .and_then(Value::as_str)
        .unwrap_or("responses");
    let openai_like = base_url.is_none_or(|url| url.contains("openai.com") || url.contains("chatgpt.com"));

    // The model in the catalog.
    let catalog_entry = perfil.str("model_catalog_json").and_then(|path| {
        let path = Path::new(path);
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Json>(&text).ok());
        let Some(catalog) = parsed else {
            push(
                Nivel::Error,
                format!("model_catalog_json `{}` no existe o no es JSON", path.display()),
            );
            return None;
        };
        let entry = catalog
            .get("models")
            .and_then(Json::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .find(|m| m.get("slug").and_then(Json::as_str) == Some(model))
                    .cloned()
            });
        if entry.is_none() {
            push(
                Nivel::Aviso,
                format!(
                    "`{model}` no está en el catálogo {}: Codex usará metadatos genéricos",
                    path.display()
                ),
            );
        }
        entry
    });
    let supports_search = catalog_entry
        .as_ref()
        .and_then(|entry| entry.get("supports_search_tool"))
        .and_then(Json::as_bool)
        .unwrap_or(false);

    // Wire and tool exposure.
    if wire == "responses" && !openai_like {
        let detail = if supports_search {
            " y con supports_search_tool las herramientas quedan detrás de un tool_search que ese servidor no ejecuta"
        } else {
            ""
        };
        push(
            Nivel::Error,
            format!(
                "`{provider_id}` usa wire_api = \"responses\" fuera de OpenAI: servidores como llama.cpp descartan las herramientas MCP (van en `namespace`){detail}; usa wire_api = \"chat\""
            ),
        );
    }

    // Local servers: idle timeout, model actually served, real context.
    let Some(base_url) = base_url.filter(|url| es_loopback(url)) else {
        return;
    };
    let idle = provider
        .and_then(|table| table.get("stream_idle_timeout_ms"))
        .and_then(Value::as_integer);
    if idle.is_none_or(|ms| ms < MIN_LOCAL_STREAM_IDLE_MS) {
        push(
            Nivel::Aviso,
            format!(
                "`{provider_id}` es local y stream_idle_timeout_ms = {}: un prefill largo se cancela y se reprocesa desde cero; usa ≥ {MIN_LOCAL_STREAM_IDLE_MS}",
                idle.map_or("(por defecto 300000)".to_string(), |ms| ms.to_string())
            ),
        );
    }
    let configured_context = perfil
        .config
        .get("model_context_window")
        .and_then(Value::as_integer)
        .or_else(|| {
            catalog_entry
                .as_ref()
                .and_then(|entry| entry.get("context_window"))
                .and_then(Json::as_i64)
        });
    if !sondeados.insert((base_url.to_string(), model.to_string())) {
        return;
    }
    let base = base_url.trim_end_matches('/');
    let Some(models_body) = sondeo.get(&format!("{base}/models")) else {
        push(
            Nivel::Info,
            format!("{base} no responde: el modelo local está apagado o el túnel caído"),
        );
        return;
    };
    let served: Vec<String> = serde_json::from_str::<Json>(&models_body)
        .ok()
        .and_then(|json| json.get("data").and_then(Json::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m.get("id").and_then(Json::as_str).map(str::to_string))
        .collect();
    if !served.is_empty() && !served.iter().any(|id| id == model) {
        // One model at a time is normal on a shared host: if another profile asks for
        // what is loaded, this one just needs a model switch first. A model no profile
        // asks for is the suspicious case.
        let dueno = pedidos.iter().find(|(url, pedido, _)| {
            url == base && served.iter().any(|id| id == pedido)
        });
        match dueno {
            Some((_, cargado, otro)) => push(
                Nivel::Info,
                format!(
                    "{base} sirve ahora `{cargado}` ({otro}); este perfil pide `{model}`: cambia el modelo de ese host antes de arrancarlo"
                ),
            ),
            None => push(
                Nivel::Aviso,
                format!(
                    "{base} está sirviendo {} y ningún perfil lo pide; este pide `{model}`: si lo arrancas ahora, llama.cpp contestaría con otro modelo sin avisar",
                    served.join(", ")
                ),
            ),
        }
        return;
    }
    let server_root = base.strip_suffix("/v1").unwrap_or(base);
    let server_context = sondeo
        .get(&format!("{server_root}/props"))
        .and_then(|body| serde_json::from_str::<Json>(&body).ok())
        .and_then(|props| {
            props
                .pointer("/default_generation_settings/n_ctx")
                .or_else(|| props.get("n_ctx"))
                .and_then(Json::as_i64)
        });
    if let (Some(configured), Some(real)) = (configured_context, server_context)
        && configured > real
    {
        push(
            Nivel::Error,
            format!(
                "contexto configurado {configured} > contexto real del servidor {real}: la compactación llegará tarde y el servidor cortará"
            ),
        );
    }
}

fn revisar_desatendido(home: &Path, perfil: &Perfil, push: &mut impl FnMut(Nivel, String)) {
    if let Some(servers) = perfil.tabla("mcp_servers") {
        for (name, server) in servers {
            let enabled = server
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if enabled {
                push(
                    Nivel::Error,
                    format!("desatendido con el MCP `{name}` activo (desactívalo con enabled = false)"),
                );
            }
        }
    }
    let feature = |key: &str| {
        perfil
            .tabla("features")
            .and_then(|features| features.get(key))
            .and_then(Value::as_bool)
    };
    for key in ["hooks", "plugins", "apps"] {
        if feature(key) != Some(false) {
            push(
                Nivel::Error,
                format!("desatendido sin `[features] {key} = false`"),
            );
        }
    }
    let global_agents = home.join("AGENTS.md");
    if std::fs::metadata(&global_agents).is_ok_and(|meta| meta.len() > 0) {
        push(
            Nivel::Aviso,
            format!(
                "desatendido con {} global: sus instrucciones entran en cada sesión",
                global_agents.display()
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    struct Falso(HashMap<String, String>);

    impl Sondeo for Falso {
        fn get(&self, url: &str) -> Option<String> {
            self.0.get(url).cloned()
        }
    }

    fn home(config: &str, catalogo: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_path = dir.path().join("cat.json");
        std::fs::write(&catalog_path, catalogo).expect("catalog");
        std::fs::write(
            dir.path().join("config.toml"),
            config.replace("CAT", &catalog_path.display().to_string()),
        )
        .expect("config");
        dir
    }

    fn mensajes(hallazgos: &[Hallazgo], nivel: Nivel) -> Vec<String> {
        hallazgos
            .iter()
            .filter(|h| h.nivel == nivel)
            .map(|h| h.mensaje.clone())
            .collect()
    }

    const LOCAL: &str = r#"
model = "qwen-coder"
model_provider = "local"
model_catalog_json = "CAT"
[model_providers.local]
base_url = "http://127.0.0.1:11434/v1"
wire_api = "chat"
stream_idle_timeout_ms = 1800000
"#;
    const CATALOGO: &str =
        r#"{"models":[{"slug":"qwen-coder","context_window":200000,"supports_search_tool":true}]}"#;

    #[test]
    fn a_healthy_local_profile_only_reports_the_home() {
        let dir = home(LOCAL, r#"{"models":[{"slug":"qwen-coder","context_window":131072}]}"#);
        let falso = Falso(HashMap::from([
            (
                "http://127.0.0.1:11434/v1/models".to_string(),
                r#"{"data":[{"id":"qwen-coder"}]}"#.to_string(),
            ),
            (
                "http://127.0.0.1:11434/props".to_string(),
                r#"{"default_generation_settings":{"n_ctx":131072}}"#.to_string(),
            ),
        ]));
        let hallazgos = revisar(dir.path(), &falso);
        assert_eq!(mensajes(&hallazgos, Nivel::Error), Vec::<String>::new());
        assert_eq!(mensajes(&hallazgos, Nivel::Aviso), Vec::<String>::new());
    }

    #[test]
    fn detects_wrong_model_served_and_context_too_large() {
        let dir = home(LOCAL, CATALOGO);
        let otro_modelo = Falso(HashMap::from([(
            "http://127.0.0.1:11434/v1/models".to_string(),
            r#"{"data":[{"id":"gemma-4"}]}"#.to_string(),
        )]));
        let avisos = mensajes(&revisar(dir.path(), &otro_modelo), Nivel::Aviso);
        assert!(avisos[0].contains("está sirviendo gemma-4 y ningún perfil lo pide"), "{avisos:?}");

        // If another profile of the home asks for what is loaded, it is only info.
        std::fs::write(
            dir.path().join("gemma.config.toml"),
            "model = \"gemma-4\"\n",
        )
        .expect("profile");
        let hallazgos = revisar(dir.path(), &otro_modelo);
        assert!(
            !mensajes(&hallazgos, Nivel::Aviso)
                .iter()
                .any(|m| m.contains("sirviendo")),
            "{hallazgos:?}"
        );
        assert!(
            mensajes(&hallazgos, Nivel::Info)
                .iter()
                .any(|m| m.contains("sirve ahora `gemma-4` (perfil `gemma`)")),
            "{hallazgos:?}"
        );

        let contexto = Falso(HashMap::from([
            (
                "http://127.0.0.1:11434/v1/models".to_string(),
                r#"{"data":[{"id":"qwen-coder"}]}"#.to_string(),
            ),
            (
                "http://127.0.0.1:11434/props".to_string(),
                r#"{"default_generation_settings":{"n_ctx":131072}}"#.to_string(),
            ),
        ]));
        let errores = mensajes(&revisar(dir.path(), &contexto), Nivel::Error);
        assert!(errores[0].contains("200000 > contexto real del servidor 131072"), "{errores:?}");
    }

    #[test]
    fn detects_responses_wire_outside_openai_and_short_idle_timeout() {
        let config = LOCAL
            .replace("wire_api = \"chat\"", "wire_api = \"responses\"")
            .replace("stream_idle_timeout_ms = 1800000\n", "");
        let dir = home(&config, CATALOGO);
        let hallazgos = revisar(dir.path(), &Falso(HashMap::new()));
        let errores = mensajes(&hallazgos, Nivel::Error);
        assert!(errores[0].contains("wire_api = \"responses\" fuera de OpenAI"), "{errores:?}");
        assert!(errores[0].contains("tool_search"), "{errores:?}");
        assert!(mensajes(&hallazgos, Nivel::Aviso)[0].contains("stream_idle_timeout_ms"));
        assert!(mensajes(&hallazgos, Nivel::Info)[1].contains("no responde"));
    }

    #[test]
    fn unattended_home_rejects_mcp_hooks_and_plugins() {
        let config = format!(
            "{LOCAL}\n[eslabon]\ndesatendido = true\n[features]\nhooks = false\n[mcp_servers.vps]\ncommand = \"vps\"\n[mcp_servers.apagado]\ncommand = \"x\"\nenabled = false\n"
        );
        let dir = home(&config, CATALOGO);
        std::fs::write(dir.path().join("AGENTS.md"), "reglas").expect("agents");
        let hallazgos = revisar(dir.path(), &Falso(HashMap::new()));
        let errores = mensajes(&hallazgos, Nivel::Error);
        assert!(errores.iter().any(|e| e.contains("MCP `vps` activo")), "{errores:?}");
        assert!(!errores.iter().any(|e| e.contains("`apagado`")), "{errores:?}");
        assert!(errores.iter().any(|e| e.contains("plugins = false")), "{errores:?}");
        assert!(errores.iter().any(|e| e.contains("apps = false")), "{errores:?}");
        assert!(!errores.iter().any(|e| e.contains("hooks = false")), "{errores:?}");
        assert!(mensajes(&hallazgos, Nivel::Aviso)
            .iter()
            .any(|a| a.contains("AGENTS.md")));
    }

    #[test]
    fn missing_config_and_undefined_provider_are_errors() {
        let vacio = tempfile::tempdir().expect("tempdir");
        assert!(
            mensajes(&revisar(vacio.path(), &Falso(HashMap::new())), Nivel::Error)[0]
                .contains("no hay config.toml")
        );
        let dir = home("model = \"x\"\nmodel_provider = \"fantasma\"\n", "{}");
        assert!(
            mensajes(&revisar(dir.path(), &Falso(HashMap::new())), Nivel::Error)[0]
                .contains("`fantasma` no está definido")
        );
    }
}
