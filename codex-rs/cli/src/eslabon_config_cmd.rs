//! eslabon: configuration-layer commands — `revisar` (verify a home without running a
//! model) and `desatendido` (create an unattended home from a profile).

use clap::Parser;
use codex_core::config::find_codex_home;
use eslabon_config::doctor::Nivel;
use std::path::PathBuf;

#[derive(Debug, Parser)]
pub struct RevisarCommand {
    /// Home to check; defaults to the one eslabon uses ($CODEX_HOME or ~/.eslabon).
    #[arg(long)]
    home: Option<PathBuf>,
}

#[derive(Debug, Parser)]
pub struct DesatendidoCommand {
    /// Directory of the new home (must not contain a config.toml yet).
    destino: PathBuf,
    /// Profile of the source home to take the model and provider from.
    #[arg(long, short = 'p')]
    perfil: Option<String>,
    /// Source home; defaults to the one eslabon uses.
    #[arg(long)]
    desde: Option<PathBuf>,
}

pub fn run_revisar(command: RevisarCommand) -> anyhow::Result<()> {
    let home = match command.home {
        Some(home) => home,
        None => find_codex_home()?.into_path_buf(),
    };
    let hallazgos = eslabon_config::doctor::revisar(&home, &eslabon_config::sondeo::SondeoHttp);
    for hallazgo in &hallazgos {
        println!("{hallazgo}");
    }
    let errores = hallazgos.iter().filter(|h| h.nivel == Nivel::Error).count();
    let avisos = hallazgos.iter().filter(|h| h.nivel == Nivel::Aviso).count();
    println!("\n{errores} errores, {avisos} avisos");
    if errores > 0 {
        std::process::exit(1);
    }
    Ok(())
}

pub fn run_desatendido(command: DesatendidoCommand) -> anyhow::Result<()> {
    let origen = match command.desde {
        Some(home) => home,
        None => find_codex_home()?.into_path_buf(),
    };
    let config = eslabon_config::desatendido::crear(&origen, command.perfil.as_deref(), &command.destino)?;
    println!("Home desatendido creado: {}", config.display());
    println!(
        "Úsalo con: CODEX_HOME={} eslabon ...   (verifica con `eslabon revisar --home {}`)",
        command.destino.display(),
        command.destino.display()
    );
    Ok(())
}
