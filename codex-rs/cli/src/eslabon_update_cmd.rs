// eslabon: `eslabon update --check`, the on-demand release check (P7.3).
//
// Update checks stay off unless the user asks for one: with `--check` the CLI
// makes a single anonymous GET to the public releases API, compares the latest
// tag with this build's version and prints one line. `ESLABON_RELEASES_API`
// overrides the endpoint so a local server can answer it in tests.
use anyhow::Context;
use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const API_POR_DEFECTO: &str = "https://api.github.com/repos/Eslabon-cli/eslabon/releases/latest";
const VARIABLE_API: &str = "ESLABON_RELEASES_API";
const ESPERA: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
struct Lanzamiento {
    tag_name: String,
}

pub(crate) fn api_de_lanzamientos() -> String {
    match std::env::var(VARIABLE_API) {
        Ok(valor) if !valor.trim().is_empty() => valor.trim().to_string(),
        _ => API_POR_DEFECTO.to_string(),
    }
}

pub(crate) fn version_actual() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn version_de_etiqueta(etiqueta: &str) -> anyhow::Result<Version> {
    let limpia = etiqueta.trim().trim_start_matches('v');
    Version::parse(limpia)
        .with_context(|| format!("release tag `{etiqueta}` is not a semver version"))
}

fn mensaje_de_revision(actual: &Version, publicada: &Version) -> String {
    if publicada > actual {
        format!(
            "version {publicada} is available: {}/tag/v{publicada}; install with the script in the README",
            codex_config::eslabon::marca::URL_RELEASES
        )
    } else {
        "up to date".to_string()
    }
}

async fn comprobar_version(api: &str, actual: &Version) -> anyhow::Result<()> {
    let cliente = reqwest::Client::builder()
        .timeout(ESPERA)
        .build()
        .context("could not build the release-check HTTP client")?;
    let respuesta = cliente
        .get(api)
        // eslabon: no headers identify the user; only the product and version.
        .header(reqwest::header::USER_AGENT, format!("eslabon/{actual}"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("release check failed: could not reach {api}"))?;
    if !respuesta.status().is_success() {
        let codigo = respuesta.status();
        anyhow::bail!("release check failed: {api} answered {codigo}");
    }
    let lanzamiento: Lanzamiento = respuesta
        .json()
        .await
        .with_context(|| format!("release check failed: {api} returned an unexpected payload"))?;
    let publicada = version_de_etiqueta(&lanzamiento.tag_name)?;
    println!("{}", mensaje_de_revision(actual, &publicada));
    Ok(())
}

pub(crate) async fn run_check() -> anyhow::Result<()> {
    if !codex_config::eslabon::modo_eslabon() {
        anyhow::bail!("update --check is only available in Eslabon mode");
    }
    comprobar_version(&api_de_lanzamientos(), &version_actual()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn version(texto: &str) -> Version {
        Version::parse(texto).expect("valid test version")
    }

    fn actual() -> Version {
        version("0.167.2")
    }

    #[test]
    fn las_versiones_se_comparan_por_semver() {
        assert_eq!(
            mensaje_de_revision(&actual(), &version("0.167.2")),
            "up to date"
        );
        assert_eq!(
            mensaje_de_revision(&actual(), &version("0.167.1")),
            "up to date"
        );
        assert!(mensaje_de_revision(&actual(), &version("0.168.0")).contains("0.168.0"));
        assert!(mensaje_de_revision(&actual(), &version("1.0.0")).contains("1.0.0"));
        // eslabon: a prerelease sorts below its final release and above older ones.
        assert_eq!(
            mensaje_de_revision(&version("0.168.0"), &version("0.168.0-rc.1")),
            "up to date"
        );
        assert!(
            mensaje_de_revision(&version("0.167.9"), &version("0.168.0-beta.1"))
                .contains("0.168.0-beta.1")
        );
    }

    #[test]
    fn el_aviso_trae_la_url_del_tag_y_el_comando_de_instalacion() {
        let mensaje = mensaje_de_revision(&actual(), &version("0.168.0"));
        assert_eq!(
            mensaje,
            "version 0.168.0 is available: https://github.com/Eslabon-cli/eslabon/releases/tag/v0.168.0; install with the script in the README"
        );
    }

    #[test]
    fn la_etiqueta_acepta_la_v_o_no() {
        assert_eq!(
            version_de_etiqueta("v0.168.0").expect("tag"),
            version("0.168.0")
        );
        assert_eq!(
            version_de_etiqueta("0.168.0").expect("tag"),
            version("0.168.0")
        );
    }

    #[test]
    fn una_etiqueta_sin_semver_falla() {
        let error = version_de_etiqueta("latest").expect_err("bad tag");
        assert!(
            error.to_string().contains("not a semver version"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn una_respuesta_simulada_con_una_version_nueva_no_falla() {
        let servidor = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v0.168.0",
                // eslabon: the response is simulated; the html_url must be ignored.
                "html_url": "https://github.com/Eslabon-cli/eslabon/releases/tag/other"
            })))
            .expect(1)
            .mount(&servidor)
            .await;
        let api = format!("{}/releases/latest", servidor.uri());
        comprobar_version(&api, &actual())
            .await
            .expect("release check with a simulated response");
        let peticiones = servidor.received_requests().await.expect("requests");
        assert_eq!(peticiones.len(), 1);
    }

    #[tokio::test]
    async fn la_peticion_se_identifica_con_el_producto_y_la_version() {
        let servidor = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .and(header("user-agent", format!("eslabon/{}", actual())))
            .and(header("accept", "application/vnd.github+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v0.167.2"
            })))
            .expect(1)
            .mount(&servidor)
            .await;
        let api = format!("{}/releases/latest", servidor.uri());
        comprobar_version(&api, &actual())
            .await
            .expect("release check with a simulated response");
    }

    #[tokio::test]
    async fn un_servidor_de_github_con_error_es_un_error_claro() {
        let servidor = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .respond_with(
                ResponseTemplate::new(500).set_body_string("the GitHub API is having a bad day"),
            )
            .expect(1)
            .mount(&servidor)
            .await;
        let api = format!("{}/releases/latest", servidor.uri());
        let error = comprobar_version(&api, &actual())
            .await
            .expect_err("a 500 is a failed check");
        assert!(error.to_string().contains("answered 500"), "{error}");
    }

    #[tokio::test]
    async fn una_respuesta_sin_tag_name_es_un_error() {
        let servidor = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/releases/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1
            })))
            .expect(1)
            .mount(&servidor)
            .await;
        let api = format!("{}/releases/latest", servidor.uri());
        let error = comprobar_version(&api, &actual())
            .await
            .expect_err("missing tag_name is a failed check");
        assert!(error.to_string().contains("unexpected payload"), "{error}");
    }
}
