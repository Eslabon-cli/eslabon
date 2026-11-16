//! eslabon: minimal `config.toml` for a local OpenAI-compatible server.
//!
//! Shared by `eslabon init` and the TUI onboarding so both surfaces write the
//! same provider table. The API key is never written, only the name of the
//! environment variable that holds it.

use std::path::Path;
use std::path::PathBuf;
use toml::Table;
use toml::Value;

/// Provider id both surfaces write; one fixed id keeps repeated runs idempotent.
pub const PROVEEDOR_LOCAL: &str = "local";

/// Idle timeout a long prefill needs; `eslabon revisar` warns below this value.
pub const STREAM_IDLE_TIMEOUT_MS: u64 = 900_000;

/// eslabon: longest name accepted for an API key environment variable. Real
/// names are short; a long token is a pasted key, not a name.
const LONGITUD_MAXIMA_VARIABLE: usize = 64;

/// eslabon: prefixes used by hosted providers for their keys. A variable name
/// never starts with one of them; a pasted key almost always does.
const PREFIJOS_DE_LLAVE: &[&str] = &["sk-", "sk_", "gsk_", "hf_", "AIza", "xai-"];

/// eslabon: validates an API key *variable name*, never a key. The wizard and
/// `eslabon init` share this implementation so both reject the same input with
/// the same copy. The message never echoes the value: a pasted key must not be
/// printed.
pub fn validar_nombre_de_variable(valor: &str) -> Result<(), String> {
    let valor = valor.trim();
    if valor.is_empty() {
        return Err("the API key variable name cannot be empty".to_string());
    }
    if valor.len() > LONGITUD_MAXIMA_VARIABLE
        || PREFIJOS_DE_LLAVE
            .iter()
            .any(|prefijo| valor.starts_with(prefijo))
    {
        return Err(
            "that looks like an API key, not the name of an environment variable; pass the variable name (for example MY_API_KEY)"
                .to_string(),
        );
    }
    let mut caracteres = valor.chars();
    let primero_valido = caracteres
        .next()
        .is_some_and(|caracter| caracter.is_ascii_alphabetic() || caracter == '_');
    let resto_valido =
        caracteres.all(|caracter| caracter.is_ascii_alphanumeric() || caracter == '_');
    if !primero_valido || !resto_valido {
        return Err(
            "use only letters, digits and underscores, starting with a letter or an underscore"
                .to_string(),
        );
    }
    Ok(())
}

/// eslabon: serializes the provider table after validating the variable name,
/// so a key can never reach `config.toml` through any caller.
fn construir_toml_validado(
    base_url: &str,
    modelo: &str,
    api_key_env: Option<&str>,
    nombre: &str,
) -> anyhow::Result<Table> {
    if let Some(var) = api_key_env {
        validar_nombre_de_variable(var).map_err(anyhow::Error::msg)?;
    }
    Ok(construir_toml(base_url, modelo, api_key_env, nombre))
}

/// Builds the minimal `config.toml` table for a local direct provider.
pub fn construir_toml(
    base_url: &str,
    modelo: &str,
    api_key_env: Option<&str>,
    nombre: &str,
) -> Table {
    let mut proveedor = Table::new();
    proveedor.insert("name".to_string(), Value::String(nombre.to_string()));
    proveedor.insert("base_url".to_string(), Value::String(base_url.to_string()));
    // P5.3: upstream defaults a provider without `wire_api` to Responses, which
    // most local servers do not implement; spell out the chat wire here.
    proveedor.insert("wire_api".to_string(), Value::String("chat".to_string()));
    // P5.2: without an explicit idle timeout a long prefill is cancelled and
    // reprocessed from zero; `eslabon revisar` asks for at least 900000 ms.
    proveedor.insert(
        "stream_idle_timeout_ms".to_string(),
        Value::Integer(STREAM_IDLE_TIMEOUT_MS as i64),
    );
    if let Some(var) = api_key_env {
        proveedor.insert("env_key".to_string(), Value::String(var.to_string()));
    }
    let mut model_providers = Table::new();
    model_providers.insert(PROVEEDOR_LOCAL.to_string(), Value::Table(proveedor));

    let mut tabla = Table::new();
    tabla.insert("model".to_string(), Value::String(modelo.to_string()));
    tabla.insert(
        "model_provider".to_string(),
        Value::String(PROVEEDOR_LOCAL.to_string()),
    );
    tabla.insert("model_providers".to_string(), Value::Table(model_providers));
    tabla
}

