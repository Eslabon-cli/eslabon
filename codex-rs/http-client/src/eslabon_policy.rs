//! eslabon: data-egress policy for sessions that handle protected data (PHI) and
//! the origin rule that keeps Eslabon-mode sessions off OpenAI services.
//!
//! When the PHI block is on, only the base URLs of `[eslabon] phi_providers` may
//! receive requests. When the origin rule is on (decided at config load), hosts
//! of the OpenAI family are denied instead. The checks live in the shared HTTP
//! transport (every request, every redirect hop, and the gate for SDKs without
//! destination checks), so model clients, extensions, remote MCP servers and
//! telemetry cannot route around them. WebSocket clients, which bypass this
//! transport, check in codex-api.
//!
//! Two layers decide whether the block applies:
//! * the session default, from `[eslabon] data_policy` when the config loads; and
//! * a runtime switch, `$CODEX_HOME/eslabon-phi.json`, written by `codex phi on|off`
//!   or the sidebar. While the file exists it overrides the default in either
//!   direction. It is a file, not memory, so it reaches the core whether it runs in
//!   the TUI process or in a separate app-server daemon.
//!
//! Fail-closed: an unreadable switch file blocks every request.

use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use reqwest::Url;

/// eslabon: whether the origin lock is on. Independent of the PHI block.
static ORIGIN_POLICY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// eslabon: turns on the origin rule (OpenAI-family hosts denied).
///
/// The config load decides whether to call this (see `codex-core`'s
/// `eslabon::apply_data_policy`); tests call it directly.
pub fn set_origin_policy(active: bool) {
    ORIGIN_POLICY_ACTIVE.store(active, Ordering::Relaxed);
}

/// Whether the origin rule applies right now.
pub fn origin_policy_active() -> bool {
    ORIGIN_POLICY_ACTIVE.load(Ordering::Relaxed)
}

/// eslabon: whether `url` belongs to an OpenAI service, independent of the rule
/// being active. Used at config load to recognize an OpenAI provider base URL.
pub fn pertenece_al_origen(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .is_some_and(|url| origen_denegado(&url).is_some())
}

/// eslabon: denies OpenAI-family destinations while the origin rule is on.
///
/// The rule is independent of the PHI block: either lock denying is enough.
pub fn origin_violation(url: &str) -> Option<String> {
    if !origin_policy_active() {
        return None;
    }
    let url = Url::parse(url).ok()?;
    origen_denegado(&url).map(|destination| {
        format!(
            "blocked by eslabon origin policy: `{destination}` is an OpenAI service; \
             allow it with `[eslabon] permitir_openai = true`, or run `eslabon login`"
        )
    })
}

/// eslabon: the OpenAI-family hosts (and GitHub OpenAI paths) denied by the rule.
fn origen_denegado(url: &Url) -> Option<String> {
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if es_host_de_openai(&host) {
        return Some(host);
    }
    let prefix = match host.as_str() {
        "github.com" => "/openai",
        "api.github.com" => "/repos/openai",
        "raw.githubusercontent.com" => "/openai",
        _ => return None,
    };
    let path = url.path();
    // GitHub org/repo names are case-insensitive; the path keeps its case in the URL.
    let path = path.to_ascii_lowercase();
    if path == prefix || path.starts_with(&format!("{prefix}/")) {
        return Some(format!("{host}{path}"));
    }
    None
}

/// eslabon: OpenAI owns these domains and their subdomains.
fn es_host_de_openai(host: &str) -> bool {
    [
        "chatgpt.com",
        "openai.com",
        "oaistatic.com",
        "oaiusercontent.com",
    ]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// File name of the runtime switch inside `$CODEX_HOME`.
pub const SWITCH_FILE_NAME: &str = "eslabon-phi.json";

/// Contents of the runtime switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhiSwitch {
    pub enabled: bool,
    /// Base URLs of the providers allowed while enabled (from `phi_providers`).
    #[serde(default)]
    pub allowed_base_urls: Vec<String>,
}

static SWITCH_PATH: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, PartialEq)]
enum SwitchRead {
    Absent,
    Present(PhiSwitch),
    Unreadable(String),
}

