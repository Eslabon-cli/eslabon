//! eslabon: onboarding wizard for a local OpenAI-compatible server.
//!
//! In Eslabon mode the picker offers this wizard first, so a first run with no
//! account never has to log in to OpenAI. The wizard lists what the detector
//! found, lets the user pick a server and model (or type a URL and a model id)
//! and writes `config.toml` through the same function as `eslabon init`.
//!
//! The config write happens before the first conversation starts, so the
//! running app server is told to reload its user config instead of restarting.

use super::auth::AuthModeWidget;
use super::auth::SignInOption;
use super::auth::SignInState;
use super::keys;
use super::provider_setup::FieldDisplay;
use super::provider_setup::ProviderInput;
use crate::key_hint::KeyBindingListExt;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::RequestId;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use eslabon_config::detector::ServidorLocal;
use eslabon_config::sondeo::SondeoHttp;
use eslabon_config::sondeo::es_loopback;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::path::PathBuf;
use std::sync::PoisonError;
use uuid::Uuid;

/// Everything the wizard needs from the process: the home to write and the
/// servers the detector found (empty when nothing answered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EslabonLocalSetup {
    pub codigo_home: PathBuf,
    pub servidores: Vec<ServidorLocal>,
}

impl EslabonLocalSetup {
    /// Short label for the picker: "Ollama · 2 models" or "no server found".
    pub(super) fn etiqueta(&self) -> String {
        match self.servidores.as_slice() {
            [] => "No local server detected".to_string(),
            [servidor] => format!(
                "{} · {} model{}",
                servidor.nombre,
                servidor.modelos.len(),
                if servidor.modelos.len() == 1 { "" } else { "s" }
            ),
            servidores => {
                let nombres: Vec<&str> = servidores
                    .iter()
                    .map(|servidor| servidor.nombre.as_str())
                    .collect();
                format!(
                    "{} · {} servers detected",
                    nombres.join(", "),
                    servidores.len()
                )
            }
        }
    }
}

/// The values the wizard will write; built only when the flow is complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DatosLocal {
    pub codigo_home: PathBuf,
    pub base_url: String,
    pub modelo: String,
    pub api_key_env: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Vista {
    Servidores,
    Url,
    Modelos,
    ModeloEscrito,
    Llave,
    Guardando,
}

#[derive(Clone)]
pub(super) struct LocalSetupState {
    setup: EslabonLocalSetup,
    vista: Vista,
    elegido: usize,
    base_url: Option<String>,
    modelos: Vec<String>,
    modelo: Option<String>,
    nombre: String,
    url: ProviderInput,
    modelo_input: ProviderInput,
    env_key: ProviderInput,
    error: Option<String>,
}

pub(super) enum AccionLocal {
    Nada,
    Volver,
    Guardar,
}

impl LocalSetupState {
    pub(super) fn new(setup: EslabonLocalSetup) -> Self {
        // With nothing detected the wizard starts at the URL field; with one or
        // more servers it starts at the list.
        let vista = if setup.servidores.is_empty() {
            Vista::Url
        } else {
            Vista::Servidores
        };
        Self {
            setup,
            vista,
            elegido: 0,
            base_url: None,
            modelos: Vec::new(),
            modelo: None,
            nombre: String::new(),
            url: ProviderInput::default(),
            modelo_input: ProviderInput::default(),
            env_key: ProviderInput::default(),
            error: None,
        }
    }

    fn set_error(&mut self, mensaje: Option<String>) {
        self.error = mensaje;
    }

    fn opciones_servidores(&self) -> usize {
        self.setup.servidores.len() + 1
    }

    fn elegir_servidor(&mut self, indice: usize) {
        let Some(servidor) = self.setup.servidores.get(indice).cloned() else {
            // The extra row is "Enter a server URL".
            self.vista = Vista::Url;
            self.set_error(None);
            return;
        };
        self.base_url = Some(servidor.base_url.clone());
        self.nombre = servidor.nombre.clone();
        self.modelos = servidor.modelos.clone();
        self.elegido = 0;
        self.set_error(None);
        if self.modelos.len() == 1 {
            self.modelo = self.modelos.first().cloned();
            self.vista = Vista::Llave;
        } else if self.modelos.is_empty() {
            self.vista = Vista::ModeloEscrito;
        } else {
            self.vista = Vista::Modelos;
        }
    }

