//! eslabon: shared Eslabon-mode detection and the module for its public identity.
//!
//! The installed launcher marks the process with `ESLABON_LAUNCHER=1` (it runs
//! the binary as `codex` so terminal managers recognize the agent), and argv[0]
//! covers the `eslabon` binary itself. CLI and core read the answer only through
//! this module.

// eslabon: the detection lives in the leaf crate `codex-utils-home-dir` because
// the home resolver needs it too (and that crate cannot depend on codex-config).
pub use codex_utils_home_dir::modo_eslabon;
pub use codex_utils_home_dir::modo_eslabon_desde;

/// Eslabon's public identity: one place for every literal the packages use
/// instead of hardcoding it. The home constants live in `codex-utils-home-dir`
/// so the resolver and this module cannot drift apart; they are re-exported here
/// unchanged.
pub mod marca {
    /// Product name shown to users.
    pub const NOMBRE_PRODUCTO: &str = "Eslabon";
    /// Name of the shipped binary.
    pub const NOMBRE_BINARIO: &str = "eslabon";
    /// Upstream's product name, used when Eslabon mode is off.
    pub const NOMBRE_PRODUCTO_UPSTREAM: &str = "OpenAI Codex";
    /// Upstream's short product name, used when Eslabon mode is off.
    pub const NOMBRE_CORTO_UPSTREAM: &str = "Codex";
    /// Label of the session-title item that names the application.
    pub const ETIQUETA_NOMBRE_APP: &str = "Eslabon app name";
    /// Same label with upstream's name, used when Eslabon mode is off.
    pub const ETIQUETA_NOMBRE_APP_UPSTREAM: &str = "Codex app name";
    /// Label of the session-title item that shows the application version.
    pub const ETIQUETA_VERSION_APP: &str = "Eslabon application version";
    /// Same label with upstream's name, used when Eslabon mode is off.
    pub const ETIQUETA_VERSION_APP_UPSTREAM: &str = "Codex application version";
    /// Composer placeholder on the first screen.
    pub const PLACEHOLDER_PREGUNTAR: &str = "Ask Eslabon to do anything";
    /// Same placeholder upstream, used when Eslabon mode is off.
    pub const PLACEHOLDER_PREGUNTAR_UPSTREAM: &str = "Ask Codex to do anything";
    /// Default home folder, under the user's home directory.
    pub const CARPETA_HOME: &str = codex_utils_home_dir::ESLABON_HOME_DIR_NAME;
    /// Environment variable that overrides the home.
    pub const VARIABLE_HOME: &str = codex_utils_home_dir::ESLABON_HOME_ENV;
    // eslabon: provisional URL until a release location is chosen.
    pub const URL_REPOSITORIO: &str = "https://github.com/Eslabon-cli/eslabon";
    pub const URL_RELEASES: &str = "https://github.com/Eslabon-cli/eslabon/releases";
    /// Required attribution for the upstream project.
    pub const ATRIBUCION: &str = "fork of OpenAI Codex, not affiliated with OpenAI";
}

#[cfg(test)]
mod tests {
    use super::marca;

    #[test]
    fn marca_fija_los_literales_publicos() {
        assert_eq!(marca::NOMBRE_PRODUCTO, "Eslabon");
        assert_eq!(marca::NOMBRE_BINARIO, "eslabon");
        assert_eq!(marca::NOMBRE_PRODUCTO_UPSTREAM, "OpenAI Codex");
        assert_eq!(marca::NOMBRE_CORTO_UPSTREAM, "Codex");
        assert_eq!(marca::ETIQUETA_NOMBRE_APP, "Eslabon app name");
        assert_eq!(marca::ETIQUETA_NOMBRE_APP_UPSTREAM, "Codex app name");
        assert_eq!(marca::CARPETA_HOME, ".eslabon");
        assert_eq!(marca::VARIABLE_HOME, "ESLABON_HOME");
        assert_eq!(
            marca::URL_REPOSITORIO,
            "https://github.com/Eslabon-cli/eslabon"
        );
        assert_eq!(
            marca::URL_RELEASES,
            "https://github.com/Eslabon-cli/eslabon/releases"
        );
        assert_eq!(
            marca::ATRIBUCION,
            "fork of OpenAI Codex, not affiliated with OpenAI"
        );
    }
}