/// Serializes and writes the config, refusing to overwrite without `force`.
pub fn escribir(
    home: &Path,
    base_url: &str,
    modelo: &str,
    api_key_env: Option<&str>,
    nombre: Option<&str>,
    force: bool,
) -> anyhow::Result<(PathBuf, String, Option<PathBuf>)> {
    let nombre = nombre
        .map(str::to_string)
        .unwrap_or_else(|| nombre_del_proveedor(base_url));
    let tabla = construir_toml_validado(base_url, modelo, api_key_env, &nombre)?;
    let texto = toml::to_string(&tabla)?;
    let (destino, copia) = escribir_config(home, &texto, force)?;
    Ok((destino, texto, copia))
}

/// eslabon: writes the provider table for the TUI wizard over a config that
/// only holds startup settings (`[tui]`). It keeps that table and leaves no
/// `.bak` copy: the wizard owns the first-run config, the user never does.
pub fn escribir_sobre_ajustes_de_arranque(
    home: &Path,
    base_url: &str,
    modelo: &str,
    api_key_env: Option<&str>,
    nombre: Option<&str>,
) -> anyhow::Result<(PathBuf, String)> {
    let nombre = nombre
        .map(str::to_string)
        .unwrap_or_else(|| nombre_del_proveedor(base_url));
    let mut tabla = construir_toml_validado(base_url, modelo, api_key_env, &nombre)?;
    // eslabon: keep the startup table the app wrote before onboarding; dropping
    // it would erase the screen-reader marker and the user's TUI settings.
    let destino = home.join("config.toml");
    if destino.exists() {
        let anterior = crate::leer_toml(&destino)?;
        if anterior.keys().all(|clave| clave == "tui")
            && let Some(Value::Table(tui)) = anterior.get("tui")
        {
            tabla.insert("tui".to_string(), Value::Table(tui.clone()));
        }
    }
    let texto = toml::to_string(&tabla)?;
    std::fs::write(&destino, &texto)?;
    Ok((destino, texto))
}

pub fn escribir_config(
    home: &Path,
    texto: &str,
    force: bool,
) -> anyhow::Result<(PathBuf, Option<PathBuf>)> {
    let destino = home.join("config.toml");
    let mut copia = None;
    if destino.exists() {
        if !force {
            anyhow::bail!(
                "{} already exists: use --force to replace it (leaves a copy next to it)",
                destino.display()
            );
        }
        // eslabon: never overwrite an existing `config.toml.bak`; a repeated
        // `--force` would lose the previous copy (it happened in P5.2).
        let ruta = ruta_de_la_copia(home);
        std::fs::copy(&destino, &ruta)?;
        copia = Some(ruta);
    }
    std::fs::write(&destino, texto)?;
    Ok((destino, copia))
}

/// eslabon: name of the copy left by `--force`. `config.toml.bak` for the first
/// one, then `config.toml.bak-<AAAAMMDD-HHMMSS>` (UTC), then a numeric suffix
/// when two copies land in the same second. Keeping the stamp preserves the
/// previous `.bak` instead of overwriting it.
fn ruta_de_la_copia(home: &Path) -> PathBuf {
    let primera = home.join("config.toml.bak");
    if !primera.exists() {
        return primera;
    }
    let marca = marca_de_tiempo();
    let candidata = home.join(format!("config.toml.bak-{marca}"));
    if !candidata.exists() {
        return candidata;
    }
    let mut numero = 2u32;
    loop {
        let candidata = home.join(format!("config.toml.bak-{marca}-{numero}"));
        if !candidata.exists() {
            return candidata;
        }
        numero += 1;
    }
}

/// `AAAAMMDD-HHMMSS` in UTC. Only the std library is available here; a UTC stamp
/// is still unique enough to tie a copy to the moment it was made.
fn marca_de_tiempo() -> String {
    let segundos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duracion| duracion.as_secs() as i64)
        .unwrap_or_default();
    let (anio, mes, dia) = fecha_civil_desde_dias(segundos.div_euclid(86_400));
    let resto = segundos.rem_euclid(86_400);
    format!(
        "{anio:04}{mes:02}{dia:02}-{:02}{:02}{:02}",
        resto / 3_600,
        (resto % 3_600) / 60,
        resto % 60
    )
}

