//! eslabon: `codex phi` — turn the data lock on or off for this `$CODEX_HOME`.

use anyhow::Context;
use clap::Parser;
use clap::Subcommand;
use codex_api::eslabon_policy;
use codex_core::config::find_codex_home;
use codex_utils_cli::ProfileV2Name;

/// eslabon: command name in this command's hints. Keeps upstream's "codex"
/// outside Eslabon mode and says "eslabon" inside it.
fn nombre_binario_mensaje() -> &'static str {
    if codex_config::eslabon::modo_eslabon() {
        codex_config::eslabon::marca::NOMBRE_BINARIO
    } else {
        "codex"
    }
}

#[derive(Debug, Parser)]
pub struct PhiCommand {
    #[command(subcommand)]
    action: PhiAction,
}

#[derive(Debug, Subcommand)]
enum PhiAction {
    /// Block model requests to every provider not listed in `[eslabon] phi_providers`.
    On {
        /// Profile whose `phi_providers` define what stays allowed (e.g. `local`).
        #[arg(long, short = 'p')]
        profile: Option<ProfileV2Name>,
    },
    /// Lift the block. Only for sessions that handle no protected data.
    Off,
    /// Remove the switch: each session follows its profile's `data_policy` again.
    Default,
    /// Show the current state.
    Status,
}

pub async fn run(command: PhiCommand) -> anyhow::Result<()> {
    let codex_home = find_codex_home()?.into_path_buf();
    let path = eslabon_policy::switch_path(&codex_home);
    match command.action {
        PhiAction::On { profile } => {
            let allowed = allowed_base_urls(&codex_home, profile.as_ref()).await?;
            eslabon_policy::write_switch(
                &path,
                &eslabon_policy::PhiSwitch {
                    enabled: true,
                    allowed_base_urls: allowed.clone(),
                },
            )
            .map_err(anyhow::Error::msg)?;
            println!("Data lock ON. Model requests may only reach:");
            for base in allowed {
                println!("  {base}");
            }
        }
        PhiAction::Off => {
            eslabon_policy::write_switch(
                &path,
                &eslabon_policy::PhiSwitch {
                    enabled: false,
                    allowed_base_urls: Vec::new(),
                },
            )
            .map_err(anyhow::Error::msg)?;
            println!(
                "Data lock OFF for every eslabon session in {}. Do not use it with protected data.",
                codex_home.display()
            );
        }
        PhiAction::Default => {
            eslabon_policy::clear_switch(&path).map_err(anyhow::Error::msg)?;
            println!("Switch removed: each session follows its profile's [eslabon] data_policy.");
        }
        PhiAction::Status => match eslabon_policy::read_switch(&path) {
            Ok(Some(switch)) if switch.enabled => {
                println!("Data lock ON (switch). Allowed:");
                for base in switch.allowed_base_urls {
                    println!("  {base}");
                }
            }
            Ok(Some(_)) => println!("Data lock OFF (switch)."),
            Ok(None) => println!(
                "No switch: each session follows its profile's [eslabon] data_policy (default: off)."
            ),
            Err(err) => println!(
                "Switch unreadable ({err}): every request is blocked until you fix it or run `{} phi default`.",
                nombre_binario_mensaje()
            ),
        },
    }
    Ok(())
}

/// `phi_providers` base URLs, read from the merged TOML without validating the
/// session (a full config load would itself refuse an unlisted provider).
async fn allowed_base_urls(
    codex_home: &std::path::Path,
    profile: Option<&ProfileV2Name>,
) -> anyhow::Result<Vec<String>> {
    let loader_overrides = match profile {
        Some(profile) => codex_config::LoaderOverrides {
            user_config_path: Some(codex_core::config::resolve_profile_v2_config_path(
                codex_home, profile,
            )),
            user_config_profile: Some(profile.clone()),
            ..Default::default()
        },
        None => codex_config::LoaderOverrides::default(),
    };
    let toml = codex_core::config::load_config_as_toml_with_cli_overrides(
        codex_home,
        /*cwd*/ None,
        Vec::new(),
        loader_overrides,
    )
    .await
    .context("failed to read configuration")?;
    let listed = toml
        .eslabon
        .as_ref()
        .and_then(|eslabon| eslabon.phi_providers.clone())
        .unwrap_or_default();
    let mut bases = Vec::new();
    for id in &listed {
        let base = toml
            .model_providers
            .get(id)
            .and_then(|provider| provider.base_url.clone())
            .with_context(|| format!("phi_providers lists `{id}`, which has no base_url"))?;
        bases.push(base);
    }
    if bases.is_empty() {
        anyhow::bail!(
            "no [eslabon] phi_providers in this configuration{}; pass the data lock profile, e.g. `{} phi on -p local`",
            profile
                .map(|p| format!(" (profile {})", p.as_str()))
                .unwrap_or_default(),
            nombre_binario_mensaje()
        );
    }
    Ok(bases)
}