    fn confirmar_url(&mut self) {
        let Some(url) = self
            .url
            .value()
            .map(|valor| valor.trim_end_matches('/').to_string())
        else {
            self.set_error(Some("Enter the URL of your local server".to_string()));
            return;
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.set_error(Some("The URL must start with http:// or https://".to_string()));
            return;
        }
        self.base_url = Some(url.clone());
        self.nombre = eslabon_config::servidor_local::nombre_del_proveedor(&url);
        // Probing stays inside loopback, like the detector: remote URLs ask for
        // the model id instead of making a request.
        self.modelos = if es_loopback(&url) {
            eslabon_config::detector::detectar(&SondeoHttp)
                .into_iter()
                .find(|servidor| servidor.base_url == url)
                .map(|servidor| servidor.modelos)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.elegido = 0;
        self.set_error(None);
        if self.modelos.len() == 1 {
            self.modelo = self.modelos.first().cloned();
            self.vista = Vista::Llave;
        } else if self.modelos.is_empty() {
            self.vista = Vista::ModeloEscrito;
        } else {
            self.vista = Vista::Modelos;
        }
    }

    fn retroceder(&mut self) -> AccionLocal {
        self.set_error(None);
        match self.vista {
            Vista::Servidores | Vista::Guardando => AccionLocal::Volver,
            Vista::Url => {
                if self.setup.servidores.is_empty() {
                    return AccionLocal::Volver;
                }
                self.vista = Vista::Servidores;
                AccionLocal::Nada
            }
            Vista::Modelos => {
                self.vista = Vista::Servidores;
                AccionLocal::Nada
            }
            Vista::ModeloEscrito => {
                self.vista = if self.modelos.is_empty() {
                    Vista::Url
                } else {
                    Vista::Servidores
                };
                AccionLocal::Nada
            }
            Vista::Llave => {
                self.vista = if self.modelos.len() > 1 {
                    Vista::Modelos
                } else if self.modelos.is_empty() {
                    Vista::ModeloEscrito
                } else {
                    Vista::Servidores
                };
                AccionLocal::Nada
            }
        }
    }

    pub(super) fn handle_key_event(&mut self, key_event: &KeyEvent) -> AccionLocal {
        if self.vista == Vista::Guardando {
            return AccionLocal::Nada;
        }
        if keys::CANCEL.is_pressed(*key_event) {
            return self.retroceder();
        }
        match self.vista {
            Vista::Servidores => {
                if keys::MOVE_UP.is_pressed(*key_event) {
                    self.elegido =
                        (self.elegido + self.opciones_servidores() - 1) % self.opciones_servidores();
                } else if keys::MOVE_DOWN.is_pressed(*key_event) {
                    self.elegido = (self.elegido + 1) % self.opciones_servidores();
                } else if key_event.kind == KeyEventKind::Press
                    && keys::CONFIRM.is_pressed(*key_event)
                {
                    self.elegir_servidor(self.elegido);
                }
                AccionLocal::Nada
            }
            Vista::Modelos => {
                if keys::MOVE_UP.is_pressed(*key_event) {
                    self.elegido = (self.elegido + self.modelos.len() - 1) % self.modelos.len();
                } else if keys::MOVE_DOWN.is_pressed(*key_event) {
                    self.elegido = (self.elegido + 1) % self.modelos.len();
                } else if key_event.kind == KeyEventKind::Press
                    && keys::CONFIRM.is_pressed(*key_event)
                {
                    self.modelo = self.modelos.get(self.elegido).cloned();
                    if self.modelo.is_some() {
                        self.set_error(None);
                        self.vista = Vista::Llave;
                    }
                }
                AccionLocal::Nada
            }
            Vista::Url => {
                if key_event.kind == KeyEventKind::Press && keys::CONFIRM.is_pressed(*key_event) {
                    self.confirmar_url();
                } else if key_event.kind == KeyEventKind::Press {
                    self.url.input(*key_event);
                    self.set_error(None);
                }
                AccionLocal::Nada
            }
            Vista::ModeloEscrito => {
                if key_event.kind == KeyEventKind::Press && keys::CONFIRM.is_pressed(*key_event) {
                    match self.modelo_input.value() {
                        Some(modelo) => {
                            self.modelo = Some(modelo);
                            self.set_error(None);
                            self.vista = Vista::Llave;
                        }
                        None => {
                            self.set_error(Some("Enter the model id the server serves".to_string()))
                        }
                    }
                } else if key_event.kind == KeyEventKind::Press {
                    self.modelo_input.input(*key_event);
                    self.set_error(None);
                }
                AccionLocal::Nada
            }
            Vista::Llave => {
                if key_event.kind == KeyEventKind::Press && keys::CONFIRM.is_pressed(*key_event) {
                    let rechazo = self.env_key.value().and_then(|var| {
                        eslabon_config::servidor_local::validar_nombre_de_variable(&var).err()
                    });
                    if let Some(mensaje) = rechazo {
                        self.set_error(Some(mensaje));
                    } else {
                        self.set_error(None);
                        self.vista = Vista::Guardando;
                        return AccionLocal::Guardar;
                    }
                } else if key_event.kind == KeyEventKind::Press {
                    self.env_key.input(*key_event);
                    self.set_error(None);
                }
                AccionLocal::Nada
            }
            Vista::Guardando => AccionLocal::Nada,
        }
    }

    pub(super) fn datos(&self) -> Option<DatosLocal> {
        Some(DatosLocal {
            codigo_home: self.setup.codigo_home.clone(),
            base_url: self.base_url.clone()?,
            modelo: self.modelo.clone()?,
            api_key_env: self.env_key.value(),
        })
    }

    pub(super) fn fallar(&mut self, mensaje: String) {
        self.vista = Vista::Llave;
        self.set_error(Some(mensaje));
    }

    pub(super) fn is_text_entry_active(&self) -> bool {
        matches!(
            self.vista,
            Vista::Url | Vista::ModeloEscrito | Vista::Llave
        )
    }

    pub(super) fn active_input_mut(&mut self) -> Option<&mut ProviderInput> {
        match self.vista {
            Vista::Url => Some(&mut self.url),
            Vista::ModeloEscrito => Some(&mut self.modelo_input),
            Vista::Llave => Some(&mut self.env_key),
            Vista::Servidores | Vista::Modelos | Vista::Guardando => None,
        }
    }

    pub(super) fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut lines: Vec<Line> = vec!["> Set up a local server".bold().into(), "".into()];
        match self.vista {
            Vista::Servidores => self.render_servidores(&mut lines),
            Vista::Url => {
                lines.push("  Enter the URL of your local server (chat completions API).".into());
                lines.push("".into());
                lines.push(self.url.line(
                    "  URL: ",
                    "http://127.0.0.1:11434/v1",
                    FieldDisplay::Focused,
                    area.width,
                ));
            }
            Vista::Modelos => {
                lines.push(format!("  {} serves these models. Choose one.", self.nombre).into());
                lines.push("".into());
                for (indice, modelo) in self.modelos.iter().enumerate() {
                    self.render_opcion(&mut lines, indice, modelo, None);
                }
            }
            Vista::ModeloEscrito => {
                lines.push("  Enter the model id the server serves.".into());
                lines.push("".into());
                lines.push(self.modelo_input.line(
                    "  Model: ",
                    "e.g. llama3.2",
                    FieldDisplay::Focused,
                    area.width,
                ));
            }
            Vista::Llave => {
                lines.push(
                    "  Name of the environment variable that holds the API key."
                        .dim()
                        .into(),
                );
                lines.push("".into());
                lines.push(self.env_key.line(
                    "  API key env var: ",
                    "empty = no key",
                    FieldDisplay::Focused,
                    area.width,
                ));
                if let Some(base_url) = &self.base_url {
                    lines.push("".into());
                    lines.push(
                        format!(
                            "  Provider: {base_url} · Model: {}",
                            self.modelo.clone().unwrap_or_default()
                        )
                        .dim()
                        .into(),
                    );
                }
            }
            Vista::Guardando => {
                lines.push(
                    "  Writing config.toml and reloading the session…"
                        .dim()
                        .into(),
                );
            }
        }
        lines.push("".into());
        lines.push(self.footer());
        if let Some(error) = &self.error {
            lines.push("".into());
            lines.push(error.clone().red().into());
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_servidores(&self, lines: &mut Vec<Line<'static>>) {
        let encontrados = self.setup.servidores.len();
        let resumen = match encontrados {
            0 => "  No local server detected.".to_string(),
            1 => "  Found 1 local server. Choose it, or enter its URL.".to_string(),
            n => format!("  Found {n} local servers. Choose one, or enter its URL."),
        };
        lines.push(resumen.into());
        lines.push("".into());
        for (indice, servidor) in self.setup.servidores.iter().enumerate() {
            let modelos = if servidor.modelos.is_empty() {
                "no models reported".to_string()
            } else {
                format!(
                    "{} model{}: {}",
                    servidor.modelos.len(),
                    if servidor.modelos.len() == 1 { "" } else { "s" },
                    servidor.modelos.join(", ")
                )
            };
            self.render_opcion(
                lines,
                indice,
                &format!("{} — {}", servidor.nombre, servidor.base_url),
                Some(&format!("Models: {modelos}")),
            );
        }
        self.render_opcion(
            lines,
            encontrados,
            "Enter a server URL",
            Some("Chat completions API, base URL ending in /v1"),
        );
    }

    /// One list row with an optional dimmed second line, caret when selected.
    fn render_opcion(
        &self,
        lines: &mut Vec<Line<'static>>,
        indice: usize,
        texto: &str,
        detalle: Option<&str>,
    ) {
        let seleccionado = indice == self.elegido;
        let linea = if seleccionado {
            Line::from(format!("> {}. {texto}", indice + 1)).cyan()
        } else {
            Line::from(format!("  {}. {texto}", indice + 1))
        };
        lines.push(linea);
        if let Some(detalle) = detalle {
            let detalle = Line::from(format!("     {detalle}")).style(
                Style::default().add_modifier(Modifier::DIM),
            );
            if seleccionado {
                lines.push(detalle.fg(Color::Cyan).add_modifier(Modifier::DIM));
            } else {
                lines.push(detalle);
            }
        }
    }

    fn footer(&self) -> Line<'static> {
        let (accion, atras) = match self.vista {
            Vista::Servidores => ("choose", "go back"),
            Vista::Modelos => ("choose", "back"),
            Vista::Llave => ("save", "back"),
            Vista::Guardando => return Line::from("  Please wait…".dim()),
            Vista::Url | Vista::ModeloEscrito => ("continue", "back"),
        };
        Line::from(vec![
            "  Press ".dim(),
            keys::CONFIRM[0].into(),
            format!(" to {accion} · ").dim(),
            keys::CANCEL[0].into(),
            format!(" to {atras}").dim(),
        ])
    }
}

pub(super) fn render_local_configured(area: Rect, buf: &mut Buffer) {
    let lines: Vec<Line> = vec![
        "✓ Local server configured".green().into(),
        "".into(),
        "  Eslabon will use this provider in the next session.".dim().into(),
    ];
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(area, buf);
}

impl AuthModeWidget {
    pub(super) fn start_local_setup(&mut self) {
        let Some(setup) = self.eslabon_local.clone() else {
            return;
        };
        self.set_error(/*message*/ None);
        *self
            .sign_in_state
            .write()
            .unwrap_or_else(PoisonError::into_inner) =
            SignInState::LocalSetup(LocalSetupState::new(setup));
        self.request_frame.schedule_frame();
    }

