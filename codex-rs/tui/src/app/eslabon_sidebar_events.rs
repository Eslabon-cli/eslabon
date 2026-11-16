//! eslabon: app-level handling of sidebar menu actions.

use super::*;
use crate::eslabon_sidebar::SidebarEvent;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::McpServerRefreshResponse;
use codex_app_server_protocol::McpServerStatusDetail;
use codex_app_server_protocol::RequestId;

const RELOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Opens a directory in the system file manager without going through a shell.
fn open_folder(path: &str) -> std::io::Result<()> {
    let path = std::path::Path::new(path);
    if !path.is_absolute() || !path.is_dir() {
        return Err(std::io::Error::other("not an existing absolute directory"));
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

impl App {
    /// eslabon: continue the conversation on another provider's model by forking the
    /// thread with that provider; the history travels with it.
    pub(super) async fn eslabon_switch_provider(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        provider: String,
        model: String,
    ) -> Result<()> {
        let Some(thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Start the conversation first, then switch providers with /model.".to_string(),
            );
            return Ok(());
        };
        let screen_size = tui.terminal.last_known_screen_size;
        self.handle_draw_pre_render(tui, screen_size)?;
        self.render_chat_widget_frame(tui, screen_size)?;
        let mut fork_config = self.config.clone();
        fork_config.model_provider_id = provider.clone();
        fork_config.model = Some(model.clone());
        fork_config.model_reasoning_effort = None;
        match app_server
            .fork_thread_at(
                &self.local_settings,
                fork_config,
                thread_id,
                /*last_turn_id*/ None,
                /*before_turn_id*/ None,
                crate::app_server_session::ForkGoalContinuation::StartIfIdle,
                /*selected_profile*/ None,
            )
            .await
        {
            Ok(forked) => {
                let new_thread = forked.session.thread_id;
                self.detach_current_thread_for_navigation(app_server, Some(new_thread))
                    .await;
                match self
                    .replace_chat_widget_with_app_server_thread(
                        tui,
                        forked,
                        super::session_lifecycle::ThreadAttachPresentation::SessionLineage,
                        /*initial_user_message*/ None,
                    )
                    .await
                {
                    Ok(()) => self.chat_widget.add_info_message(
                        format!(
                            "Switched to {model} on {provider}; the conversation continues here."
                        ),
                        /*hint*/ None,
                    ),
                    Err(err) => self.chat_widget.add_error_message(format!(
                        "Switched provider but could not attach to the new thread: {err}"
                    )),
                }
            }
            Err(err) => {
                // A thread without a rollout on disk cannot be forked, so start a fresh session instead.
                if err.to_string().contains("no rollout found") {
                    self.harness_overrides.model = Some(model.clone());
                    self.harness_overrides.model_provider = Some(provider.clone());
                    // thread/start only sends a provider that comes from a config layer;
                    // the session override is what carries it (as at startup and reconnect).
                    app_server.model_provider_override = Some(provider.clone());
                    self.start_fresh_session(
                        tui, app_server, /*session_start_source*/ None,
                        /*initial_user_message*/ None, /*new_thread_name*/ None,
                    )
                    .await;
                    self.chat_widget.add_info_message(
                        format!("Started a new session with {model} on {provider} (the previous one had no saved turns yet)."),
                        /*hint*/ None,
                    );
                } else {
                    self.chat_widget.add_error_message(format!(
                        "Could not switch to {model} on {provider}: {err}"
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_eslabon_sidebar_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: SidebarEvent,
    ) {
        match event {
            SidebarEvent::ReloadMcp => {
                if !self.chat_widget.eslabon_begin_mcp_reload() {
                    self.chat_widget.add_info_message(
                        "MCP reconnect already in progress.".to_string(),
                        /*hint*/ None,
                    );
                    return;
                }
                self.chat_widget
                    .add_info_message("Reconnecting MCP servers…".to_string(), /*hint*/ None);
                let request_handle = app_server.request_handle();
                let app_event_tx = self.app_event_tx.clone();
                let thread_id = self.chat_widget.thread_id();
                tokio::spawn(async move {
                    let work = async {
                        let request_id = RequestId::String(format!(
                            "eslabon-mcp-reload-{}",
                            uuid::Uuid::new_v4()
                        ));
                        let _: McpServerRefreshResponse = request_handle
                            .request_typed(ClientRequest::McpServerRefresh {
                                request_id,
                                params: None,
                            })
                            .await
                            .map_err(|err| format!("reload failed: {err}"))?;
                        // A reload starts servers with a cached tool catalog lazily, so a
                        // reconnected server would sit in "starting" until first use.
                        // Listing the inventory (what `/mcp` does) brings them up now.
                        let statuses = super::background_requests::fetch_all_mcp_server_statuses(
                            request_handle,
                            McpServerStatusDetail::ToolsAndAuthOnly,
                            thread_id,
                        )
                        .await
                        .map_err(|err| format!("reloaded, but servers did not start: {err}"))?;
                        Ok::<Vec<String>, String>(statuses
                            .into_iter()
                            .map(|status| {
                                use codex_app_server_protocol::McpAuthStatus;
                                let name = status.name;
                                if matches!(status.auth_status, McpAuthStatus::NotLoggedIn) {
                                    format!("{name}: not logged in — click it in the sidebar → Sign in")
                                } else if let Some(error) = status.tools_error {
                                    format!("{name}: {error}")
                                } else {
                                    let tools = status.tools.len();
                                    format!("{name}: connected ({tools} tool{})", if tools == 1 { "" } else { "s" })
                                }
                            })
                            .collect())
                    };
                    let result = tokio::time::timeout(RELOAD_TIMEOUT, work)
                        .await
                        .unwrap_or_else(|_| {
                            Err(format!("timed out after {}s", RELOAD_TIMEOUT.as_secs()))
                        });
                    app_event_tx.send(AppEvent::Eslabon(SidebarEvent::ReloadMcpFinished(result)));
                });
            }
            SidebarEvent::ReloadMcpFinished(result) => {
                self.chat_widget.eslabon_end_mcp_reload();
                match result {
                    Ok(lines) => self.chat_widget.add_info_message(
                        format!("MCP servers after reconnect:\n{}", lines.join("\n")),
                        /*hint*/ None,
                    ),
                    Err(err) => self
                        .chat_widget
                        .add_error_message(format!("Could not reconnect MCP servers: {err}")),
                }
            }
            SidebarEvent::CopyText(text) => {
                let result = tui
                    .copy_transcript_selection(&text, crate::clipboard_copy::CopyFormat::PlainText);
                self.chat_widget.show_selection_copy_result(result);
            }
            SidebarEvent::OpenPath(path) => {
                if let Err(err) = open_folder(&path) {
                    self.chat_widget
                        .add_error_message(format!("Could not open {path}: {err}"));
                }
            }
            SidebarEvent::Slash(command) => self.chat_widget.eslabon_dispatch(command),
            SidebarEvent::TogglePhi => match self.chat_widget.eslabon_toggle_phi() {
                Ok(message) => self.chat_widget.add_info_message(message, /*hint*/ None),
                Err(err) => self.chat_widget.add_error_message(err),
            },
            SidebarEvent::ShowError(message) => self.chat_widget.add_error_message(message),
        }
    }
}
