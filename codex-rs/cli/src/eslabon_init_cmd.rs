//! eslabon: `eslabon init` — first-run setup for a local chat-completions server.
//!
//! Without flags it probes the usual local ports and offers what is running; with
//! flags it writes the same config without touching the terminal. It never makes a
//! request outside loopback and never handles the API key itself, only the name of
//! the environment variable that holds it.

use clap::Parser;
use codex_core::config::find_codex_home;
use eslabon_config::detector::ServidorLocal;
use eslabon_config::detector::detectar;
use eslabon_config::servidor_local::escribir;
use eslabon_config::sondeo::Sondeo;
use eslabon_config::sondeo::SondeoHttp;
use eslabon_config::sondeo::es_loopback;
use std::io::IsTerminal;
use std::io::Write;

#[derive(Debug, Parser)]
pub struct InitCommand {
    /// Chat-completions base URL of the local server (e.g. http://127.0.0.1:11434/v1).
    #[arg(long)]
    base_url: Option<String>,
    /// Id of the model the server serves.
    #[arg(long)]
    model: Option<String>,
    /// Name of the environment variable that holds the API key (never the key itself).
    #[arg(long)]
    api_key_env: Option<String>,
    /// Overwrite an existing config.toml, keeping a config.toml.bak copy.
    #[arg(long)]
    force: bool,
}

pub fn run(command: InitCommand) -> anyhow::Result<()> {
    exigir_modo_eslabon(codex_config::eslabon::modo_eslabon())?;
    let home = find_codex_home()?.into_path_buf();
    if let Some(aviso) = aviso_de_home_heredado(
        std::env::var_os(codex_config::eslabon::marca::VARIABLE_HOME).as_deref(),
        std::env::var_os("CODEX_HOME").as_deref(),
    ) {
        eprintln!("warning: {aviso}");
    }
    let terminal = Terminal::detectar();
    ejecutar_inicio(&home, command, &terminal, &SondeoHttp)
}

/// eslabon: `eslabon init` writes a home and a config; it must not run as
/// upstream's `codex init` (which would write to `~/.codex`).
fn exigir_modo_eslabon(modo_eslabon: bool) -> anyhow::Result<()> {
    if !modo_eslabon {
        anyhow::bail!("`init` is only available in Eslabon mode");
    }
    Ok(())
}

/// eslabon: one-line notice when the home is inherited from an exported
/// `CODEX_HOME` instead of `ESLABON_HOME` or the default `~/.eslabon`.
fn aviso_de_home_heredado(
    eslabon_home: Option<&std::ffi::OsStr>,
    codex_home: Option<&std::ffi::OsStr>,
) -> Option<String> {
    if eslabon_home.is_some_and(|valor| !valor.is_empty()) {
        return None;
    }
    let codex_home = codex_home.filter(|valor| !valor.is_empty())?;
    Some(format!(
        "using the Codex home inherited from CODEX_HOME ({}) instead of ESLABON_HOME",
        std::path::Path::new(codex_home).display()
    ))
}

/// eslabon: the part of `init` that can be exercised without touching the
/// process home: resolve the server, validate the variable name, write.
fn ejecutar_inicio(
    home: &std::path::Path,
    command: InitCommand,
    terminal: &Terminal,
    sondeo: &dyn Sondeo,
) -> anyhow::Result<()> {
    let (base_url, modelo, nombre) =
        resolver_servidor(command.base_url, command.model, terminal, sondeo)?;
    let api_key_env = match command.api_key_env {
        Some(var) => Some(var),
        None if terminal.interactivo => terminal
            .preguntar_opcional("Name of the API key environment variable (empty = no key)")?,
        None => None,
    };
    if let Some(var) = api_key_env.as_deref() {
        // eslabon: reject anything that looks like a key before it can reach
        // config.toml; the shared validator never echoes the value.
        eslabon_config::servidor_local::validar_nombre_de_variable(var)
            .map_err(anyhow::Error::msg)?;
        if std::env::var_os(var).is_none() {
            eprintln!(
                "warning: the environment variable `{var}` is not set; export it before running the session"
            );
        }
    }
    let (destino, texto, copia) = escribir(
        home,
        &base_url,
        &modelo,
        api_key_env.as_deref(),
        nombre.as_deref(),
        command.force,
    )?;
    println!("Wrote {}", destino.display());
    if let Some(copia) = copia {
        println!("Previous config saved at {}", copia.display());
    }
    print!("{texto}");
    println!("Next step: eslabon");
    Ok(())
}