    pub(super) fn handle_local_setup_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let accion = {
            let mut guard = self
                .sign_in_state
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            if let SignInState::LocalSetup(state) = &mut *guard {
                state.handle_key_event(key_event)
            } else {
                return false;
            }
        };
        match accion {
            AccionLocal::Nada => {
                self.request_frame.schedule_frame();
            }
            AccionLocal::Volver => {
                *self
                    .sign_in_state
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = SignInState::PickMode;
                self.highlighted_mode = SignInOption::LocalServer;
                self.set_error(/*message*/ None);
                self.request_frame.schedule_frame();
            }
            AccionLocal::Guardar => {
                let guard = self
                    .sign_in_state
                    .read()
                    .unwrap_or_else(PoisonError::into_inner);
                let datos = match &*guard {
                    SignInState::LocalSetup(state) => state.datos(),
                    _ => None,
                };
                drop(guard);
                if let Some(datos) = datos {
                    self.spawn_local_write(datos);
                }
            }
        }
        true
    }

    pub(super) fn handle_local_setup_paste(&mut self, pasted: &str) -> bool {
        let mut guard = self
            .sign_in_state
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(state) = (match &mut *guard {
            SignInState::LocalSetup(state) => Some(state),
            _ => None,
        }) else {
            return false;
        };
        if let Some(input) = state.active_input_mut() {
            input.paste(pasted);
            state.set_error(None);
        } else {
            return false;
        }
        drop(guard);
        self.request_frame.schedule_frame();
        true
    }

