//! eslabon: opencode-style compact rows for tool activity in the owned transcript.
//!
//! One row per action with an icon by kind (opencode's vocabulary): `→` read,
//! `✱` search, `$` command, `←` edit. Successful output stays collapsed behind the
//! existing disclosure (⌃t / click); failures keep their last lines visible.
//! Set `ESLABON_STREAM=codex` to get upstream's presentation back.

use codex_protocol::parse_command::ParsedCommand;
use std::sync::OnceLock;

pub(crate) const ICON_READ: &str = "→";
pub(crate) const ICON_SEARCH: &str = "✱";
pub(crate) const ICON_COMMAND: &str = "$";
pub(crate) const ICON_EDIT: &str = "←";
pub(crate) const ICON_OK: &str = "✓";
pub(crate) const ICON_FAIL: &str = "✗";
/// Output lines kept visible under a failed command.
pub(crate) const FAILURE_TAIL_LINES: usize = 3;
/// Calls listed under an exploration summary before collapsing into "+N more".
pub(crate) const EXPLORATION_ROWS: usize = 4;

/// Library default for eslabon presentations: off, so upstream's integration tests and
/// other embedders keep Codex's look. The `codex` binary turns it on at startup.
static DEFAULTS_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turns eslabon's presentation (sidebar, opencode-style rows) on by default.
/// `ESLABON_SIDEBAR` / `ESLABON_STREAM` still override it either way.
pub fn enable_eslabon_defaults() {
    DEFAULTS_ON.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn env_flag(name: &'static str, cache: &'static OnceLock<Option<bool>>) -> Option<bool> {
    *cache.get_or_init(|| {
        std::env::var(name).ok().map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "codex" | "0" | "off" | "false" | "no" | "disabled"
            )
        })
    })
}

fn resolve(name: &'static str, cache: &'static OnceLock<Option<bool>>) -> bool {
    #[cfg(test)]
    {
        let _ = (name, cache);
        return test_override::get();
    }
    #[allow(unreachable_code)]
    env_flag(name, cache).unwrap_or_else(|| DEFAULTS_ON.load(std::sync::atomic::Ordering::Relaxed))
}

/// Provider of a model according to the merged catalog index written by
/// `codex catalog` (`$CODEX_HOME/catalogs/providers-index.json`).
pub(crate) fn provider_for_model(codex_home: &std::path::Path, slug: &str) -> Option<String> {
    let text = std::fs::read_to_string(codex_home.join("catalogs/providers-index.json")).ok()?;
    let index: std::collections::HashMap<String, String> = serde_json::from_str(&text).ok()?;
    index.get(slug).cloned()
}

/// Whether the eslabon identity is active (eslabon binary, launcher, or a test guard).
pub(crate) fn branded() -> bool {
    #[cfg(test)]
    let branded = test_override::get();
    #[cfg(not(test))]
    let branded = DEFAULTS_ON.load(std::sync::atomic::Ordering::Relaxed);
    branded
}

/// Product name in headers: "eslabon" in the eslabon binary, upstream's name otherwise.
pub(crate) fn product_name() -> &'static str {
    if branded() {
        codex_config::eslabon::marca::NOMBRE_BINARIO
    } else {
        codex_config::eslabon::marca::NOMBRE_PRODUCTO_UPSTREAM
    }
}

/// Name shown in the terminal window title; upstream uses the lowercase `codex`.
pub(crate) fn app_name() -> &'static str {
    if branded() {
        codex_config::eslabon::marca::NOMBRE_BINARIO
    } else {
        "codex"
    }
}

/// Label of the terminal-title item name in the settings popup.
pub(crate) fn app_name_label() -> &'static str {
    if branded() {
        codex_config::eslabon::marca::ETIQUETA_NOMBRE_APP
    } else {
        codex_config::eslabon::marca::ETIQUETA_NOMBRE_APP_UPSTREAM
    }
}

