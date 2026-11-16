//! eslabon: turns `[eslabon] data_policy = "phi"` into an enforced egress policy.

use codex_config::config_toml::EslabonDataPolicy;
use codex_config::config_toml::EslabonToml;
use codex_config::eslabon::marca;
use codex_config::types::AuthCredentialsStoreMode;
use codex_features::Feature;
use codex_features::FeaturesToml;
use codex_login::AuthKeyringBackendKind;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use std::collections::BTreeMap;
use std::collections::HashMap;

/// eslabon: the shared Eslabon-mode detection. Tests pin the answer per thread so
/// that config-loading tests never mutate process-wide environment variables.
pub(crate) fn modo_eslabon() -> bool {
    #[cfg(test)]
    {
        // Deterministic for the unit suite: a left-over ESLABON_LAUNCHER in the
        // test runner must not silently flip every fixture to Eslabon defaults.
        return MODO_ESLABON_PRUEBA.with(|cell| cell.get().unwrap_or(false));
    }
    #[cfg(not(test))]
    {
        codex_config::eslabon::modo_eslabon()
    }
}

#[cfg(test)]
thread_local! {
    static MODO_ESLABON_PRUEBA: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn fijar_modo_eslabon_para_prueba(value: Option<bool>) {
    MODO_ESLABON_PRUEBA.with(|cell| cell.set(value));
}

/// `analytics.enabled` as the session will see it. PHI sessions always say no;
/// Eslabon mode defaults to no unless the user set the key explicitly.
pub(crate) fn analytics_enabled(configured: Option<bool>, phi_active: bool) -> Option<bool> {
    if phi_active {
        return Some(false);
    }
    configured.or_else(|| modo_eslabon().then_some(false))
}

/// `check_for_update_on_startup`: upstream defaults to `true`, Eslabon to `false`.
pub(crate) fn check_for_update_on_startup(configured: Option<bool>) -> bool {
    configured.unwrap_or(!modo_eslabon())
}

/// `feedback.enabled` as the session will see it: Eslabon mode defaults to
/// `false` (no Sentry uploads), and an explicit `true` still wins.
pub(crate) fn feedback_enabled(configured: Option<bool>) -> bool {
    configured.unwrap_or(!modo_eslabon())
}

/// eslabon: command name shown in `codex phi`/`eslabon phi` hints. The mode
/// decides the prefix so every user-visible string keeps upstream's copy when
/// Eslabon mode is off.
fn nombre_binario_mensaje() -> &'static str {
    if modo_eslabon() {
        marca::NOMBRE_BINARIO
    } else {
        "codex"
    }
}

/// `[features] plugins` overlay for Eslabon mode. Returns `None` when the user
/// already set the key (an explicit value always wins).
pub(crate) fn features_eslabon(configured: Option<&FeaturesToml>) -> Option<FeaturesToml> {
    if !modo_eslabon() {
        return None;
    }
    if configured.is_some_and(|features| features.entries().contains_key("plugins")) {
        return None;
    }
    Some(FeaturesToml::from(BTreeMap::from([(
        Feature::Plugins.key().to_string(),
        false,
    )])))
}

/// `[eslabon] anuncios` as the session will see it: `None` means the mode default.
pub(crate) fn anuncios(configured: Option<bool>) -> bool {
    configured.unwrap_or(!modo_eslabon())
}

// A loopback base URL is NOT trusted by itself: on these machines 127.0.0.1 ports are
// often SSH tunnels, and a tunnel's local end says nothing about where the text lands
// (2026-09-16: a "127.0.0.1" model resolved to a third-party server in Germany).
// Every provider allowed under PHI must be listed explicitly in `phi_providers`.

/// What the config load decided about PHI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DataPolicyOutcome {
    /// Whether the block applies to this session at load time (switch, else default).
    pub(crate) phi_active: bool,
    /// eslabon: whether the origin rule (OpenAI hosts denied) was decided on.
    pub(crate) origen_activo: bool,
    /// Base URLs of the providers in `phi_providers`, for turning the block on later.
    pub(crate) allowed_base_urls: Vec<String>,
}

