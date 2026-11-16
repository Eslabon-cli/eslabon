//! eslabon: sidebar snapshot and the actions behind its clickable rows.

use super::ChatWidget;
use super::mcp_startup::McpStartupStatus;
use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::eslabon_sidebar::ContextUsage;
use crate::eslabon_sidebar::McpRow;
use crate::eslabon_sidebar::McpState;
use crate::eslabon_sidebar::SidebarEvent;
use crate::eslabon_sidebar::SidebarHits;
use crate::eslabon_sidebar::SidebarSnapshot;
use crate::eslabon_sidebar::SidebarTarget;
use crate::eslabon_sidebar::hit_test;
use crate::slash_command::SlashCommand;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItemPayload;

fn send(event: SidebarEvent) -> crate::bottom_pane::SelectionAction {
    Box::new(move |tx| tx.send(AppEvent::Eslabon(event.clone())))
}

fn item(name: &str, description: &str, event: SidebarEvent) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions: vec![send(event)],
        dismiss_on_select: true,
        ..Default::default()
    }
}

impl ChatWidget {
    pub(crate) fn eslabon_sidebar_snapshot(&self) -> SidebarSnapshot {
        let context = self.token_info.as_ref().map(|info| ContextUsage {
            used: info.last_token_usage.tokens_in_context_window(),
            window: info.model_context_window,
            last_input: info.last_token_usage.input_tokens,
            last_cached: info.last_token_usage.cached_input_tokens,
            total_input: info.total_token_usage.input_tokens,
            total_cached: info.total_token_usage.cached_input_tokens,
            total_output: info.total_token_usage.output_tokens,
        });

        // Servers seen in status updates, plus configured ones that have not reported yet.
        let mut mcp: Vec<McpRow> = self
            .eslabon_mcp_states
            .iter()
            .map(|(name, status)| McpRow {
                name: name.clone(),
                state: match status {
                    McpStartupStatus::Starting => McpState::Starting,
                    McpStartupStatus::Ready => McpState::Ready,
                    McpStartupStatus::Failed { error, .. } => McpState::Failed(error.clone()),
                    McpStartupStatus::Cancelled => McpState::Cancelled,
                },
            })
            .collect();
        if let Some(expected) = &self.mcp_startup_expected_servers {
            for name in expected {
                if !self.eslabon_mcp_states.contains_key(name) {
                    mcp.push(McpRow {
                        name: name.clone(),
                        state: McpState::Pending,
                    });
                }
            }
        }
        mcp.sort_by(|a, b| a.name.cmp(&b.name));

        SidebarSnapshot {
            model: self.model_display_name().to_string(),
            provider: self.config.model_provider_id.clone(),
            effort: self
                .current_reasoning_effort()
                .map(|effort| effort.to_string()),
            cwd: self.config.cwd.display().to_string(),
            branch: self.status_line_branch.clone(),
            context,
            mcp,
            // Shown when the profile lists PHI providers or the block is on right now.
            phi: (!self.config.eslabon_phi_allowed_base_urls.is_empty()
                || codex_api::eslabon_policy::phi_policy_active())
            .then(codex_api::eslabon_policy::phi_policy_active),
            mcp_collapsed: self.eslabon_mcp_collapsed,
            working: self.bottom_pane.is_task_running(),
            speed: self
                .eslabon_speed
                .view(std::time::Instant::now(), self.bottom_pane.is_task_running()),
            server: crate::eslabon_speed::server_stats(
                self.config.model_provider.base_url.as_deref(),
                self.bottom_pane.is_task_running(),
                &self.frame_requester,
            ),
        }
    }

    /// Feeds the sidebar's speed meter from live (not replayed) notifications.
    pub(crate) fn eslabon_observe_speed(&mut self, notification: &ServerNotification) {
        let now = std::time::Instant::now();
        let meter = &mut self.eslabon_speed;
        match notification {
            ServerNotification::TurnStarted(_) => meter.turn_started(now),
            ServerNotification::TurnCompleted(_) => meter.turn_finished(),
            ServerNotification::ItemCompleted(n)
                if !matches!(
                    n.item.payload,
                    ThreadItemPayload::UserMessage { .. }
                        | ThreadItemPayload::HookPrompt { .. }
                        | ThreadItemPayload::AgentMessage { .. }
                        | ThreadItemPayload::Plan { .. }
                        | ThreadItemPayload::Reasoning { .. }
                ) =>
            {
                meter.tool_completed(now)
            }
            ServerNotification::AgentMessageDelta(n) => meter.output(now, n.delta.chars().count()),
            ServerNotification::ReasoningTextDelta(n) => meter.output(now, n.delta.chars().count()),
            ServerNotification::ReasoningSummaryTextDelta(n) => {
                meter.output(now, n.delta.chars().count())
            }
            ServerNotification::PlanDelta(n) => meter.output(now, n.delta.chars().count()),
            ServerNotification::ThreadTokenUsageUpdated(n) => {
                let last = &n.token_usage.last;
                meter.reply_finished(
                    now,
                    n.token_usage.total.total_tokens,
                    last.input_tokens,
                    last.cached_input_tokens,
                    last.output_tokens,
                );
            }
            _ => {}
        }
    }

    pub(crate) fn set_eslabon_sidebar_hits(&mut self, hits: SidebarHits) {
        self.eslabon_sidebar_hits = hits;
    }