/// Label of the session-title item that shows the application version.
pub(crate) fn version_label() -> &'static str {
    if branded() {
        codex_config::eslabon::marca::ETIQUETA_VERSION_APP
    } else {
        codex_config::eslabon::marca::ETIQUETA_VERSION_APP_UPSTREAM
    }
}

/// Composer placeholder on the first screen.
pub(crate) fn ask_placeholder() -> &'static str {
    if branded() {
        codex_config::eslabon::marca::PLACEHOLDER_PREGUNTAR
    } else {
        codex_config::eslabon::marca::PLACEHOLDER_PREGUNTAR_UPSTREAM
    }
}

/// Whether the opencode-style rows are active (`ESLABON_STREAM`).
pub(crate) fn enabled() -> bool {
    static CACHE: OnceLock<Option<bool>> = OnceLock::new();
    resolve("ESLABON_STREAM", &CACHE)
}

/// Whether the right-hand sidebar is active (`ESLABON_SIDEBAR`).
pub(crate) fn sidebar_enabled() -> bool {
    static CACHE: OnceLock<Option<bool>> = OnceLock::new();
    resolve("ESLABON_SIDEBAR", &CACHE)
}

#[cfg(test)]
mod test_override {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn get() -> bool {
        FORCED.with(Cell::get)
    }

    /// Sets the flag and returns the previous value.
    pub(super) fn set(value: bool) -> bool {
        FORCED.with(|forced| forced.replace(value))
    }
}

/// Enables eslabon presentations for the current test thread until the guard drops,
/// restoring whatever was set before (guards may nest).
#[cfg(test)]
pub(crate) fn force_for_test() -> impl Drop {
    struct Guard(bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            test_override::set(self.0);
        }
    }
    Guard(test_override::set(true))
}

/// One exploration step rendered as `icon label`.
pub(crate) fn exploration_row(parsed: &ParsedCommand) -> (&'static str, String) {
    match parsed {
        ParsedCommand::Read { name, .. } => (ICON_READ, format!("Read {name}")),
        ParsedCommand::ListFiles { path, .. } => (
            ICON_SEARCH,
            format!("List {}", path.as_deref().unwrap_or(".")),
        ),
        ParsedCommand::Search { query, path, .. } => {
            let mut label = match query {
                Some(query) => format!("Search \"{query}\""),
                None => "Search".to_string(),
            };
            if let Some(path) = path {
                label.push_str(&format!(" in {path}"));
            }
            (ICON_SEARCH, label)
        }
        ParsedCommand::Unknown { cmd } => (ICON_COMMAND, cmd.clone()),
    }
}

/// opencode's exploration summary: "2 reads, 1 search, 1 list".
pub(crate) fn exploration_summary<'a>(
    parsed: impl IntoIterator<Item = &'a ParsedCommand>,
) -> String {
    let (mut reads, mut searches, mut lists, mut other) = (0, 0, 0, 0);
    for command in parsed {
        match command {
            ParsedCommand::Read { .. } => reads += 1,
            ParsedCommand::Search { .. } => searches += 1,
            ParsedCommand::ListFiles { .. } => lists += 1,
            ParsedCommand::Unknown { .. } => other += 1,
        }
    }
    [
        (reads, "read", "reads"),
        (searches, "search", "searches"),
        (lists, "list", "lists"),
        (other, "command", "commands"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
    .collect::<Vec<_>>()
    .join(", ")
}

/// `← Edit src/math.py +3 −0`, one row per file, sorted by path.
pub(crate) fn patch_rows(
    changes: &std::collections::HashMap<std::path::PathBuf, crate::diff_model::FileChange>,
    cwd: &std::path::Path,
    width: u16,
) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
    use crate::diff_model::FileChange;
    use ratatui::style::Stylize;
    use ratatui::text::Line;
    let mut paths: Vec<_> = changes.keys().collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let change = &changes[path];
            let (added, removed) = crate::diff_render::line_counts(change);
            let verb = match change {
                FileChange::Add { .. } => "Add",
                FileChange::Delete { .. } => "Delete",
                FileChange::Update {
                    move_path: Some(_), ..
                } => "Move",
                FileChange::Update { .. } => "Edit",
            };
            let mut target = crate::diff_render::display_path_for(path, cwd);
            if let FileChange::Update {
                move_path: Some(destination),
                ..
            } = change
            {
                target.push_str(&format!(
                    " → {}",
                    crate::diff_render::display_path_for(destination, cwd)
                ));
            }
            let line = Line::from(vec![
                ICON_EDIT.green().bold(),
                " ".into(),
                verb.bold(),
                " ".into(),
                target.into(),
                " ".into(),
                format!("+{added}").green(),
                " ".into(),
                format!("−{removed}").red(),
            ]);
            crate::history_cell::activity_preview::clipped_line(line, width)
        })
        .collect()
}

