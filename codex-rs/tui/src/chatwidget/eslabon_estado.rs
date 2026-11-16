//! eslabon: session state visible from outside the terminal.
//!
//! Each session keeps `$CODEX_HOME/estado/<thread>.json` with `trabajando`,
//! `esperando_aprobacion` or `inactiva` and the time it entered that state, so
//! scripts, herdr or another session can tell "waiting on a human" from "hung".
//! On entering `esperando_aprobacion` the `[eslabon] notify_aprobacion` command runs.

use super::ChatWidget;
use serde_json::json;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EstadoSesion {
    Trabajando,
    EsperandoAprobacion,
    Inactiva,
}

impl EstadoSesion {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Trabajando => "trabajando",
            Self::EsperandoAprobacion => "esperando_aprobacion",
            Self::Inactiva => "inactiva",
        }
    }
}

/// The state a session is in: an approval on screen wins over a running turn.
pub(crate) fn estado_actual(
    approval_pending: bool,
    approval_view_open: bool,
    task_running: bool,
) -> EstadoSesion {
    if approval_pending && approval_view_open {
        EstadoSesion::EsperandoAprobacion
    } else if task_running {
        EstadoSesion::Trabajando
    } else {
        EstadoSesion::Inactiva
    }
}

impl ChatWidget {
    /// Called for every approval prompt (command, edit, MCP elicitation).
    pub(super) fn eslabon_mark_approval(&mut self, detail: String) {
        self.eslabon_approval_detail = Some(detail);
    }

    /// Writes the state file when the state changed since the last call.
    pub(crate) fn eslabon_publish_state(&mut self) {
        if !crate::eslabon_stream::enabled() {
            return;
        }
        let approval_view_open = self.bottom_pane.has_active_view();
        if !approval_view_open {
            // The approval was answered (or dismissed): back to the turn.
            self.eslabon_approval_detail = None;
        }
        let estado = estado_actual(
            self.eslabon_approval_detail.is_some(),
            approval_view_open,
            self.bottom_pane.is_task_running(),
        );
        // A new approval queued behind the first one counts as a change too.
        let published = (estado, self.eslabon_approval_detail.clone());
        if self.eslabon_published_state.as_ref() == Some(&published) {
            return;
        }
        let Some(thread_id) = self.thread_id() else {
            return;
        };
        self.eslabon_published_state = Some(published);
        let desde = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or_default();
        let record = json!({
            "estado": estado.as_str(),
            "desde": desde,
            "hilo": thread_id.to_string(),
            "pid": std::process::id(),
            "cwd": self.config.cwd.to_path_buf(),
            "modelo": self.model_display_name(),
            "detalle": self.eslabon_approval_detail,
        });
        let dir = self.config.codex_home.join("estado");
        let path = dir.join(format!("{thread_id}.json"));
        // Per process: two sessions resumed on the same thread never share a temp file.
        let tmp = dir.join(format!("{thread_id}.{}.json.tmp", std::process::id()));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&tmp, record.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(err) = written {
            tracing::warn!("eslabon: could not write session state {}: {err}", path.display());
        }
        if estado == EstadoSesion::EsperandoAprobacion
            && let Some(command) = self.config.eslabon_notify_aprobacion.as_ref()
            && let Some((program, args)) = command.split_first()
        {
            let payload = json!({
                "type": "approval-requested",
                "hilo": thread_id.to_string(),
                "cwd": self.config.cwd.to_path_buf(),
                "detalle": self.eslabon_approval_detail,
            });
            let spawned = std::process::Command::new(program)
                .args(args)
                .arg(payload.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            match spawned {
                // Reap the child so repeated approvals do not leave zombies behind.
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(err) => {
                    tracing::warn!("eslabon: notify_aprobacion `{program}` failed to start: {err}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn an_open_approval_wins_over_a_running_turn() {
        assert_eq!(
            estado_actual(true, true, true),
            EstadoSesion::EsperandoAprobacion
        );
        // The prompt was answered: the turn continues.
        assert_eq!(estado_actual(true, false, true), EstadoSesion::Trabajando);
        // Another popup without a pending approval is not "waiting".
        assert_eq!(estado_actual(false, true, true), EstadoSesion::Trabajando);
        assert_eq!(estado_actual(false, false, false), EstadoSesion::Inactiva);
    }
}
