//! eslabon: opencode-style compact rows for commands and explorations.

use super::model::ExecCell;
use crate::eslabon_stream;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::history_cell::ActivityDisclosure;
use crate::history_cell::activity_preview::clipped_line;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::terminal_hyperlinks::HyperlinkLine;
use codex_ansi_escape::ansi_escape_line;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;

impl ExecCell {
    fn eslabon_marker(&self, fallback: Span<'static>) -> Span<'static> {
        if self.is_active() {
            activity_indicator(
                self.active_start_time(),
                MotionMode::from_animations_enabled(self.animations_enabled()),
                ReducedMotionIndicator::StaticBullet,
            )
            .unwrap_or_else(|| "•".dim())
        } else {
            fallback
        }
    }

    pub(super) fn eslabon_compact_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        // Background-terminal interactions keep upstream's dedicated presentation.
        if self
            .iter_calls()
            .any(super::model::ExecCall::is_unified_exec_interaction)
        {
            return self.compact_command_lines(width);
        }
        if self.is_exploring_cell() {
            self.eslabon_exploration_lines(width)
        } else {
            self.eslabon_command_lines(width)
        }
    }

    /// `→ Explored 2 reads, 1 search` followed by one dim row per step.
    fn eslabon_exploration_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let parsed: Vec<_> = self
            .iter_calls()
            .flat_map(|call| call.parsed.iter())
            .collect();
        let steps: Vec<_> = self
            .iter_calls()
            .flat_map(|call| {
                let failed = call.output.as_ref().is_some_and(|out| out.exit_code != 0);
                call.parsed.iter().map(move |parsed| (parsed, failed))
            })
            .collect();
        let failures = self
            .iter_calls()
            .filter(|call| call.output.as_ref().is_some_and(|out| out.exit_code != 0))
            .count();
        let marker = self.eslabon_marker(if failures > 0 {
            eslabon_stream::ICON_FAIL.red().bold()
        } else {
            eslabon_stream::ICON_READ.green().bold()
        });
        let verb = if self.is_active() {
            "Exploring"
        } else {
            "Explored"
        };
        let mut header = Line::from(vec![
            marker,
            " ".into(),
            verb.bold(),
            " ".into(),
            eslabon_stream::exploration_summary(parsed.iter().copied()).into(),
        ]);
        if failures > 0 {
            header.push_span(format!(" · {failures} failed").red());
        }
        let mut lines = vec![clipped_line(header, width)];
        for (parsed, failed) in steps.iter().take(eslabon_stream::EXPLORATION_ROWS) {
            let (icon, label) = eslabon_stream::exploration_row(parsed);
            let row = if *failed {
                Line::from(vec![
                    format!("  {} ", eslabon_stream::ICON_FAIL).red(),
                    label.red(),
                ])
            } else {
                Line::from(vec![format!("  {icon} ").dim(), label.dim()])
            };
            lines.push(clipped_line(row, width));
        }
        if parsed.len() > eslabon_stream::EXPLORATION_ROWS {
            lines.push(clipped_line(
                Line::from(
                    format!(
                        "  +{} more",
                        parsed.len() - eslabon_stream::EXPLORATION_ROWS
                    )
                    .dim(),
                ),
                width,
            ));
        }
        lines
    }

    /// `✓ $ python3 main.py · 2 lines · 0.4s`; failures keep their last output lines.
    fn eslabon_command_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let [call] = self.group.calls.as_slice() else {
            return Vec::new();
        };
        let failed = call.output.as_ref().filter(|output| output.exit_code != 0);
        let marker = self.eslabon_marker(if failed.is_some() {
            eslabon_stream::ICON_FAIL.red().bold()
        } else {
            eslabon_stream::ICON_OK.green().bold()
        });
        let script = strip_bash_lc_and_escape(&call.command);
        let mut first = script.lines().next().unwrap_or_default().to_string();
        if script.lines().nth(1).is_some() {
            first.push_str(" …");
        }
        let mut header = Line::from(vec![
            marker,
            " ".into(),
            format!("{} ", eslabon_stream::ICON_COMMAND).dim(),
            first.into(),
        ]);
        let mut details = Vec::new();
        if let Some(output) = &call.output {
            if let Some(failed) = failed {
                details.push(format!("exit {}", failed.exit_code).red());
            }
            let total = output.line_counts().0;
            if total > 0 {
                details.push(format!("{total} line{}", if total == 1 { "" } else { "s" }).dim());
            }
        }
        // Some calls report no duration; "0ms" would be misleading.
        if let Some(duration) = call
            .duration
            .filter(|duration| !self.is_active() && !duration.is_zero())
        {
            details.push(eslabon_stream::short_duration(duration).dim());
        }
        for detail in details {
            header.push_span(" · ".dim());
            header.push_span(detail);
        }
        let mut lines = vec![clipped_line(header, width)];
        let tail = self.eslabon_visible_tail(failed.is_some());
        if let Some(output) = &call.output {
            let total = output.lines().count();
            for (index, raw) in output.lines().skip(total.saturating_sub(tail)).enumerate() {
                let mut line = ansi_escape_line(raw.as_ref());
                line.spans
                    .insert(0, if index == 0 { "  └ " } else { "    " }.dim());
                line.spans
                    .iter_mut()
                    .for_each(|span| span.style = span.style.add_modifier(Modifier::DIM));
                lines.push(clipped_line(line, width));
            }
        }
        lines
    }

    /// Output lines kept visible: running commands show their latest output and
    /// failures their last lines; successful output is fully collapsed.
    fn eslabon_visible_tail(&self, failed: bool) -> usize {
        if self.is_active() {
            2
        } else if failed {
            eslabon_stream::FAILURE_TAIL_LINES
        } else {
            0
        }
    }

    /// Everything not in the visible tail is behind the disclosure.
    pub(super) fn eslabon_command_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        if self
            .iter_calls()
            .any(super::model::ExecCall::is_unified_exec_interaction)
        {
            return self.command_disclosure(width);
        }
        // Like upstream: a non-exploring cell holds a single call.
        let [call] = self.group.calls.as_slice() else {
            return None;
        };
        match &call.output {
            Some(output) if output.line_counts().0 > 0 => {
                let shown = self.eslabon_visible_tail(output.exit_code != 0);
                let hidden = output.line_counts().1.saturating_sub(shown);
                (hidden > 0).then_some(ActivityDisclosure::OutputLines(hidden))
            }
            _ => strip_bash_lc_and_escape(&call.command)
                .lines()
                .nth(1)
                .is_some()
                .then_some(ActivityDisclosure::Generic),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::model::ExecCall;
    use crate::history_cell::HistoryCell;
    use codex_app_server_protocol::CommandExecutionSource;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn call(id: &str, command: &[&str], output: Option<(i32, &str)>) -> ExecCall {
        let command: Vec<String> = command.iter().map(|part| (*part).to_owned()).collect();
        ExecCall {
            call_id: id.to_owned(),
            parsed: codex_shell_command::parse_command::parse_command(&command),
            command,
            output: output.map(|(code, text)| CommandOutput::new(code, text.to_owned())),
            source: CommandExecutionSource::UnifiedExecStartup,
            start_time: None,
            duration: Some(Duration::from_millis(1234)),
            interaction_input: None,
        }
    }

    fn text(lines: Vec<HyperlinkLine>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn successful_command_is_one_row_with_hidden_output() {
        let _eslabon = crate::eslabon_stream::force_for_test();
        let cell = ExecCell::new(
            call("c1", &["python3", "main.py"], Some((0, "5\n6\n"))),
            /*animations_enabled*/ false,
        );
        assert_eq!(
            text(cell.compact_hyperlink_lines(80)),
            vec!["✓ $ python3 main.py · 2 lines · 1.2s"]
        );
        assert_eq!(
            cell.activity_disclosure(80),
            Some(ActivityDisclosure::OutputLines(2))
        );
    }

    #[test]
    fn failed_command_keeps_its_last_lines() {
        let _eslabon = crate::eslabon_stream::force_for_test();
        let cell = ExecCell::new(
            call(
                "c1",
                &["python3", "-c", "import x"],
                Some((1, "a\nb\nc\nModuleNotFoundError: x\n")),
            ),
            /*animations_enabled*/ false,
        );
        assert_eq!(
            text(cell.compact_hyperlink_lines(80)),
            vec![
                "✗ $ python3 -c 'import x' · exit 1 · 4 lines · 1.2s",
                "  └ b",
                "    c",
                "    ModuleNotFoundError: x",
            ]
        );
        assert_eq!(
            cell.activity_disclosure(80),
            Some(ActivityDisclosure::OutputLines(1))
        );
    }

    #[test]
    fn exploration_is_summarized_like_opencode() {
        let _eslabon = crate::eslabon_stream::force_for_test();
        let mut cell = ExecCell::new(
            call("r1", &["cat", "README.md"], Some((0, "# demo"))),
            /*animations_enabled*/ false,
        );
        let search = ExecCell::new(
            call("s1", &["rg", "add"], Some((0, "main.py:1"))),
            /*animations_enabled*/ false,
        );
        cell.append_completed(search)
            .expect("completed explorations group together");
        assert_eq!(
            text(cell.compact_hyperlink_lines(80)),
            vec![
                "→ Explored 1 read, 1 search",
                "  → Read README.md",
                "  ✱ Search \"add\"",
            ]
        );
    }

    #[test]
    fn upstream_presentation_is_untouched_when_disabled() {
        let cell = ExecCell::new(
            call("c1", &["python3", "main.py"], Some((0, "5\n"))),
            /*animations_enabled*/ false,
        );
        let lines = text(cell.compact_hyperlink_lines(80));
        assert!(lines[0].contains("Ran"), "{lines:?}");
    }
}