/// Civil date (proleptic Gregorian) for days since 1970-01-01, by Howard
/// Hinnant's algorithm; no calendar dependency is needed.
fn fecha_civil_desde_dias(dias: i64) -> (i64, i64, i64) {
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let dia_de_era = z - era * 146_097;
    let anio_de_era =
        (dia_de_era - dia_de_era / 1_460 + dia_de_era / 36_524 - dia_de_era / 146_096) / 365;
    let anio = anio_de_era + era * 400;
    let dia_del_anio = dia_de_era - (365 * anio_de_era + anio_de_era / 4 - anio_de_era / 100);
    let mes_de_era = (5 * dia_del_anio + 2) / 153;
    let dia = dia_del_anio - (153 * mes_de_era + 2) / 5 + 1;
    let mes = if mes_de_era < 10 {
        mes_de_era + 3
    } else {
        mes_de_era - 9
    };
    let anio = if mes <= 2 { anio + 1 } else { anio };
    (anio, mes, dia)
}

/// True when `config.toml` is missing or only holds the `[tui]` table the app
/// writes before onboarding (screen-reader detection), i.e. the wizard can
/// replace it and keep a `.bak` copy without losing user settings.
pub fn config_solo_ajustes_de_arranque(home: &Path) -> anyhow::Result<bool> {
    let destino = home.join("config.toml");
    if !destino.exists() {
        return Ok(true);
    }
    let tabla = crate::leer_toml(&destino)?;
    Ok(tabla.keys().all(|clave| clave == "tui"))
}