    /// Handles a press on the sidebar. Returns `true` when a row was hit.
    pub(crate) fn eslabon_sidebar_click(&mut self, column: u16, row: u16) -> bool {
        let Some(target) = hit_test(&self.eslabon_sidebar_hits, column, row).cloned() else {
            return false;
        };
        match target {
            SidebarTarget::Model => self.dispatch_command(SlashCommand::Model),
            SidebarTarget::Context => self.open_eslabon_context_menu(),
            SidebarTarget::McpHeader => {
                self.eslabon_mcp_collapsed = !self.eslabon_mcp_collapsed;
                // Rows move on the next frame; drop the stale targets until then.
                self.eslabon_sidebar_hits.clear();
                self.request_redraw();
            }
            SidebarTarget::McpServer(name) => self.open_eslabon_mcp_menu(&name),
            SidebarTarget::Cwd => self.open_eslabon_cwd_menu(),
            SidebarTarget::Phi => self.open_eslabon_phi_menu(),
        }
        true
    }

    /// Claims the single in-flight MCP reconnect. Returns `false` if one is running.
    pub(crate) fn eslabon_begin_mcp_reload(&mut self) -> bool {
        !std::mem::replace(&mut self.eslabon_mcp_reload_in_flight, true)
    }

    pub(crate) fn eslabon_end_mcp_reload(&mut self) {
        self.eslabon_mcp_reload_in_flight = false;
    }

    /// Runs a slash command requested from a sidebar menu.
    pub(crate) fn eslabon_dispatch(&mut self, command: SlashCommand) {
        self.dispatch_command(command);
    }

    fn open_eslabon_context_menu(&mut self) {
        self.show_selection_view(SelectionViewParams {
            title: Some("Context".to_string()),
            items: vec![
                item(
                    "Compact now",
                    "Summarize the conversation to free context",
                    SidebarEvent::Slash(SlashCommand::Compact),
                ),
                item(
                    "Show status",
                    "Model, limits and token usage",
                    SidebarEvent::Slash(SlashCommand::Status),
                ),
            ],
            ..SelectionViewParams::picker()
        });
    }

    fn open_eslabon_mcp_menu(&mut self, name: &str) {
        let status = self.eslabon_mcp_states.get(name).cloned();
        let subtitle = match &status {
            Some(McpStartupStatus::Ready) => "connected",
            Some(McpStartupStatus::Starting) => "starting",
            None => "pending",
            Some(McpStartupStatus::Failed { .. }) => "failed",
            Some(McpStartupStatus::Cancelled) => "cancelled",
        };
        let mut items = vec![item(
            "Reconnect",
            "Restart failed or disconnected servers; healthy ones keep running",
            SidebarEvent::ReloadMcp,
        )];
        // Remote servers may need an OAuth login (e.g. "Not logged in"); offering it
        // always is harmless for servers that don't use OAuth.
        if let Some(thread_id) = self.thread_id {
            let server = name.to_string();
            items.push(SelectionItem {
                name: "Sign in".to_string(),
                description: Some("Start the OAuth sign-in (servers that use OAuth)".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::StartMcpLogin {
                        name: server.clone(),
                        thread_id,
                    })
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }
        if let Some(McpStartupStatus::Failed { error, .. }) = &status {
            items.push(item(
                "Show full error",
                "Print the startup error in the transcript",
                SidebarEvent::ShowError(format!("MCP `{name}`: {error}")),
            ));
        }
        items.push(item(
            "Show tools",
            "List MCP servers, their tools and auth state",
            SidebarEvent::Slash(SlashCommand::Mcp),
        ));
        self.show_selection_view(SelectionViewParams {
            title: Some(format!("MCP server: {name}")),
            subtitle: Some(subtitle.to_string()),
            items,
            ..SelectionViewParams::picker()
        });
    }

    fn open_eslabon_phi_menu(&mut self) {
        let on = codex_api::eslabon_policy::phi_policy_active();
        let (name, description) = if on {
            (
                "Turn data lock off",
                "Only for sessions without protected data; applies to every eslabon session",
            )
        } else {
            (
                "Turn data lock on",
                "Model requests may only reach the providers in [eslabon] phi_providers",
            )
        };
        self.show_selection_view(SelectionViewParams {
            title: Some("Data lock".to_string()),
            subtitle: Some(if on { "on" } else { "off" }.to_string()),
            items: vec![item(name, description, SidebarEvent::TogglePhi)],
            ..SelectionViewParams::picker()
        });
    }

    /// Flips the PHI switch file; returns the message to show.
    pub(crate) fn eslabon_toggle_phi(&mut self) -> Result<String, String> {
        let path = codex_api::eslabon_policy::switch_path(self.config.codex_home.as_path());
        let turn_on = !codex_api::eslabon_policy::phi_policy_active();
        codex_api::eslabon_policy::write_switch(
            &path,
            &codex_api::eslabon_policy::PhiSwitch {
                enabled: turn_on,
                allowed_base_urls: if turn_on {
                    self.config.eslabon_phi_allowed_base_urls.clone()
                } else {
                    Vec::new()
                },
            },
        )?;
        self.request_redraw();
        Ok(if turn_on {
            "Data lock ON: model requests may only reach [eslabon] phi_providers.".to_string()
        } else {
            "Data lock OFF for every eslabon session. Do not use it with protected data."
                .to_string()
        })
    }

    fn open_eslabon_cwd_menu(&mut self) {
        let path = self.config.cwd.display().to_string();
        self.show_selection_view(SelectionViewParams {
            title: Some("Working directory".to_string()),
            subtitle: Some(path.clone()),
            items: vec![
                item(
                    "Copy path",
                    "Copy to the clipboard",
                    SidebarEvent::CopyText(path.clone()),
                ),
                item(
                    "Open folder",
                    "Open in the system file manager",
                    SidebarEvent::OpenPath(path),
                ),
            ],
            ..SelectionViewParams::picker()
        });
    }
}