/// "1.2s" / "350ms" for completed calls.
pub(crate) fn short_duration(duration: std::time::Duration) -> String {
    if duration.as_millis() < 1000 {
        format!("{}ms", duration.as_millis())
    } else if duration.as_secs() < 60 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}m{}s", duration.as_secs() / 60, duration.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_config::eslabon::marca;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn summary_counts_kinds_like_opencode() {
        let parsed = vec![
            ParsedCommand::Read {
                cmd: "cat a".into(),
                name: "a".into(),
                path: PathBuf::from("a"),
            },
            ParsedCommand::Read {
                cmd: "cat b".into(),
                name: "b".into(),
                path: PathBuf::from("b"),
            },
            ParsedCommand::Search {
                cmd: "rg add".into(),
                query: Some("add".into()),
                path: Some("src".into()),
            },
        ];
        assert_eq!(exploration_summary(&parsed), "2 reads, 1 search");
        assert_eq!(
            exploration_row(&parsed[2]),
            (ICON_SEARCH, "Search \"add\" in src".to_string())
        );
    }

    #[test]
    fn patch_rows_show_counts_per_file() {
        let _eslabon = force_for_test();
        let mut changes = std::collections::HashMap::new();
        changes.insert(
            PathBuf::from("/repo/src/math.py"),
            crate::diff_model::FileChange::Update {
                unified_diff: "@@ -1,2 +1,5 @@\n def add(a, b):\n     return a + b\n+\n+def subtract(a, b):\n+    return a - b\n".to_string(),
                move_path: None,
            },
        );
        changes.insert(
            PathBuf::from("/repo/NEW.md"),
            crate::diff_model::FileChange::Add {
                content: "hola\n".to_string(),
            },
        );
        let rows: Vec<String> = patch_rows(&changes, std::path::Path::new("/repo"), 60)
            .into_iter()
            .map(|line| line.line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rows, vec!["← Add NEW.md +1 −0", "← Edit src/math.py +3 −0"]);
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(
            short_duration(std::time::Duration::from_millis(350)),
            "350ms"
        );
        assert_eq!(
            short_duration(std::time::Duration::from_millis(1234)),
            "1.2s"
        );
        assert_eq!(short_duration(std::time::Duration::from_secs(75)), "1m15s");
    }

    #[test]
    fn tests_keep_upstream_presentation_unless_forced() {
        assert!(!enabled());
        let guard = force_for_test();
        assert!(enabled());
        drop(guard);
        assert!(!enabled());
    }

    #[test]
    fn product_copy_follows_the_brand_flag() {
        assert_eq!(product_name(), marca::NOMBRE_PRODUCTO_UPSTREAM);
        assert_eq!(app_name(), "codex");

        let guard = force_for_test();
        assert_eq!(product_name(), marca::NOMBRE_BINARIO);
        assert_eq!(app_name(), marca::NOMBRE_BINARIO);
        assert_eq!(app_name_label(), marca::ETIQUETA_NOMBRE_APP);
        assert_eq!(version_label(), marca::ETIQUETA_VERSION_APP);
        drop(guard);

        assert_eq!(
            app_name_label(),
            marca::ETIQUETA_NOMBRE_APP_UPSTREAM
        );
        assert_eq!(version_label(), marca::ETIQUETA_VERSION_APP_UPSTREAM);
    }
}