/// Detected server (or the one the user typed) plus its model.
pub(crate) fn resolver_servidor(
    base_url: Option<String>,
    modelo: Option<String>,
    terminal: &Terminal,
    sondeo: &dyn Sondeo,
) -> anyhow::Result<(String, String, Option<String>)> {
    let (base_url, modelo, nombre) = match base_url {
        Some(url) => (url.trim_end_matches('/').to_string(), modelo, None),
        None if terminal.interactivo => {
            let servidores = detectar(sondeo);
            if servidores.is_empty() {
                let url = preguntar_url(terminal)?;
                (url, modelo, None)
            } else {
                let (url, modelo_servidor, nombre) = elegir_servidor(&servidores, terminal)?;
                (url, modelo.or(Some(modelo_servidor)), Some(nombre))
            }
        }
        None => anyhow::bail!(
            "eslabon init needs a terminal or flags: --base-url http://127.0.0.1:<port>/v1 --model <model> [--api-key-env <VARIABLE>]"
        ),
    };
    let modelo = match modelo {
        Some(modelo) => modelo,
        None if terminal.interactivo => elegir_modelo(&base_url, terminal, sondeo)?,
        None => anyhow::bail!("eslabon init needs --model with the id the server serves"),
    };
    Ok((base_url, modelo, nombre))
}

fn preguntar_url(terminal: &Terminal) -> anyhow::Result<String> {
    terminal
        .preguntar(
            "Chat-completions URL of your server (e.g. http://127.0.0.1:11434/v1)",
        )
        .map(|url| url.trim_end_matches('/').to_string())
}

/// Prints the detected servers and models and returns the pair the user picked.
fn elegir_servidor(
    servidores: &[ServidorLocal],
    terminal: &Terminal,
) -> anyhow::Result<(String, String, String)> {
    let mut opciones = Vec::new();
    for servidor in servidores {
        for modelo in &servidor.modelos {
            opciones.push((
                servidor.base_url.clone(),
                modelo.clone(),
                servidor.nombre.clone(),
            ));
        }
    }
    println!("Local servers detected:");
    for (indice, servidor) in servidores.iter().enumerate() {
        out(&format!(
            "  {} · {} ({}) — models: {}",
            indice + 1,
            servidor.nombre,
            servidor.base_url,
            servidor.modelos.join(", ")
        ));
    }
    let respuesta = terminal.preguntar(&format!(
        "Choose a server and model [1-{}] (empty = 1)",
        opciones.len()
    ))?;
    let indice = match respuesta.trim() {
        "" => 1,
        numero => numero
            .parse::<usize>()
            .ok()
            .filter(|numero| (1..=opciones.len()).contains(numero))
            .ok_or_else(|| anyhow::anyhow!("`{numero}` is not an option in the list"))?,
    };
    Ok(opciones[indice - 1].clone())
}

/// Model id: the first one the server reports when it is loopback, else the answer.
fn elegir_modelo(
    base_url: &str,
    terminal: &Terminal,
    sondeo: &dyn Sondeo,
) -> anyhow::Result<String> {
    if es_loopback(base_url)
        && let Some(cuerpo) = sondeo.get(&format!("{base_url}/models"))
        && let Some(modelo) = modelos_del_cuerpo(&cuerpo).into_iter().next()
    {
        return Ok(modelo);
    }
    terminal.preguntar("Model id that server serves")
}

