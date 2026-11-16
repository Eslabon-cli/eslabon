//! eslabon: Eslabon's CLI identity.
//!
//! The clap tree is derived from `MultitoolCli` in main.rs; this module only
//! rewrites the copy users see at parse time, so the upstream tree (and codex
//! mode) stays untouched and there is no second declaration of the command.

use crate::MultitoolCli;
use clap::Command;
use clap::CommandFactory;
use clap::FromArgMatches;
use codex_config::eslabon::marca;
use codex_utils_cli::collect_config_overrides;
use std::ffi::OsString;

/// Name of the binary as users invoke it (`eslabon`), for copy and usage.
fn nombre_binario() -> &'static str {
    marca::NOMBRE_BINARIO
}

/// Eslabon's full version line: the version, then the required attribution.
fn linea_de_version(version: &str) -> &'static str {
    Box::leak(format!("{version}\n{}", marca::ATRIBUCION).into_boxed_str())
}

/// Applies Eslabon's copy to the command and every subcommand it builds.
pub(super) fn con_marca(comando: Command) -> Command {
    let version = comando.get_version().unwrap_or_default().to_string();
    let comando = comando
        .display_name(nombre_binario())
        .bin_name(nombre_binario())
        .override_usage(format!(
            "{0} [OPTIONS] [PROMPT]\n       {0} [OPTIONS] <COMMAND> [ARGS]",
            nombre_binario()
        ))
        .version(linea_de_version(&version))
        .after_help(marca::ATRIBUCION);
    con_rama(comando, "")
}

fn con_rama(comando: Command, ruta: &str) -> Command {
    let comando = reescribir_uso(reescribir_textos(comando), ruta);
    comando.mut_subcommands(|hija| {
        let nombre = hija.get_name().to_string();
        let ruta_hija = if ruta.is_empty() {
            nombre
        } else {
            format!("{ruta} {nombre}")
        };
        con_rama(hija, &ruta_hija)
    })
}

/// `exec` and `mcp add` hardcode their usage upstream (`codex ...`). They have no
/// public getter on `clap::Command`, so they are rebuilt here by their path.
fn reescribir_uso(comando: Command, ruta: &str) -> Command {
    let bin = nombre_binario();
    match ruta {
        "exec" => comando.override_usage(format!(
            "{bin} exec [OPTIONS] [PROMPT]\n       {bin} exec [OPTIONS] <COMMAND> [ARGS]"
        )),
        "mcp add" => comando.override_usage(format!(
            "{bin} mcp add [OPTIONS] <NAME> (--url <URL> | -- <COMMAND>...)"
        )),
        _ => comando,
    }
}

fn reescribir_textos(comando: Command) -> Command {
    let mut comando = comando;
    if let Some(texto) = comando.get_about().map(ToString::to_string)
        && let Some(nuevo) = texto_marca(&texto)
    {
        comando = comando.about(cadena_estatica(nuevo));
    }
    if let Some(texto) = comando.get_long_about().map(ToString::to_string)
        && let Some(nuevo) = texto_marca(&texto)
    {
        comando = comando.long_about(cadena_estatica(nuevo));
    }
    comando.mut_args(reescribir_arg)
}

fn reescribir_arg(mut arg: clap::Arg) -> clap::Arg {
    if let Some(texto) = arg.get_help().map(ToString::to_string)
        && let Some(nuevo) = texto_marca(&texto)
    {
        arg = arg.help(cadena_estatica(nuevo));
    }
    if let Some(texto) = arg.get_long_help().map(ToString::to_string)
        && let Some(nuevo) = texto_marca(&texto)
    {
        arg = arg.long_help(cadena_estatica(nuevo));
    }
    arg
}

/// Eslabon's copy of one string, or `None` when the text does not name the product.
///
/// `$CODEX_HOME` and `~/.codex` are rewritten first because Eslabon resolves its
/// home from `ESLABON_HOME`; environment variable names (`CODEX_ACCESS_TOKEN`)
/// are left alone on purpose: renaming them in help would be a lie.
fn texto_marca(texto: &str) -> Option<String> {
    let nuevo = texto
        .replace("$CODEX_HOME", "$ESLABON_HOME")
        .replace("~/.codex", "~/.eslabon")
        .replace("Codex", marca::NOMBRE_PRODUCTO)
        .replace("codex", marca::NOMBRE_BINARIO);
    (nuevo != texto).then_some(nuevo)
}

fn cadena_estatica(texto: String) -> &'static str {
    Box::leak(texto.into_boxed_str())
}

/// Same plumbing as `config_override_args::try_parse_from`, but over the branded
/// command. It is only used in Eslabon mode; codex mode keeps the original path.
pub(super) fn try_parse_from(
    args: impl IntoIterator<Item = impl Into<OsString>>,
) -> Result<MultitoolCli, clap::Error> {
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let mut command = con_marca(MultitoolCli::command());
    let mut matches = command.try_get_matches_from_mut(&args)?;
    crate::queue_cmd::validate_options(&command, &matches)?;
    let mut cli = MultitoolCli::from_arg_matches_mut(&mut matches)
        .map_err(|error| error.format(&mut command))?;
    if cli.subcommand.is_none() || cli.config_overrides.raw_overrides.is_empty() {
        return Ok(cli);
    }
    cli.config_overrides = collect_config_overrides(MultitoolCli::command(), args)?;
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ayuda_de(args: &[&str]) -> String {
        let err = try_parse_from(args.iter().copied()).expect_err("help should short-circuit");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        err.to_string()
    }

    /// Lines that name the product semantically, not upstream env vars (`CODEX_HOME`).
    fn lineas_con_codex(texto: &str) -> Vec<&str> {
        texto
            .lines()
            .filter(|linea| {
                linea.to_ascii_lowercase().contains("codex") && !linea.contains("CODEX_")
            })
            .collect()
    }

    #[test]
    fn ayuda_en_modo_eslabon_no_nombra_codex_salvo_la_atribucion() {
        let ayuda = ayuda_de(&["eslabon", "--help"]);
        assert!(ayuda.starts_with("Eslabon CLI"));
        assert!(ayuda.contains("Usage: eslabon [OPTIONS] [PROMPT]"));
        assert_eq!(lineas_con_codex(&ayuda), vec![marca::ATRIBUCION]);
    }

    #[test]
    fn version_en_modo_eslabon_lleva_la_atribucion_en_la_segunda_linea() {
        let err = try_parse_from(["eslabon", "--version"]).expect_err("version should short-circuit");
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
        let version = err.to_string();
        let lineas: Vec<&str> = version.lines().filter(|linea| !linea.is_empty()).collect();
        assert_eq!(lineas.len(), 2, "{version}");
        assert!(lineas[0].starts_with("eslabon "), "{version}");
        assert_eq!(lineas[1], marca::ATRIBUCION, "{version}");
    }

    #[test]
    fn ayudas_de_subcomandos_usan_eslabon() {
        let ayuda = ayuda_de(&["eslabon", "exec", "--help"]);
        assert!(ayuda.contains("Run Eslabon non-interactively"), "{ayuda}");
        assert!(ayuda.contains("Usage: eslabon exec"), "{ayuda}");
        assert!(lineas_con_codex(&ayuda).is_empty(), "{ayuda}");
    }

    #[test]
    fn modo_codex_queda_identico_a_upstream() {
        let ayuda = MultitoolCli::command().render_help().to_string();
        assert!(ayuda.starts_with("Codex CLI"));
        assert!(ayuda.contains("Usage: codex [OPTIONS] [PROMPT]"));
        assert!(!ayuda.contains("Eslabon"));
    }
}
