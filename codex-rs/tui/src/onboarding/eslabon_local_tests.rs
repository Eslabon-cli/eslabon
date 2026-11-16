#![cfg(test)]

//! eslabon: onboarding coverage for the local-server wizard.

use super::*;
use crate::onboarding::eslabon_local::EslabonLocalSetup;
use crate::onboarding::eslabon_local::LocalSetupState;
use crossterm::event::KeyCode;
use crossterm::event::KeyModifiers;
use eslabon_config::detector::ServidorLocal;
use pretty_assertions::assert_eq;
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

async fn widget_with_servers(servidores: Vec<ServidorLocal>) -> (AuthModeWidget, TempDir) {
    let (mut widget, home) = super::tests::widget_forced_chatgpt().await;
    widget.auth_config.forced_login_method = None;
    let setup = EslabonLocalSetup {
        codigo_home: home.path().to_path_buf(),
        servidores,
    };
    widget.eslabon_local = Some(setup.clone());
    widget.highlighted_mode = SignInOption::LocalServer;
    *widget.sign_in_state.write().unwrap() = SignInState::LocalSetup(LocalSetupState::new(setup));
    (widget, home)
}

fn snapshot(widget: &AuthModeWidget, name: &str, width: u16, height: u16) {
    let area = Rect::new(0, 0, width, height);
    let mut terminal = crate::custom_terminal::Terminal::with_options(
        crate::test_backend::VT100Backend::new(width, height),
    )
    .expect("terminal");
    terminal.set_viewport_area(area);
    terminal
        .draw(|frame| widget.render_ref(area, frame.buffer_mut()))
        .expect("draw");
    insta::assert_snapshot!(name, terminal.backend().to_string());
}

#[tokio::test]
async fn wizard_renders_zero_one_and_two_detected_servers() {
    let (widget, _home) = widget_with_servers(Vec::new()).await;
    snapshot(&widget, "eslabon_local_no_servers", 80, 24);

    let (widget, _home) = widget_with_servers(vec![servidor(
        "Ollama",
        11434,
        &["qwen3:8b", "llama3.2"],
    )])
    .await;
    snapshot(&widget, "eslabon_local_one_server", 80, 24);

    let (widget, _home) = widget_with_servers(vec![
        servidor("Ollama", 11434, &["qwen3:8b"]),
        servidor("LM Studio", 1234, &["local-model"]),
    ])
    .await;
    snapshot(&widget, "eslabon_local_two_servers", 80, 24);
}

#[tokio::test]
async fn picker_offers_local_first_and_upstream_when_disabled() {
    let (mut widget, _home) = widget_with_servers(vec![servidor("Ollama", 11434, &["humo-1"])]).await;
    // eslabon: the local option is the first one the picker shows.
    assert_eq!(
        widget.displayed_sign_in_options().first(),
        Some(&SignInOption::LocalServer)
    );
    assert_eq!(
        widget.displayed_sign_in_options()[1],
        SignInOption::ChatGpt
    );
    *widget.sign_in_state.write().unwrap() = SignInState::PickMode;
    widget.highlighted_mode = SignInOption::LocalServer;
    snapshot(&widget, "eslabon_local_picker_local_first", 80, 24);

    // eslabon: without the setup the picker is exactly upstream's.
    widget.eslabon_local = None;
    widget.highlighted_mode = SignInOption::ChatGpt;
    assert!(
        !widget
            .displayed_sign_in_options()
            .contains(&SignInOption::LocalServer)
    );
    snapshot(&widget, "eslabon_local_picker_upstream", 80, 24);
}

#[tokio::test]
async fn esc_in_wizard_returns_to_the_picker() {
    let (mut widget, _home) = widget_with_servers(vec![servidor("Ollama", 11434, &["humo-1"])]).await;
    widget.handle_key_event(press(KeyCode::Esc));
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::PickMode
    ));
    assert_eq!(widget.highlighted_mode, SignInOption::LocalServer);
    assert_eq!(widget.exit_message, None);
}

#[tokio::test]
async fn esc_in_picker_exits_with_the_hint() {
    let (mut widget, _home) = widget_with_servers(vec![servidor("Ollama", 11434, &["humo-1"])]).await;
    *widget.sign_in_state.write().unwrap() = SignInState::PickMode;
    widget.handle_key_event(press(KeyCode::Esc));
    assert_eq!(
        widget.exit_message.as_deref(),
        Some("Set up a provider with `eslabon init` or edit config.toml.")
    );
    snapshot(&widget, "eslabon_local_picker_esc_hint", 80, 24);
}

#[tokio::test]
async fn choosing_a_server_writes_the_config_in_a_temp_home() {
    let (mut widget, home) =
        widget_with_servers(vec![servidor("Ollama", 11434, &["humo-1"])]).await;
    widget.handle_key_event(press(KeyCode::Enter));
    widget.handle_key_event(press(KeyCode::Enter));
    let destino = home.path().join("config.toml");
    for _ in 0..100 {
        if destino.exists()
            && matches!(&*widget.sign_in_state.read().unwrap(), SignInState::LocalConfigured)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let texto = std::fs::read_to_string(&destino).expect("config.toml escrito");
    assert!(texto.contains("model = \"humo-1\""), "{texto}");
    assert!(texto.contains("model_provider = \"local\""), "{texto}");
    assert!(texto.contains("stream_idle_timeout_ms = 900000"), "{texto}");
    assert!(matches!(
        &*widget.sign_in_state.read().unwrap(),
        SignInState::LocalConfigured
    ));
}