fn modelos_del_cuerpo(cuerpo: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(cuerpo)
        .ok()
        .and_then(|json| json.get("data").and_then(|data| data.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|modelo| modelo.get("id").and_then(|id| id.as_str()))
        .map(str::to_string)
        .collect()
}

fn out(texto: &str) {
    let _ = writeln!(std::io::stdout(), "{texto}");
}

/// Answers from the terminal: real prompts when interactive, errors otherwise.
pub(crate) struct Terminal {
    pub(crate) interactivo: bool,
}

impl Terminal {
    pub(crate) fn detectar() -> Self {
        Self {
            interactivo: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        }
    }

    pub(crate) fn preguntar(&self, texto: &str) -> anyhow::Result<String> {
        self.leer(texto, /*permitir_vacio*/ false)
    }

    pub(crate) fn preguntar_opcional(&self, texto: &str) -> anyhow::Result<Option<String>> {
        let valor = self.leer(texto, /*permitir_vacio*/ true)?;
        Ok((!valor.is_empty()).then_some(valor))
    }

    fn leer(&self, texto: &str, permitir_vacio: bool) -> anyhow::Result<String> {
        if !self.interactivo {
            anyhow::bail!(
                "this question needs a terminal; use --base-url, --model and --api-key-env"
            );
        }
        print!("{texto}: ");
        std::io::stdout().flush()?;
        let mut linea = String::new();
        if std::io::stdin().read_line(&mut linea)? == 0 {
            anyhow::bail!("no answer on input");
        }
        let valor = linea.trim().to_string();
        if valor.is_empty() && !permitir_vacio {
            anyhow::bail!("the answer cannot be empty");
        }
        Ok(valor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eslabon_config::servidor_local::PROVEEDOR_LOCAL;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;
    use toml::Table;

    #[test]
    fn con_flags_escribe_el_toml_esperado() {
        let home = TempDir::new().expect("temp home");
        let (destino, texto, copia) = escribir(
            home.path(),
            "http://127.0.0.1:11434/v1",
            "humo-1",
            None,
            None,
            /*force*/ false,
        )
        .expect("escribe");
        assert_eq!(copia, None);
        assert_eq!(destino, home.path().join("config.toml"));
        let tabla: Table = texto.parse().expect("TOML");
        assert_eq!(tabla["model"].as_str(), Some("humo-1"));
        assert_eq!(tabla["model_provider"].as_str(), Some(PROVEEDOR_LOCAL));
        let proveedor = tabla["model_providers"][PROVEEDOR_LOCAL]
            .as_table()
            .expect("proveedor");
        assert_eq!(
            proveedor["base_url"].as_str(),
            Some("http://127.0.0.1:11434/v1")
        );
        // P5.3: the chat wire API must be explicit in the file.
        assert_eq!(proveedor["wire_api"].as_str(), Some("chat"));
        // P5.2: the shared writer must silence `eslabon revisar`'s idle warning.
        assert_eq!(
            proveedor["stream_idle_timeout_ms"].as_integer(),
            Some(900_000)
        );
        assert!(
            proveedor["name"]
                .as_str()
                .is_some_and(|nombre| nombre == "Local server (127.0.0.1:11434)")
        );
        assert!(!proveedor.contains_key("env_key"));
        assert_eq!(
            fs::read_to_string(&destino).expect("config en disco"),
            texto
        );
    }

    #[test]
    fn no_pisa_sin_force_y_force_deja_copia() {
        let home = TempDir::new().expect("temp home");
        let destino = home.path().join("config.toml");
        fs::write(&destino, "model = \"anterior\"\n").expect("config anterior");
        let err = escribir(
            home.path(),
            "http://127.0.0.1:1234/v1",
            "nuevo-1",
            None,
            None,
            /*force*/ false,
        )
        .expect_err("no pisa sin --force");
        assert!(err.to_string().contains("--force"), "{err}");
        assert_eq!(
            fs::read_to_string(&destino).expect("sin cambios"),
            "model = \"anterior\"\n"
        );
        assert!(!home.path().join("config.toml.bak").exists());

        escribir(
            home.path(),
            "http://127.0.0.1:1234/v1",
            "nuevo-1",
            None,
            None,
            /*force*/ true,
        )
        .expect("--force escribe");
        assert_eq!(
            fs::read_to_string(home.path().join("config.toml.bak")).expect("copia"),
            "model = \"anterior\"\n"
        );
        let nuevo = fs::read_to_string(&destino).expect("config nuevo");
        assert!(nuevo.contains("model = \"nuevo-1\""), "{nuevo}");
    }

    #[test]
    fn la_llave_nunca_aparece_solo_el_nombre_de_la_variable() {
        let home = TempDir::new().expect("temp home");
        let (_, texto, _) = escribir(
            home.path(),
            "http://127.0.0.1:8080/v1",
            "modelo-local",
            Some("MI_LLAVE"),
            None,
            /*force*/ false,
        )
        .expect("escribe");
        let tabla: Table = texto.parse().expect("TOML");
        let proveedor = tabla["model_providers"][PROVEEDOR_LOCAL]
            .as_table()
            .expect("proveedor");
        assert_eq!(proveedor["env_key"].as_str(), Some("MI_LLAVE"));
        // Only the variable name is recorded: no value field and no secret material.
        assert_eq!(proveedor.len(), 5, "{proveedor:?}");
        assert!(!texto.contains("sk-"), "{texto}");
    }

    #[test]
    fn sin_terminal_ni_flags_pide_las_banderas() {
        let terminal = Terminal { interactivo: false };
        let err = resolver_servidor(None, None, &terminal, &SondeoHttp).expect_err("sin flags");
        let mensaje = err.to_string();
        assert!(mensaje.contains("--base-url"), "{mensaje}");
        assert!(mensaje.contains("--model"), "{mensaje}");
    }

    #[test]
    fn una_llave_como_nombre_de_variable_no_escribe_nada() {
        let home = TempDir::new().expect("temp home");
        let command = InitCommand {
            base_url: Some("http://127.0.0.1:8080/v1".to_string()),
            model: Some("modelo-local".to_string()),
            api_key_env: Some("sk-abc123".to_string()),
            force: false,
        };
        let err = ejecutar_inicio(
            home.path(),
            command,
            &Terminal { interactivo: false },
            &SondeoHttp,
        )
        .expect_err("una llave no es nombre de variable");
        let mensaje = err.to_string();
        assert!(mensaje.contains("looks like an API key"), "{mensaje}");
        // eslabon: the message must not echo the value it is rejecting.
        assert!(!mensaje.contains("sk-abc123"), "{mensaje}");
        assert!(!home.path().join("config.toml").exists());
    }

    #[test]
    fn un_nombre_de_variable_valido_si_escribe() {
        let home = TempDir::new().expect("temp home");
        let command = InitCommand {
            base_url: Some("http://127.0.0.1:8080/v1".to_string()),
            model: Some("modelo-local".to_string()),
            api_key_env: Some("MI_LLAVE".to_string()),
            force: false,
        };
        ejecutar_inicio(
            home.path(),
            command,
            &Terminal { interactivo: false },
            &SondeoHttp,
        )
        .expect("escribe");
        let texto = fs::read_to_string(home.path().join("config.toml")).expect("config");
        assert!(texto.contains("env_key = \"MI_LLAVE\""), "{texto}");
    }

    #[test]
    fn init_solo_existe_en_modo_eslabon() {
        let err = exigir_modo_eslabon(false).expect_err("modo upstream");
        assert_eq!(err.to_string(), "`init` is only available in Eslabon mode");
        assert!(exigir_modo_eslabon(true).is_ok());
    }

    #[test]
    fn el_home_heredado_de_codex_home_se_avisa() {
        use std::ffi::OsStr;
        let codex = OsStr::new("/tmp/hogar-codex");
        let aviso = aviso_de_home_heredado(None, Some(codex)).expect("aviso");
        assert!(aviso.contains("CODEX_HOME"), "{aviso}");
        assert!(aviso.contains("/tmp/hogar-codex"), "{aviso}");
        // ESLABON_HOME manda: no hay nada que avisar.
        assert_eq!(
            aviso_de_home_heredado(Some(OsStr::new("/tmp/otro")), Some(codex)),
            None
        );
        assert_eq!(aviso_de_home_heredado(None, None), None);
    }
}