    fn spawn_local_write(&self, datos: DatosLocal) {
        let sign_in_state = self.sign_in_state.clone();
        let request_frame = self.request_frame.clone();
        let request_handle = self.app_server_request_handle.clone();
        tokio::spawn(async move {
            let resultado = escribir_y_recargar(request_handle, datos).await;
            let mut state = sign_in_state
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            match resultado {
                Ok(()) => *state = SignInState::LocalConfigured,
                Err(mensaje) => {
                    if let SignInState::LocalSetup(setup) = &mut *state {
                        setup.fallar(mensaje);
                    }
                }
            }
            drop(state);
            request_frame.schedule_frame();
        });
    }
}

/// Writes the config with `eslabon init`'s writer, then asks the running app
/// server to reload its user config so the session can start without a restart.
async fn escribir_y_recargar(
    request_handle: AppServerRequestHandle,
    datos: DatosLocal,
) -> Result<(), String> {
    let codigo_home = datos.codigo_home.clone();
    // eslabon: startup writes its own `[tui]` marker before this wizard opens;
    // replacing that with a `.bak` copy is safe, but a config with real
    // settings is left alone.
    if !eslabon_config::servidor_local::config_solo_ajustes_de_arranque(&codigo_home)
        .map_err(|error| format!("Could not read config.toml: {error}"))?
    {
        return Err(
            "config.toml already has settings: add the provider by hand or run `eslabon init --force`"
                .to_string(),
        );
    }
    tokio::task::spawn_blocking(move || {
        // eslabon: the wizard keeps the startup `[tui]` table and writes no `.bak`.
        eslabon_config::servidor_local::escribir_sobre_ajustes_de_arranque(
            &codigo_home,
            &datos.base_url,
            &datos.modelo,
            datos.api_key_env.as_deref(),
            None,
        )
        .map(|_| ())
    })
    .await
    .map_err(|error| format!("Could not write config.toml: {error}"))?
    .map_err(|error| format!("Could not write config.toml: {error}"))?;
    let request_id = RequestId::String(format!("tui-eslabon-local-write-{}", Uuid::new_v4()));
    request_handle
        .request_typed::<ConfigWriteResponse>(ClientRequest::ConfigBatchWrite {
            request_id,
            params: ConfigBatchWriteParams {
                edits: Vec::new(),
                file_path: None,
                expected_version: None,
                reload_user_config: true,
            },
        })
        .await
        .map_err(|error| {
            format!("config.toml was written but the session could not reload it: {error}")
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;
    use eslabon_config::detector::ServidorLocal;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::TempDir;

    fn servidor(nombre: &str, puerto: u16, modelos: &[&str]) -> ServidorLocal {
        ServidorLocal {
            nombre: nombre.to_string(),
            base_url: format!("http://127.0.0.1:{puerto}/v1"),
            modelos: modelos.iter().map(|modelo| modelo.to_string()).collect(),
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn elegir_un_servidor_escribe_el_config_esperado() {
        let codigo_home = TempDir::new().expect("temp home");
        let mut state = LocalSetupState::new(EslabonLocalSetup {
            codigo_home: codigo_home.path().to_path_buf(),
            servidores: vec![servidor("Ollama", 11434, &["humo-1"])],
        });
        // Server 1 has one model, so Enter lands directly on the optional key field.
        state.handle_key_event(&press(KeyCode::Enter));
        assert_eq!(state.vista, Vista::Llave);
        state.handle_key_event(&press(KeyCode::Enter));
        assert_eq!(state.vista, Vista::Guardando);
        let datos = state.datos().expect("datos completos");
        let (destino, texto, _) = eslabon_config::servidor_local::escribir(
            &datos.codigo_home,
            &datos.base_url,
            &datos.modelo,
            datos.api_key_env.as_deref(),
            None,
            /*force*/ false,
        )
        .expect("escribe");
        assert_eq!(destino, codigo_home.path().join("config.toml"));
        let tabla: toml::Table = texto.parse().expect("TOML");
        assert_eq!(tabla["model"].as_str(), Some("humo-1"));
        assert_eq!(
            tabla["model_providers"]["local"]["stream_idle_timeout_ms"].as_integer(),
            Some(900_000)
        );
        // eslabon: only the variable name is written, never the key itself.
        assert_eq!(
            fs::read_to_string(&destino).expect("config en disco"),
            texto
        );
    }

    #[test]
    fn esc_vuelve_del_wizard_al_picker() {
        let codigo_home = TempDir::new().expect("temp home");
        let mut state = LocalSetupState::new(EslabonLocalSetup {
            codigo_home: codigo_home.path().to_path_buf(),
            servidores: vec![servidor("Ollama", 11434, &["a", "b"])],
        });
        assert!(matches!(
            state.handle_key_event(&press(KeyCode::Esc)),
            AccionLocal::Volver
        ));
    }

    #[test]
    fn elegir_la_fila_url_pide_la_url() {
        let codigo_home = TempDir::new().expect("temp home");
        let mut state = LocalSetupState::new(EslabonLocalSetup {
            codigo_home: codigo_home.path().to_path_buf(),
            servidores: vec![servidor("Ollama", 11434, &["a"])],
        });
        state.handle_key_event(&press(KeyCode::Down));
        state.handle_key_event(&press(KeyCode::Enter));
        assert_eq!(state.vista, Vista::Url);
    }
}
