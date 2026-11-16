//! eslabon: discovery of the local model servers a first run can use.
//!
//! Only loopback is probed (enforced by `SondeoHttp`); every candidate answers the
//! OpenAI-compatible `GET /v1/models` with `{"data":[{"id":"..."}]}`. A port that
//! does not answer, or answers with something else, is ignored.

use crate::sondeo::Sondeo;
use serde_json::Value as Json;

/// (port, friendly name) of the servers a first run looks for. The base path is
/// `/v1` on all of them and only loopback is probed.
pub const SERVIDORES_HABITUALES: &[(u16, &str)] = &[
    (11434, "Ollama"),
    (1234, "LM Studio"),
    (8080, "llama.cpp"),
    (8000, "vLLM"),
];

/// One detected server and the models it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServidorLocal {
    pub nombre: String,
    pub base_url: String,
    pub modelos: Vec<String>,
}

/// Probes the habitual ports and returns the servers that answered with models.
pub fn detectar(sondeo: &dyn Sondeo) -> Vec<ServidorLocal> {
    SERVIDORES_HABITUALES
        .iter()
        .filter_map(|&(puerto, nombre)| {
            let base_url = format!("http://127.0.0.1:{puerto}/v1");
            let cuerpo = sondeo.get(&format!("{base_url}/models"))?;
            let modelos = modelos_de(&cuerpo);
            (!modelos.is_empty()).then(|| ServidorLocal {
                nombre: nombre.to_string(),
                base_url,
                modelos,
            })
        })
        .collect()
}

/// Model ids of an OpenAI-compatible `{"data":[{"id":...}]}` body; anything that
/// does not parse is ignored.
fn modelos_de(cuerpo: &str) -> Vec<String> {
    cuerpo
        .parse::<Json>()
        .ok()
        .and_then(|json| json.get("data").and_then(Json::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|modelo| modelo.get("id").and_then(Json::as_str))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    struct Falso(HashMap<String, String>);

    impl Sondeo for Falso {
        fn get(&self, url: &str) -> Option<String> {
            self.0.get(url).cloned()
        }
    }

    fn respuesta(modelos: &[&str]) -> String {
        let ids: Vec<String> = modelos
            .iter()
            .map(|id| format!(r#"{{"id":"{id}"}}"#))
            .collect();
        format!(r#"{{"data":[{}]}}"#, ids.join(","))
    }

    #[test]
    fn detecta_cada_servidor_habitual() {
        let falso = Falso(
            SERVIDORES_HABITUALES
                .iter()
                .map(|&(puerto, _)| {
                    (
                        format!("http://127.0.0.1:{puerto}/v1/models"),
                        respuesta(&["local-1"]),
                    )
                })
                .collect(),
        );
        let servidores = detectar(&falso);
        assert_eq!(servidores.len(), SERVIDORES_HABITUALES.len());
        let nombres: Vec<&str> = servidores.iter().map(|s| s.nombre.as_str()).collect();
        assert_eq!(nombres, vec!["Ollama", "LM Studio", "llama.cpp", "vLLM"]);
        assert_eq!(
            servidores[0].base_url,
            "http://127.0.0.1:11434/v1".to_string()
        );
        assert_eq!(servidores[0].modelos, vec!["local-1".to_string()]);
    }

    #[test]
    fn respuesta_invalida_se_ignora() {
        let falso = Falso(HashMap::from([
            (
                "http://127.0.0.1:11434/v1/models".to_string(),
                "no es json".to_string(),
            ),
            (
                "http://127.0.0.1:1234/v1/models".to_string(),
                r#"{"data":[{"sin_id":true}]}"#.to_string(),
            ),
            (
                "http://127.0.0.1:8080/v1/models".to_string(),
                respuesta(&["si-1"]),
            ),
        ]));
        let servidores = detectar(&falso);
        assert_eq!(servidores.len(), 1);
        assert_eq!(servidores[0].nombre, "llama.cpp");
        assert_eq!(servidores[0].modelos, vec!["si-1".to_string()]);
    }

    #[test]
    fn sin_servidores_arriba_devuelve_vacio() {
        assert_eq!(detectar(&Falso(HashMap::new())), Vec::new());
    }
}
