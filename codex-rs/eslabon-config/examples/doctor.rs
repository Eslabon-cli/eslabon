//! `cargo run -p eslabon-config --example doctor -- [home]`: the doctor without the CLI.
fn main() {
    let Some(home) = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .or_else(eslabon_config::home_por_defecto)
    else {
        eprintln!("no hay home: pasa una ruta o define CODEX_HOME/HOME");
        std::process::exit(2);
    };
    for hallazgo in eslabon_config::doctor::revisar(&home, &eslabon_config::sondeo::SondeoHttp) {
        println!("{hallazgo}");
    }
}