static SWITCH_CACHE: Mutex<Option<(Option<SystemTime>, SwitchRead)>> = Mutex::new(None);

/// Points the policy at `$CODEX_HOME/eslabon-phi.json`. Called when the config loads.
pub fn set_switch_dir(codex_home: &Path) {
    let _ = SWITCH_PATH.set(codex_home.join(SWITCH_FILE_NAME));
}

pub fn switch_path(codex_home: &Path) -> PathBuf {
    codex_home.join(SWITCH_FILE_NAME)
}

/// Reads the switch file directly (no cache): `Ok(None)` when it does not exist.
pub fn read_switch(path: &Path) -> Result<Option<PhiSwitch>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// Turns the block on or off for every eslabon process using this `$CODEX_HOME`.
pub fn write_switch(path: &Path, switch: &PhiSwitch) -> Result<(), String> {
    if switch.enabled && switch.allowed_base_urls.is_empty() {
        return Err(
            "cannot turn the data lock on: no provider is listed in [eslabon] phi_providers"
                .to_string(),
        );
    }
    let text = serde_json::to_string_pretty(switch).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|err| format!("{}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| format!("{}: {err}", path.display()))
}

/// Removes the switch so the session default (`data_policy`) applies again.
pub fn clear_switch(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

fn current_switch() -> SwitchRead {
    let Some(path) = SWITCH_PATH.get() else {
        return SwitchRead::Absent;
    };
    let modified = std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
    let mut cache = SWITCH_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached_modified, read)) = cache.as_ref()
        && *cached_modified == modified
    {
        return read.clone();
    }
    let read = match read_switch(path) {
        Ok(None) => SwitchRead::Absent,
        Ok(Some(switch)) => SwitchRead::Present(switch),
        Err(err) => SwitchRead::Unreadable(err),
    };
    *cache = Some((modified, read.clone()));
    read
}

/// Allowed base URLs; `None` means no restriction.
#[derive(Debug, Default)]
pub struct EgressPolicy {
    allowed: Option<Vec<Url>>,
}

impl EgressPolicy {
    pub const fn unrestricted() -> Self {
        Self { allowed: None }
    }