/// Name shown in `[model_providers.local] name`.
pub fn nombre_del_proveedor(base_url: &str) -> String {
    let detalle = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(base_url);
    format!("Local server ({detalle})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn escribe_el_toml_esperado_con_timeout() {
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
        assert_eq!(copia, None, "no copy without an existing config.toml");
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
        assert_eq!(proveedor["wire_api"].as_str(), Some("chat"));
        assert_eq!(
            proveedor["stream_idle_timeout_ms"].as_integer(),
            Some(STREAM_IDLE_TIMEOUT_MS as i64)
        );
        assert_eq!(
            proveedor["name"].as_str(),
            Some("Local server (127.0.0.1:11434)")
        );
        assert!(!proveedor.contains_key("env_key"));
        assert_eq!(
            fs::read_to_string(&destino).expect("config en disco"),
            texto
        );
    }

    #[test]
    fn solo_el_nombre_de_la_variable_de_llave() {
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
        assert_eq!(proveedor.len(), 5, "{proveedor:?}");
        assert!(!texto.contains("sk-"), "{texto}");
    }

    // eslabon: P5.2 fix — a second `--force` must not overwrite the first copy.
    #[test]
    fn dos_force_seguidos_dejan_dos_copias_distintas() {
        let home = TempDir::new().expect("temp home");
        let destino = home.path().join("config.toml");
        fs::write(&destino, "model = \"primera\"\n").expect("config anterior");

        let (_, _, copia_1) = escribir(
            home.path(),
            "http://127.0.0.1:11434/v1",
            "humo-1",
            None,
            None,
            /*force*/ true,
        )
        .expect("primer --force");
        let copia_1 = copia_1.expect("copia del primer --force");
        assert_eq!(copia_1, home.path().join("config.toml.bak"));
        let segundo = fs::read_to_string(&destino).expect("config nuevo");

        let (_, _, copia_2) = escribir(
            home.path(),
            "http://127.0.0.1:11434/v1",
            "humo-2",
            None,
            None,
            /*force*/ true,
        )
        .expect("segundo --force");
        let copia_2 = copia_2.expect("copia del segundo --force");
        assert_ne!(copia_1, copia_2, "the copies must be distinct files");
        assert!(
            copia_2
                .file_name()
                .and_then(|nombre| nombre.to_str())
                .is_some_and(|nombre| nombre.starts_with("config.toml.bak-")),
            "{}",
            copia_2.display()
        );
        assert_eq!(
            fs::read_to_string(&copia_1).expect("copia 1"),
            "model = \"primera\"\n"
        );
        assert_eq!(fs::read_to_string(&copia_2).expect("copia 2"), segundo);
        assert!(destino.exists());
    }

    #[test]
    fn la_tercera_copia_usa_un_sufijo_numerico_o_una_marca_nueva() {
        let home = TempDir::new().expect("temp home");
        fs::write(home.path().join("config.toml.bak"), "primera").expect("bak");
        let segunda = ruta_de_la_copia(home.path());
        fs::write(&segunda, "segunda").expect("segunda copia");
        let tercera = ruta_de_la_copia(home.path());
        assert_ne!(segunda, tercera);
        assert!(
            tercera
                .file_name()
                .and_then(|nombre| nombre.to_str())
                .is_some_and(|nombre| nombre.starts_with("config.toml.bak-")),
            "{}",
            tercera.display()
        );
    }

    #[test]
    fn solo_ajustes_de_arranque_es_reemplazable() {
        let home = TempDir::new().expect("temp home");
        assert!(config_solo_ajustes_de_arranque(home.path()).expect("sin config"));
        fs::write(
            home.path().join("config.toml"),
            "[tui]\nscreen_reader_detection_done = true\n",
        )
        .expect("config de arranque");
        assert!(config_solo_ajustes_de_arranque(home.path()).expect("solo tui"));
        fs::write(
            home.path().join("config.toml"),
            "notify = [\"x\"]\n[tui]\nscreen_reader_detection_done = true\n",
        )
        .expect("config con ajustes");
        assert!(!config_solo_ajustes_de_arranque(home.path()).expect("ajustes"));
    }

    #[test]
    fn el_nombre_de_variable_tiene_reglas() {
        for valido in ["MI_LLAVE", "GEMINI_API_KEY", "_privada", "a1"] {
            assert_eq!(validar_nombre_de_variable(valido), Ok(()), "{valido}");
        }
        for invalido in [
            "",
            "1LLAVE",
            "MI-LLAVE",
            "MI LLAVE",
            "sk-abc123",
            &"x".repeat(65),
        ] {
            assert!(
                validar_nombre_de_variable(invalido).is_err(),
                "{invalido:?}"
            );
        }
        let mensaje = validar_nombre_de_variable("sk-abc123").expect_err("llave");
        assert!(mensaje.contains("looks like an API key"), "{mensaje}");
        // eslabon: never echo the rejected value; it may be the key itself.
        assert!(!mensaje.contains("sk-abc123"), "{mensaje}");
    }

    #[test]
    fn una_llave_no_se_escribe_ni_con_force() {
        let home = TempDir::new().expect("temp home");
        let err = escribir(
            home.path(),
            "http://127.0.0.1:11434/v1",
            "humo-1",
            Some("sk-abc123"),
            None,
            /*force*/ true,
        )
        .expect_err("llave");
        assert!(err.to_string().contains("looks like an API key"), "{err}");
        assert!(!home.path().join("config.toml").exists());
    }

    #[test]
    fn el_asistente_conserva_la_tabla_tui_y_no_deja_copia() {
        let home = TempDir::new().expect("temp home");
        fs::write(
            home.path().join("config.toml"),
            "[tui]\nscreen_reader_detection_done = true\n",
        )
        .expect("config de arranque");
        let (destino, texto) = escribir_sobre_ajustes_de_arranque(
            home.path(),
            "http://127.0.0.1:11434/v1",
            "humo-1",
            None,
            None,
        )
        .expect("escribe");
        assert_eq!(destino, home.path().join("config.toml"));
        let tabla: Table = texto.parse().expect("TOML");
        assert_eq!(tabla["model"].as_str(), Some("humo-1"));
        assert_eq!(
            tabla["tui"]["screen_reader_detection_done"].as_bool(),
            Some(true)
        );
        assert!(!home.path().join("config.toml.bak").exists());
        let copias = fs::read_dir(home.path())
            .expect("home")
            .filter_map(Result::ok)
            .filter(|entrada| {
                entrada
                    .file_name()
                    .to_string_lossy()
                    .starts_with("config.toml.bak")
            })
            .count();
        assert_eq!(copias, 0, "el asistente no deja .bak");
    }

    #[test]
    fn nombre_del_proveedor_quita_http_y_https() {
        assert_eq!(
            nombre_del_proveedor("http://127.0.0.1:11434/v1"),
            "Local server (127.0.0.1:11434)"
        );
        assert_eq!(
            nombre_del_proveedor("https://servidor.local/v1"),
            "Local server (servidor.local)"
        );
    }
}