/// Validates the selected provider against the effective PHI state and registers the
/// session default with the process-wide egress policy.
pub(crate) fn apply_data_policy(
    eslabon: Option<&EslabonToml>,
    model_providers: &HashMap<String, ModelProviderInfo>,
    selected_provider_id: &str,
    codex_home: &std::path::Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<DataPolicyOutcome> {
    // eslabon: hand the mode to the models manager before anything resolves a model,
    // so an unknown slug gets the conservative chat profile instead of the upstream
    // 272k fallback (and its "metadata not found" warning). Outside the mode the
    // manager keeps its upstream behavior.
    #[cfg(not(test))]
    codex_models_manager::model_info::activar_perfil_por_defecto_eslabon(modo_eslabon());
    codex_api::eslabon_policy::set_switch_dir(codex_home);
    let (outcome, register_default) = evaluate_data_policy(
        eslabon,
        model_providers,
        selected_provider_id,
        codex_home,
        auth_credentials_store_mode,
    )?;
    if register_default {
        codex_api::eslabon_policy::activate_phi_policy(outcome.allowed_base_urls.clone());
    }
    // eslabon: the origin rule is process-wide and independent of the PHI switch.
    // Tests assert the decision through `outcome.origen_activo` so the global
    // policy never leaks between tests in the same binary.
    #[cfg(not(test))]
    aplicar_candado_de_origen(outcome.origen_activo);
    Ok(outcome)
}

/// eslabon: pushes the origin-rule decision of a config load into the
/// process-wide policy. A reload that decides "off" must turn it off, not only
/// on ("sticky" was the old bug).
fn aplicar_candado_de_origen(activo: bool) {
    codex_api::eslabon_policy::set_origin_policy(activo);
}

/// The validation half of [`apply_data_policy`], without touching process-wide state.
/// Also says whether the PHI default must be registered with the egress policy.
fn evaluate_data_policy(
    eslabon: Option<&EslabonToml>,
    model_providers: &HashMap<String, ModelProviderInfo>,
    selected_provider_id: &str,
    codex_home: &std::path::Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<(DataPolicyOutcome, bool)> {
    let listed = eslabon
        .and_then(|eslabon| eslabon.phi_providers.clone())
        .unwrap_or_default();
    for id in &listed {
        if !model_providers.contains_key(id) {
            return Err(invalid(format!(
                "eslabon.phi_providers lists `{id}`, which is not a configured model provider"
            )));
        }
    }
    let allowed_base_urls: Vec<String> = listed
        .iter()
        .filter_map(|id| model_providers.get(id))
        .filter_map(|info| info.base_url.clone())
        .collect();
    let default_phi = eslabon
        .and_then(|eslabon| eslabon.data_policy)
        .unwrap_or_default()
        == EslabonDataPolicy::Phi;
    let switch_path = codex_api::eslabon_policy::switch_path(codex_home);
    let switch = codex_api::eslabon_policy::read_switch(&switch_path).map_err(|err| {
        invalid(format!(
            "eslabon data lock switch is unreadable ({err}); fix it or run `{} phi default`",
            nombre_binario_mensaje()
        ))
    })?;
    let (phi_active, effective_bases) = match &switch {
        Some(switch) => (switch.enabled, switch.allowed_base_urls.clone()),
        None => (default_phi, allowed_base_urls.clone()),
    };
    if phi_active {
        let selected_base = model_providers
            .get(selected_provider_id)
            .and_then(|info| info.base_url.clone());
        let selected_allowed = selected_base.as_deref().is_some_and(|base| {
            let mut policy = codex_api::eslabon_policy::EgressPolicy::unrestricted();
            policy.activate(effective_bases.clone());
            policy.violation(base).is_none()
        });
        if !selected_allowed {
            return Err(invalid(format!(
                "eslabon data lock is on: model provider `{selected_provider_id}` is not listed in eslabon.phi_providers (turn it off with `{} phi off` if this session handles no protected data)",
                nombre_binario_mensaje()
            )));
        }
    }
    if default_phi {
        // Only an in-effect default needs providers; with an empty list the registered
        // default still blocks everything if the switch is later cleared (fail-closed).
        if switch.is_none() && allowed_base_urls.is_empty() {
            return Err(invalid(
                "eslabon data_policy = \"phi\" requires providers with base_url in eslabon.phi_providers"
                    .to_string(),
            ));
        }
    }
    Ok((
        DataPolicyOutcome {
            phi_active,
            origen_activo: candado_de_origen_activo(
                eslabon,
                model_providers,
                selected_provider_id,
                codex_home,
                auth_credentials_store_mode,
            ),
            allowed_base_urls,
        },
        default_phi,
    ))
}

/// eslabon: whether the origin rule must deny OpenAI-family hosts for this session.
///
/// The rule is on only when nothing legitimately uses OpenAI: Eslabon mode, a
/// non-OpenAI provider, no stored OpenAI credentials and no explicit opt-in.
pub(crate) fn candado_de_origen_activo(
    eslabon: Option<&EslabonToml>,
    model_providers: &HashMap<String, ModelProviderInfo>,
    selected_provider_id: &str,
    codex_home: &std::path::Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> bool {
    if !modo_eslabon() {
        return false;
    }
    if eslabon
        .and_then(|eslabon| eslabon.permitir_openai)
        .unwrap_or(false)
    {
        return false;
    }
    let provider_es_de_openai = selected_provider_id == OPENAI_PROVIDER_ID
        || model_providers
            .get(selected_provider_id)
            .and_then(|info| info.base_url.as_deref())
            .is_some_and(codex_api::eslabon_policy::pertenece_al_origen);
    if provider_es_de_openai {
        return false;
    }
    // No network: reads the stored payload (file or keyring) only. Fail-closed:
    // an unreadable store counts as no credentials, so the rule stays on.
    !hay_auth_de_openai(codex_home, auth_credentials_store_mode)
}

fn hay_auth_de_openai(
    codex_home: &std::path::Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> bool {
    match codex_login::load_auth_dot_json(
        codex_home,
        auth_credentials_store_mode,
        AuthKeyringBackendKind::default(),
    ) {
        Ok(Some(auth)) => {
            auth.tokens.is_some()
                || auth.openai_api_key.is_some()
                || auth.personal_access_token.is_some()
                || auth.agent_identity.is_some()
        }
        Ok(None) | Err(_) => false,
    }
}

fn invalid(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::config::ConfigOverrides;
    use codex_config::config_toml::ConfigToml;
    use core_test_support::TempDirExt;
    use std::sync::Mutex;
    use std::sync::MutexGuard;
    use std::sync::PoisonError;

    /// eslabon: the origin rule lives in a process-wide atomic; serialize the
    /// tests that touch it so they never read each other's value.
    static ORIGEN_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn aislar_origen() -> MutexGuard<'static, ()> {
        ORIGEN_TEST_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Pins the Eslabon-mode answer for this test thread and clears it on drop.
    struct ModoFijado;

    impl ModoFijado {
        fn nuevo(modo: bool) -> Self {
            fijar_modo_eslabon_para_prueba(Some(modo));
            Self
        }
    }

    impl Drop for ModoFijado {
        fn drop(&mut self) {
            fijar_modo_eslabon_para_prueba(None);
        }
    }

    async fn cargar_config(toml: &str) -> Config {
        let cfg: ConfigToml = toml::from_str(toml).expect("config TOML válido");
        let home = tempfile::tempdir().expect("tempdir");
        Config::load_from_base_config_with_overrides(cfg, ConfigOverrides::default(), home.abs())
            .await
            .expect("la config carga")
    }

    #[tokio::test]
    async fn modo_eslabon_apaga_redes_de_arranque_con_home_vacio() {
        let _modo = ModoFijado::nuevo(true);
        let config = cargar_config("").await;
        assert_eq!(config.analytics_enabled, Some(false));
        assert!(!config.feedback_enabled);
        assert!(!config.check_for_update_on_startup);
        assert!(!config.features.enabled(Feature::Plugins));
        assert!(!config.eslabon_anuncios);
        // eslabon: with an empty home the implicit provider is OpenAI, so the
        // origin rule is off; it switches on when the provider is not OpenAI's.
        assert!(!config.eslabon_origen_activo);
    }

    #[tokio::test]
    async fn claves_explicitas_del_usuario_ganan_en_modo_eslabon() {
        let _modo = ModoFijado::nuevo(true);
        let config = cargar_config(
            r#"
check_for_update_on_startup = true

[analytics]
enabled = true

[features]
plugins = true

[eslabon]
anuncios = true
"#,
        )
        .await;
        assert_eq!(config.analytics_enabled, Some(true));
        assert!(config.check_for_update_on_startup);
        assert!(config.features.enabled(Feature::Plugins));
        assert!(config.eslabon_anuncios);
    }

    #[tokio::test]
    async fn fuera_del_modo_los_cuatro_valores_siguen_a_upstream() {
        let _modo = ModoFijado::nuevo(false);
        let config = cargar_config("").await;
        assert_eq!(config.analytics_enabled, None);
        assert!(config.feedback_enabled);
        assert!(config.check_for_update_on_startup);
        assert!(config.features.enabled(Feature::Plugins));
        assert!(config.eslabon_anuncios);
    }

    #[tokio::test]
    async fn feedback_explicito_en_true_gana_en_modo_eslabon() {
        let _modo = ModoFijado::nuevo(true);
        let config = cargar_config("[feedback]\nenabled = true\n").await;
        assert!(config.feedback_enabled);
    }

    #[tokio::test]
    async fn feedback_explicito_en_false_gana_fuera_del_modo() {
        let _modo = ModoFijado::nuevo(false);
        let config = cargar_config("[feedback]\nenabled = false\n").await;
        assert!(!config.feedback_enabled);
    }

    #[test]
    fn una_recarga_con_origen_inactivo_apaga_la_politica_global() {
        let _guard = aislar_origen();
        // A previous load had the rule on; a reload that decides off must clear
        // the remembered state instead of only ever turning it on.
        codex_api::eslabon_policy::set_origin_policy(true);
        assert!(codex_api::eslabon_policy::origin_policy_active());
        aplicar_candado_de_origen(false);
        assert!(!codex_api::eslabon_policy::origin_policy_active());
        codex_api::eslabon_policy::set_origin_policy(false);
    }

    fn provider(base_url: &str) -> ModelProviderInfo {
        ModelProviderInfo {
            name: base_url.to_string(),
            base_url: Some(base_url.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_external_provider_and_unknown_listing() {
        let providers = HashMap::from([
            ("local".to_string(), provider("http://127.0.0.1:11434/v1")),
            (
                "novita".to_string(),
                provider("https://api.novita.ai/openai"),
            ),
        ]);
        let phi = EslabonToml {
            data_policy: Some(EslabonDataPolicy::Phi),
            desatendido: None,
            notify_aprobacion: None,
            phi_providers: None,
            anuncios: None,
            permitir_openai: None,
        };
        let home = tempfile::tempdir().expect("tempdir");
        let err = evaluate_data_policy(
            Some(&phi),
            &providers,
            "novita",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect_err("blocked");
        assert!(err.to_string().contains("not listed"), "{err}");
        // A loopback provider is not trusted implicitly: it may be a tunnel to anywhere.
        let err = evaluate_data_policy(
            Some(&phi),
            &providers,
            "local",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect_err("loopback");
        assert!(err.to_string().contains("not listed"), "{err}");

        let typo = EslabonToml {
            data_policy: Some(EslabonDataPolicy::Phi),
            desatendido: None,
            notify_aprobacion: None,
            phi_providers: Some(vec!["vertx".to_string()]),
            anuncios: None,
            permitir_openai: None,
        };
        let err = evaluate_data_policy(
            Some(&typo),
            &providers,
            "local",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect_err("typo");
        assert!(err.to_string().contains("`vertx`"), "{err}");

        let standard = EslabonToml {
            data_policy: Some(EslabonDataPolicy::Standard),
            desatendido: None,
            notify_aprobacion: None,
            phi_providers: None,
            anuncios: None,
            permitir_openai: None,
        };
        let outcome = evaluate_data_policy(
            Some(&standard),
            &providers,
            "novita",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect("standard")
        .0;
        assert!(!outcome.phi_active);
        assert!(
            !evaluate_data_policy(
                None,
                &providers,
                "novita",
                home.path(),
                AuthCredentialsStoreMode::File,
            )
            .expect("none")
            .0
            .phi_active
        );

        // The runtime switch overrides the default in both directions.
        let switch = codex_api::eslabon_policy::switch_path(home.path());
        codex_api::eslabon_policy::write_switch(
            &switch,
            &codex_api::eslabon_policy::PhiSwitch {
                enabled: false,
                allowed_base_urls: Vec::new(),
            },
        )
        .expect("off");
        let outcome = evaluate_data_policy(
            Some(&phi),
            &providers,
            "novita",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect("switch off lets a standard provider start")
        .0;
        assert!(!outcome.phi_active);
        codex_api::eslabon_policy::write_switch(
            &switch,
            &codex_api::eslabon_policy::PhiSwitch {
                enabled: true,
                allowed_base_urls: vec!["http://127.0.0.1:11434/v1".to_string()],
            },
        )
        .expect("on");
        let err = evaluate_data_policy(
            Some(&standard),
            &providers,
            "novita",
            home.path(),
            AuthCredentialsStoreMode::File,
        )
        .expect_err("switch on blocks an unlisted provider");
        assert!(err.to_string().contains("data lock is on"), "{err}");
    }

    #[test]
    fn candado_de_origen_solo_cuando_nada_usa_openai() {
        let home = tempfile::tempdir().expect("tempdir");
        let providers = HashMap::from([("local".to_string(), provider("http://127.0.0.1:11434/v1"))]);

        {
            let _modo = ModoFijado::nuevo(false);
            assert!(!candado_de_origen_activo(
                None,
                &providers,
                "local",
                home.path(),
                AuthCredentialsStoreMode::File,
            ));
        }

        let _modo = ModoFijado::nuevo(true);
        assert!(candado_de_origen_activo(
            None,
            &providers,
            "local",
            home.path(),
            AuthCredentialsStoreMode::File,
        ));

        let openai = provider("https://api.openai.com/v1");
        let con_openai = HashMap::from([("openai".to_string(), openai)]);
        assert!(!candado_de_origen_activo(
            None,
            &con_openai,
            "openai",
            home.path(),
            AuthCredentialsStoreMode::File,
        ));

        // Any provider whose base URL is a host of the OpenAI family opts out.
        let proxy = HashMap::from([("proxy".to_string(), provider("https://api.openai.com/v1"))]);
        assert!(!candado_de_origen_activo(
            None,
            &proxy,
            "proxy",
            home.path(),
            AuthCredentialsStoreMode::File,
        ));

        let permitido = EslabonToml {
            permitir_openai: Some(true),
            ..Default::default()
        };
        assert!(!candado_de_origen_activo(
            Some(&permitido),
            &providers,
            "local",
            home.path(),
            AuthCredentialsStoreMode::File,
        ));

        std::fs::write(
            home.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-prueba"}"#,
        )
        .expect("auth.json");
        assert!(!candado_de_origen_activo(
            None,
            &providers,
            "local",
            home.path(),
            AuthCredentialsStoreMode::File,
        ));
    }

    #[tokio::test]
    async fn login_explicito_apaga_el_candado_de_origen() {
        let _guard = aislar_origen();
        // eslabon: the rule the config load decides, then the explicit override that
        // `eslabon login`/`logout` apply at their config load entry point.
        let _modo = ModoFijado::nuevo(true);
        let config = cargar_config(
            "model = \"qwen-coder\"\nmodel_provider = \"local\"\n\n[model_providers.local]\nname = \"local\"\nbase_url = \"http://127.0.0.1:11434/v1\"\nwire_api = \"chat\"\n",
        )
        .await;
        assert!(config.eslabon_origen_activo, "sin login el candado está activo");

        let mut config = config;
        crate::config::permitir_openai_explicito(&mut config);
        assert!(!config.eslabon_origen_activo, "login deja el candado apagado");
        assert!(
            !codex_api::eslabon_policy::origin_policy_active(),
            "la regla de origen queda apagada en el proceso"
        );
    }
}