    pub fn activate(&mut self, allowed_base_urls: impl IntoIterator<Item = String>) {
        let allowed = self.allowed.get_or_insert_with(Vec::new);
        for base in allowed_base_urls {
            if let Ok(url) = Url::parse(base.trim_end_matches('/')) {
                allowed.push(normalize(url));
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.allowed.is_some()
    }

    pub fn violation(&self, url: &str) -> Option<String> {
        let allowed = self.allowed.as_ref()?;
        let target = match Url::parse(url) {
            Ok(url) => normalize(url),
            Err(_) => return Some(blocked_message(url)),
        };
        if allowed.iter().any(|base| is_under(&target, base)) {
            None
        } else {
            Some(blocked_message(url))
        }
    }
}

static POLICY: RwLock<EgressPolicy> = RwLock::new(EgressPolicy::unrestricted());

/// Activates the PHI policy, allowing requests only to the given base URLs.
pub fn activate_phi_policy(allowed_base_urls: impl IntoIterator<Item = String>) {
    POLICY
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .activate(allowed_base_urls);
}

/// Whether the PHI block applies right now (runtime switch, else session default).
pub fn phi_policy_active() -> bool {
    match current_switch() {
        SwitchRead::Present(switch) => switch.enabled,
        SwitchRead::Unreadable(_) => true,
        SwitchRead::Absent => POLICY.read().unwrap_or_else(PoisonError::into_inner).is_active(),
    }
}

/// Returns an error message when the policy forbids sending a request to `url`.
pub fn egress_violation(url: &str) -> Option<String> {
    // eslabon: the origin rule is independent of the PHI block; either one denying
    // is enough, so it is checked first and its message names the OpenAI host.
    if let Some(message) = origin_violation(url) {
        return Some(message);
    }
    match current_switch() {
        SwitchRead::Present(switch) if !switch.enabled => None,
        SwitchRead::Present(switch) => {
            let mut policy = EgressPolicy::unrestricted();
            policy.activate(switch.allowed_base_urls);
            policy.violation(url)
        }
        SwitchRead::Unreadable(err) => Some(format!(
            "blocked by eslabon data lock: the data lock switch file is unreadable ({err}); fix or delete it"
        )),
        SwitchRead::Absent => POLICY.read().unwrap_or_else(PoisonError::into_inner).violation(url),
    }
}

/// Transport-level gate: denies a request (or redirect hop) the policy forbids.
pub fn check(url: &str) -> Result<(), crate::NetworkPolicyDenied> {
    match egress_violation(url) {
        Some(message) => {
            tracing::warn!("{message}");
            Err(crate::NetworkPolicyDenied::Destination)
        }
        None => Ok(()),
    }
}

fn blocked_message(url: &str) -> String {
    let host = Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_else(|| url.to_string());
    format!(
        "blocked by eslabon data lock: the data lock is on and `{host}` is not one of the [eslabon] phi_providers"
    )
}

/// WebSocket URLs are compared as their HTTP equivalents.
fn normalize(mut url: Url) -> Url {
    let scheme = match url.scheme() {
        "ws" => Some("http"),
        "wss" => Some("https"),
        _ => None,
    };
    if let Some(scheme) = scheme {
        let _ = url.set_scheme(scheme);
    }
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn is_under(target: &Url, base: &Url) -> bool {
    if target.scheme() != base.scheme()
        || target.host_str() != base.host_str()
        || target.port_or_known_default() != base.port_or_known_default()
    {
        return false;
    }
    let base_path = base.path().trim_end_matches('/');
    let path = target.path();
    path == base_path
        || base_path.is_empty()
        || path
            .strip_prefix(base_path)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn allows_only_local_and_baa_bases_under_phi() {
        // A local instance: the process-wide policy stays untouched for other tests.
        let mut policy = EgressPolicy::unrestricted();
        assert_eq!(policy.violation("https://api.novita.ai/openai/chat/completions"), None);

        policy.activate([
            "http://127.0.0.1:11434/v1".to_string(),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/example-project/".to_string(),
        ]);
        assert!(policy.is_active());
        assert_eq!(policy.violation("http://127.0.0.1:11434/v1/chat/completions"), None);
        assert_eq!(policy.violation("ws://127.0.0.1:11434/v1/responses"), None);
        assert_eq!(
            policy.violation(
                "https://us-central1-aiplatform.googleapis.com/v1/projects/example-project/locations/x"
            ),
            None
        );

        for blocked in [
            "https://api.novita.ai/openai/chat/completions",
            "http://127.0.0.1:9999/v1/chat/completions",
            "http://127.0.0.1:11434/v1beta/chat",
            "https://us-central1-aiplatform.googleapis.com/v1/projects/other",
            "wss://api.openai.com/v1/responses",
            "not a url",
        ] {
            let message = policy.violation(blocked).unwrap_or_default();
            assert!(message.starts_with("blocked by eslabon data lock"), "{blocked}");
            // eslabon: the text the user sees names the data lock, never PHI.
            assert!(message.contains("data lock"), "{blocked}");
            assert!(!message.contains("PHI"), "{blocked}");
        }
        assert!(!phi_policy_active(), "the process-wide policy was not touched");
    }

    #[test]
    fn switch_file_round_trip_and_guard() {
        // The origin rule is off in this test, so egress decisions stay untouched.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = switch_path(dir.path());
        assert_eq!(read_switch(&path), Ok(None));
        let err = write_switch(
            &path,
            &PhiSwitch {
                enabled: true,
                allowed_base_urls: Vec::new(),
            },
        )
        .expect_err("enabling with no providers is refused");
        assert!(err.contains("data lock"), "{err}");
        assert!(!err.contains("PHI"), "{err}");
        assert!(err.contains("phi_providers"), "{err}");
        let on = PhiSwitch {
            enabled: true,
            allowed_base_urls: vec!["https://us-aiplatform.googleapis.com/v1/projects/p".to_string()],
        };
        write_switch(&path, &on).expect("write");
        assert_eq!(read_switch(&path), Ok(Some(on)));
        let off = PhiSwitch {
            enabled: false,
            allowed_base_urls: Vec::new(),
        };
        write_switch(&path, &off).expect("write off");
        assert_eq!(read_switch(&path), Ok(Some(off)));
        clear_switch(&path).expect("clear");
        assert_eq!(read_switch(&path), Ok(None));
        std::fs::write(&path, "not json").expect("corrupt");
        assert!(read_switch(&path).is_err());
    }

    /// Pins the process-wide origin rule only for the test body and restores it.
    static ORIGEN_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct OrigenActivo;

    fn origen_activo() -> OrigenActivo {
        set_origin_policy(true);
        OrigenActivo
    }

    impl Drop for OrigenActivo {
        fn drop(&mut self) {
            set_origin_policy(false);
        }
    }

    #[test]
    fn origin_rule_denies_every_openai_host_family() {
        let _guard = ORIGEN_TEST_LOCK.lock().expect("origin test lock");
        let _origen = origen_activo();
        for denied in [
            "https://chatgpt.com/backend-api/plugins/export/curated",
            "https://ab.chatgpt.com/otlp/v1/metrics",
            "https://chat.openai.com/",
            "https://openai.com/",
            "https://api.openai.com/v1/responses",
            "https://auth.openai.com/oauth/token",
            "https://oaistatic.com/",
            "https://persistent.oaistatic.com/codex/pets/v1/pet.webp",
            "https://oaiusercontent.com/",
            "https://files.oaiusercontent.com/file-123",
        ] {
            let message = origin_violation(denied).unwrap_or_default();
            assert!(
                message.starts_with("blocked by eslabon origin policy")
                    && message.contains("permitir_openai")
                    && message.contains("eslabon login"),
                "{denied}: {message}"
            );
        }
    }

    #[test]
    fn origin_rule_denies_openai_paths_on_github_hosts_only() {
        let _guard = ORIGEN_TEST_LOCK.lock().expect("origin test lock");
        let _origen = origen_activo();
        for denied in [
            "https://github.com/openai/plugins.git",
            "https://github.com/OpenAI/plugins.git",
            "https://github.com/openai/codex",
            "https://api.github.com/repos/OpenAI/codex",
            "https://api.github.com/repos/openai/codex/releases/latest",
            "https://api.github.com/repos/openai/plugins",
            "https://raw.githubusercontent.com/openai/codex/main/announcement_tip.toml",
        ] {
            assert!(origin_violation(denied).is_some(), "{denied}");
        }
        for allowed in [
            "https://github.com/otro/proyecto.git",
            "https://github.com/otro",
            "https://api.github.com/repos/otro/proyecto",
            "https://raw.githubusercontent.com/otro/proyecto/main/archivo.txt",
            "https://example.com/",
            "https://models.dev/api.json",
            "http://127.0.0.1:11434/v1/models",
        ] {
            assert_eq!(origin_violation(allowed), None, "{allowed}");
        }
    }

    #[test]
    fn origin_rule_stays_off_until_activated() {
        let _guard = ORIGEN_TEST_LOCK.lock().expect("origin test lock");
        set_origin_policy(false);
        assert!(!origin_policy_active());
        assert_eq!(origin_violation("https://api.openai.com/v1/models"), None);
        assert_eq!(egress_violation("https://api.openai.com/v1/models"), None);
        assert!(pertenece_al_origen("https://api.openai.com/v1"));
        assert!(!pertenece_al_origen("https://example.com/"));
    }

    #[test]
    fn origin_and_phi_are_independent_locks() {
        let _guard = ORIGEN_TEST_LOCK.lock().expect("origin test lock");
        let mut phi = EgressPolicy::unrestricted();
        phi.activate(["https://api.openai.com".to_string()]);
        assert_eq!(phi.violation("https://api.openai.com/v1/models"), None);

        let _origen = origen_activo();
        let message = origin_violation("https://api.openai.com/v1/models").unwrap_or_default();
        assert!(message.contains("eslabon origin policy"), "{message}");
        assert!(message.contains("permitir_openai"), "{message}");
        assert!(message.contains("eslabon login"), "{message}");
    }
}
