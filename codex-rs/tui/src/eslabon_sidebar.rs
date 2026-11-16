//! eslabon: right-hand session sidebar for the owned (alt-screen) layout.
//!
//! Inspired by opencode's sidebar: model, context usage, prompt-cache hits and the
//! live state of every MCP server. Rendering is a pure function of a
//! [`SidebarSnapshot`], which `ChatWidget` builds from state it already tracks.

use crate::eslabon_speed::LiveSpeed;
use crate::eslabon_speed::Phase;
use crate::eslabon_speed::ServerSession;
use crate::eslabon_speed::ServerStats;
use crate::eslabon_speed::SpeedView;
use crate::status::format_tokens_compact;
use crate::style::StatusTone;
use crate::style::secondary_text_style;
use crate::style::status_style;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

/// Columns taken by the sidebar, including its left separator.
pub(crate) const SIDEBAR_WIDTH: u16 = 34;
/// The main column keeps at least this many columns; narrower terminals hide the sidebar.
const MIN_MAIN_WIDTH: u16 = 80;
/// Splits the screen width into (main, sidebar) when the sidebar should show.
pub(crate) fn split_width(total: u16) -> Option<(u16, u16)> {
    if !crate::eslabon_stream::sidebar_enabled() || total < MIN_MAIN_WIDTH + SIDEBAR_WIDTH {
        return None;
    }
    Some((total - SIDEBAR_WIDTH, SIDEBAR_WIDTH))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpState {
    /// Configured but no status reported yet.
    Pending,
    Starting,
    Ready,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpRow {
    pub(crate) name: String,
    pub(crate) state: McpState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContextUsage {
    /// Tokens currently in the context window (last request).
    pub(crate) used: i64,
    pub(crate) window: Option<i64>,
    /// Input tokens of the last request, and how many came from the prompt cache.
    pub(crate) last_input: i64,
    pub(crate) last_cached: i64,
    /// Session totals.
    pub(crate) total_input: i64,
    pub(crate) total_cached: i64,
    pub(crate) total_output: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SidebarSnapshot {
    pub(crate) model: String,
    pub(crate) provider: String,
    pub(crate) effort: Option<String>,
    pub(crate) cwd: String,
    pub(crate) branch: Option<String>,
    pub(crate) context: Option<ContextUsage>,
    pub(crate) mcp: Vec<McpRow>,
    /// PHI egress block state: `Some(on)` when this profile can toggle it.
    pub(crate) phi: Option<bool>,
    /// Collapse the MCP list (only honoured with more than two servers, like opencode).
    pub(crate) mcp_collapsed: bool,
    /// A turn is running: draw the sweeping activity bar.
    pub(crate) working: bool,
    /// Prompt-reading and generation speed measured on this session's requests.
    pub(crate) speed: SpeedView,
    /// Live requests on the inference server, when it publishes them (the local server).
    pub(crate) server: Option<ServerStats>,
}

/// What a click on a sidebar row refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarTarget {
    Model,
    Context,
    McpHeader,
    McpServer(String),
    Phi,
    Cwd,
}

/// Actions the sidebar menus ask the app to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarEvent {
    /// `config/mcpServer/reload`: restarts failed or disconnected servers, keeps healthy ones.
    ReloadMcp,
    /// Per-server outcome lines on success.
    ReloadMcpFinished(Result<Vec<String>, String>),
    CopyText(String),
    OpenPath(String),
    /// Flip the PHI egress switch (`$CODEX_HOME/eslabon-phi.json`).
    TogglePhi,
    Slash(crate::slash_command::SlashCommand),
    ShowError(String),
}

/// Clickable rows from the last render, in screen coordinates.
pub(crate) type SidebarHits = Vec<(Rect, SidebarTarget)>;

/// Renders the sidebar and returns its clickable rows.
pub(crate) fn render(snapshot: &SidebarSnapshot, area: Rect, buf: &mut Buffer) -> SidebarHits {
    let mut hits = Vec::new();
    if area.width < 4 || area.height == 0 {
        return hits;
    }
    ratatui::widgets::Clear.render(area, buf);
    let separator = secondary_text_style();
    for y in area.top()..area.bottom() {
        buf[(area.x, y)].set_symbol("│").set_style(separator);
    }
    let inner = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(3),
        ..area
    };
    for (offset, (line, target)) in rows(snapshot, inner.width).into_iter().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        if offset >= inner.height {
            break;
        }
        let row = Rect::new(inner.x, inner.y + offset, inner.width, 1);
        line.render(row, buf);
        if let Some(target) = target {
            // The whole row, separator included, is the click target.
            hits.push((Rect::new(area.x, row.y, area.width, 1), target));
        }
    }
    hits
}

pub(crate) fn hit_test(hits: &SidebarHits, column: u16, row: u16) -> Option<&SidebarTarget> {
    hits.iter()
        .find(|(rect, _)| {
            column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
        })
        .map(|(_, target)| target)
}

type Row = (Line<'static>, Option<SidebarTarget>);

fn rows(snapshot: &SidebarSnapshot, width: u16) -> Vec<Row> {
    let dim = secondary_text_style();
    let max = usize::from(width);
    let mut out: Vec<Row> = Vec::new();
    if snapshot.working {
        out.push((activity_bar(width, activity_phase()), None));
    }

    out.push((
        Line::from(truncate(&snapshot.model, max).bold()),
        Some(SidebarTarget::Model),
    ));
    let mut detail = snapshot.provider.clone();
    if let Some(effort) = &snapshot.effort {
        detail.push_str(&format!(" · {effort}"));
    }
    out.push((
        Line::from(Span::styled(truncate(&detail, max), dim)),
        Some(SidebarTarget::Model),
    ));
    out.push((Line::default(), None));

    let context = Some(SidebarTarget::Context);
    out.push((Line::from("Context".bold()), context.clone()));
    match &snapshot.context {
        None => out.push((
            Line::from(Span::styled("waiting for first turn", dim)),
            context,
        )),
        Some(ctx) => {
            let used = format_tokens_compact(ctx.used);
            match ctx.window.filter(|w| *w > 0) {
                Some(window) => {
                    let percent = (ctx.used as f64 / window as f64 * 100.0).clamp(0.0, 100.0);
                    out.push((
                        Line::from(vec![
                            Span::raw(format!("{used} / {} ", format_tokens_compact(window))),
                            Span::styled(format!("{percent:.0}%"), percent_style(percent)),
                        ]),
                        context.clone(),
                    ));
                    out.push((bar(percent, width), context.clone()));
                }
                None => out.push((Line::from(format!("{used} tokens")), context.clone())),
            }
            if ctx.last_input > 0 {
                let hit = cache_percent(ctx.last_cached, ctx.last_input);
                out.push((
                    Line::from(vec![
                        Span::styled("cache ", dim),
                        Span::raw(format!("{hit:.0}%")),
                        Span::styled(
                            format!(
                                " · {} of {}",
                                format_tokens_compact(ctx.last_cached),
                                format_tokens_compact(ctx.last_input)
                            ),
                            dim,
                        ),
                    ]),
                    context.clone(),
                ));
            }
            out.push((
                Line::from(Span::styled(
                    format!(
                        "session in {} · out {}",
                        format_tokens_compact(ctx.total_input),
                        format_tokens_compact(ctx.total_output)
                    ),
                    dim,
                )),
                context.clone(),
            ));
            if ctx.total_input > 0 {
                out.push((
                    Line::from(Span::styled(
                        format!(
                            "session cache {:.0}%",
                            cache_percent(ctx.total_cached, ctx.total_input)
                        ),
                        dim,
                    )),
                    context,
                ));
            }
        }
    }
    out.push((Line::default(), None));
    if speed_rows(&snapshot.speed, &mut out) {
        out.push((Line::default(), None));
    }
    if let Some(server) = &snapshot.server {
        server_rows(server, width, &mut out);
        out.push((Line::default(), None));
    }

    let collapsible = snapshot.mcp.len() > 2;
    let collapsed = collapsible && snapshot.mcp_collapsed;
    let mut header: Vec<Span<'static>> = Vec::new();
    if collapsible {
        header.push(Span::raw(if collapsed { "▶ " } else { "▼ " }));
    }
    header.push("MCP".bold());
    if collapsed {
        let active = snapshot
            .mcp
            .iter()
            .filter(|row| row.state == McpState::Ready)
            .count();
        let errors = snapshot
            .mcp
            .iter()
            .filter(|row| matches!(row.state, McpState::Failed(_)))
            .count();
        let mut summary = format!(" ({active} active");
        if errors > 0 {
            summary.push_str(&format!(
                ", {errors} error{}",
                if errors > 1 { "s" } else { "" }
            ));
        }
        summary.push(')');
        header.push(Span::styled(summary, dim));
    }
    out.push((
        Line::from(header),
        collapsible.then_some(SidebarTarget::McpHeader),
    ));
    if snapshot.mcp.is_empty() {
        out.push((Line::from(Span::styled("no servers configured", dim)), None));
    }
    if !collapsed {
        for row in &snapshot.mcp {
            let target = Some(SidebarTarget::McpServer(row.name.clone()));
            let (dot, label) = match &row.state {
                McpState::Ready => (status_style(StatusTone::Success), "connected"),
                McpState::Starting | McpState::Pending => {
                    (status_style(StatusTone::Attention), "starting")
                }
                McpState::Failed(_) => (status_style(StatusTone::Failure), "failed"),
                McpState::Cancelled => (dim, "cancelled"),
            };
            out.push((
                Line::from(vec![
                    Span::styled("● ", dot),
                    Span::raw(truncate(&row.name, max.saturating_sub(2 + label.len() + 1))),
                    Span::styled(format!(" {label}"), dim),
                ]),
                target.clone(),
            ));
            if let McpState::Failed(error) = &row.state {
                out.push((
                    Line::from(Span::styled(
                        format!("  {}", truncate(error, max.saturating_sub(2))),
                        dim,
                    )),
                    target,
                ));
            }
        }
    }
    out.push((Line::default(), None));
    if let Some(on) = snapshot.phi {
        let (dot, label) = if on {
            (status_style(StatusTone::Failure), "on · local/BAA only")
        } else {
            (dim, "off")
        };
        out.push((
            Line::from(vec![
                "Data lock ".bold(),
                Span::styled("● ", dot),
                Span::styled(label, dim),
            ]),
            Some(SidebarTarget::Phi),
        ));
        out.push((Line::default(), None));
    }
    let location = match &snapshot.branch {
        Some(branch) => format!("{}:{branch}", snapshot.cwd),
        None => snapshot.cwd.clone(),
    };
    out.push((
        Line::from(Span::styled(truncate_left(&location, max), dim)),
        Some(SidebarTarget::Cwd),
    ));
    out
}

/// Active server requests listed before collapsing into "+N more".
const SERVER_ROWS: usize = 3;
/// Saved conversations listed one line each.
const SAVED_ROWS: usize = 2;

fn cached_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn fmt_rate(tps: f64) -> String {
    if tps >= 100.0 {
        format!("{tps:.0}")
    } else {
        format!("{tps:.1}")
    }
}

fn fmt_secs(secs: f64) -> String {
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{secs:.0}s")
    } else {
        let whole = secs as u64;
        format!("{}m{:02}s", whole / 60, whole % 60)
    }
}

fn fmt_ago(secs: u64) -> String {
    match secs {
        0..=4 => "just now".to_string(),
        5..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

fn fmt_bytes(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes >= 1e9 {
        format!("{:.1} GB", bytes / 1e9)
    } else if bytes >= 1e6 {
        format!("{:.0} MB", bytes / 1e6)
    } else {
        format!("{:.0} KB", bytes / 1e3)
    }
}

/// `label` padded so the numbers line up, then a bold value and its dimmed unit.
fn metric(label: &str, value: String, rest: String) -> Line<'static> {
    let dim = secondary_text_style();
    Line::from(vec![
        Span::styled(format!("{label:<7}"), dim),
        Span::raw(value).bold(),
        Span::styled(rest, dim),
    ])
}

/// Left text and right text on one row, the right one flush with the edge.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|span| span.content.chars().count())
        .sum();
    let gap = usize::from(width).saturating_sub(used).max(1);
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    Line::from(spans)
}

/// The speed block. Returns `false` when there is nothing to show yet.
fn speed_rows(speed: &SpeedView, out: &mut Vec<Row>) -> bool {
    if speed.live.is_none() && speed.last.is_none() {
        return false;
    }
    let dim = secondary_text_style();
    out.push((Line::from("Speed".bold()), None));
    match &speed.live {
        Some(LiveSpeed::Reading { secs }) => out.push((
            Line::from(vec![
                Span::styled("● ", status_style(StatusTone::Attention)),
                Span::raw("reading prompt "),
                Span::styled(fmt_secs(*secs), dim),
            ]),
            None,
        )),
        Some(LiveSpeed::Generating { approx_tps, secs }) => {
            let mut spans = vec![
                Span::styled("● ", status_style(StatusTone::Success)),
                Span::raw("generating "),
            ];
            if let Some(tps) = approx_tps {
                spans.push(Span::raw(format!("~{}", fmt_rate(*tps))).bold());
                spans.push(Span::styled(" tok/s", dim));
            } else {
                spans.push(Span::styled(fmt_secs(*secs), dim));
            }
            out.push((Line::from(spans), None));
        }
        None => {}
    }
    if let Some(last) = &speed.last {
        match last.decode_tps {
            Some(tps) => out.push((
                metric(
                    "gen",
                    fmt_rate(tps),
                    format!(" tok/s · {}", format_tokens_compact(last.output)),
                ),
                None,
            )),
            None => out.push((metric("gen", "—".to_string(), String::new()), None)),
        }
        match last.prefill_tps {
            Some(tps) => out.push((
                metric(
                    "prompt",
                    fmt_rate(tps),
                    format!(" tok/s · {} new", format_tokens_compact(last.new_tokens)),
                ),
                None,
            )),
            None => out.push((
                Line::from(vec![
                    Span::styled(format!("{:<7}", "prompt"), dim),
                    Span::styled("all from cache", cached_style()),
                ]),
                None,
            )),
        }
        out.push((
            Line::from(Span::styled(
                format!(
                    "first token {} · {}",
                    fmt_secs(last.ttft_secs),
                    fmt_ago(last.ago_secs)
                ),
                dim,
            )),
            None,
        ));
    }
    true
}

/// Context bar split like the local server's dashboard: reused from cache, new this request, free.
fn context_split_bar(session: &ServerSession, width: u16) -> Line<'static> {
    let cells = usize::from(width).max(1);
    let window = session.window.max(session.context).max(1) as f64;
    let mut used = ((session.context as f64 / window) * cells as f64).round() as usize;
    if session.context > 0 {
        used = used.max(1);
    }
    let used = used.min(cells);
    let mut cached = ((session.cached.min(session.context) as f64 / window) * cells as f64).round() as usize;
    if session.cached > 0 {
        cached = cached.max(1);
    }
    let cached = cached.min(used);
    // A request with new tokens always shows at least one new cell.
    let cached = if session.context > session.cached && cached == used && used > 0 {
        cached - 1
    } else {
        cached
    };
    Line::from(vec![
        Span::styled("━".repeat(cached), cached_style()),
        Span::styled("━".repeat(used - cached), crate::style::accent_style()),
        Span::styled("━".repeat(cells - used), secondary_text_style()),
    ])
}

/// One line of "label value · label value tok/s" pairs; `None` when there is nothing.
fn rate_pairs(lead: &str, pairs: &[(&str, Option<f64>)]) -> Option<Line<'static>> {
    let dim = secondary_text_style();
    let mut spans = vec![Span::styled(format!("{lead} "), dim)];
    for (label, value) in pairs {
        let Some(tps) = value else { continue };
        if spans.len() > 1 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(format!("{label} "), dim));
        spans.push(Span::raw(fmt_rate(*tps)).bold());
    }
    if spans.len() == 1 {
        return None;
    }
    // " t/s" rather than " tok/s": both rates fit in the 31 usable columns.
    spans.push(Span::styled(" t/s", dim));
    Some(Line::from(spans))
}

fn server_rows(server: &ServerStats, width: u16, out: &mut Vec<Row>) {
    let dim = secondary_text_style();
    let max = usize::from(width);
    // "Server llama.cpp · example-host"
    let label = format!("{} · {}", server.engine, server.host);
    out.push((
        Line::from(vec![
            "Server ".bold(),
            Span::styled(truncate(&label, max.saturating_sub(7)), dim),
        ]),
        None,
    ));
    let active: Vec<&ServerSession> = server
        .sessions
        .iter()
        .filter(|s| s.phase != Phase::Saved)
        .collect();
    let saved: Vec<&ServerSession> = server
        .sessions
        .iter()
        .filter(|s| s.phase == Phase::Saved)
        .collect();
    // Load line: engines without per-request rows (vLLM, SGLang…) only have this.
    let mut load = Vec::new();
    if let Some(running) = server.running {
        load.push(format!("{running} running"));
    }
    if server.waiting > 0 {
        load.push(format!("{} waiting", server.waiting));
    }
    if let Some(kv) = server.kv_pct {
        load.push(format!("KV {kv:.0}%"));
    }
    if active.is_empty() && saved.is_empty() && server.running.unwrap_or(0) == 0 {
        load.push(if server.sessions.is_empty() && server.running.is_none() {
            "no request details".to_string()
        } else {
            "idle".to_string()
        });
    }
    if !load.is_empty() {
        out.push((Line::from(Span::styled(truncate(&load.join(" · "), max), dim)), None));
    }
    for session in active.iter().take(SERVER_ROWS) {
        let id = session.id.map(|id| format!("#{id} ")).unwrap_or_default();
        let (phase, tone, right): (String, Style, Vec<Span<'static>>) = match &session.phase {
            Phase::Prefill => (
                "prefill".to_string(),
                status_style(StatusTone::Attention),
                match session.prefill_pct {
                    Some(pct) => vec![Span::raw(format!("{pct:.0}%")).bold()],
                    None => vec![Span::styled("reading", dim)],
                },
            ),
            Phase::Decode => (
                "decode".to_string(),
                status_style(StatusTone::Success),
                match session.rate {
                    Some(tps) => vec![
                        Span::raw(fmt_rate(tps)).bold(),
                        Span::styled(" tok/s", dim),
                    ],
                    None => Vec::new(),
                },
            ),
            Phase::Saved | Phase::Other(_) => (
                match &session.phase {
                    Phase::Other(name) => name.clone(),
                    _ => "saved".to_string(),
                },
                dim,
                Vec::new(),
            ),
        };
        out.push((
            spread(
                vec![Span::styled(id, dim), Span::styled(phase, tone)],
                right,
                width,
            ),
            None,
        ));
        out.push((context_split_bar(session, width), None));
        // "250K · 249K cached · 1K new" stays within the 31 usable columns.
        let new_tokens = (session.context - session.cached).max(0);
        out.push((
            Line::from(vec![
                // The window size is what the bar is drawn against; repeating it here
                // pushes the line past the 31 usable columns.
                Span::styled(format!("{} · ", format_tokens_compact(session.context)), dim),
                Span::styled(
                    format!("{} cached", format_tokens_compact(session.cached)),
                    cached_style(),
                ),
                Span::styled(" · ", dim),
                Span::styled(
                    format!("{} new", format_tokens_compact(new_tokens)),
                    crate::style::accent_style(),
                ),
            ]),
            None,
        ));
        let mut detail = Vec::new();
        if session.phase == Phase::Decode {
            detail.push(format!(
                "gen {}{}",
                format_tokens_compact(session.generated),
                session
                    .max_tokens
                    .map(|m| format!("/{}", format_tokens_compact(m)))
                    .unwrap_or_default()
            ));
        } else if !session.model.is_empty() {
            detail.push(session.model.clone());
        }
        if let Some(secs) = session.elapsed_secs {
            detail.push(fmt_secs(secs));
        }
        if !detail.is_empty() {
            out.push((Line::from(Span::styled(truncate(&detail.join(" · "), max), dim)), None));
        }
    }
    for session in saved.iter().take(SAVED_ROWS) {
        let mut spans = vec![
            Span::styled("saved ", dim),
            Span::styled(format_tokens_compact(session.context), cached_style()),
        ];
        match session.state_bytes {
            Some(bytes) if bytes > 0 => {
                spans.push(Span::styled(format!(" · {}", fmt_bytes(bytes)), dim))
            }
            _ if !session.model.is_empty() => {
                spans.push(Span::styled(format!(" · {}", session.model), dim))
            }
            _ => {}
        }
        out.push((Line::from(spans), None));
    }
    let hidden = active.len().saturating_sub(SERVER_ROWS) + saved.len().saturating_sub(SAVED_ROWS);
    if hidden > 0 {
        out.push((Line::from(Span::styled(format!("+{hidden} more"), dim)), None));
    }
    if let Some(now) = &server.now
        && let Some(line) = rate_pairs("now", &[("gen", Some(now.gen_tps)), ("prompt", Some(now.prompt_tps))])
    {
        out.push((line, None));
    }
    if let Some(last) = &server.last
        && let Some(line) = rate_pairs(last.label, &[("gen", last.decode_tps), ("prompt", last.prefill_tps)])
    {
        out.push((line, None));
    }
}

/// Milliseconds-based phase so the bar advances on every redraw.
fn activity_phase() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis() / 70).unwrap_or(0)
        })
}

/// opencode-style sweep: a short block bouncing between both edges.
fn activity_bar(width: u16, phase: u64) -> Line<'static> {
    let width = usize::from(width).max(8);
    let block = (width / 5).clamp(3, 8);
    let travel = width - block;
    let span = (travel * 2).max(1) as u64;
    let step = usize::try_from(phase % span).unwrap_or(0);
    let start = if step <= travel {
        step
    } else {
        travel * 2 - step
    };
    Line::from(vec![
        Span::styled("─".repeat(start), secondary_text_style()),
        Span::styled("━".repeat(block), crate::style::accent_style().bold()),
        Span::styled("─".repeat(width - block - start), secondary_text_style()),
    ])
}

/// Codex counts cached tokens inside `input_tokens`; clamp anyway in case a
/// backend reports them separately.
fn cache_percent(cached: i64, input: i64) -> f64 {
    if input <= 0 {
        return 0.0;
    }
    (cached as f64 / input as f64 * 100.0).clamp(0.0, 100.0)
}

fn percent_style(percent: f64) -> Style {
    if percent >= 90.0 {
        status_style(StatusTone::Failure)
    } else if percent >= 70.0 {
        status_style(StatusTone::Attention)
    } else {
        Style::default()
    }
}

fn bar(percent: f64, width: u16) -> Line<'static> {
    let cells = usize::from(width).max(1);
    let filled = ((percent / 100.0) * cells as f64).round() as usize;
    let filled = filled.min(cells);
    Line::from(vec![
        Span::styled("━".repeat(filled), percent_style(percent).bold()),
        Span::styled("━".repeat(cells - filled), secondary_text_style()),
    ])
}

fn truncate(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default();
    if first_line.chars().count() <= max {
        return first_line.to_string();
    }
    let kept: String = first_line.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Keeps the end of a path, which is the part that identifies the project.
fn truncate_left(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count - max.saturating_sub(1)).collect();
    format!("…{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;

    fn render_to_string(snapshot: &SidebarSnapshot, height: u16) -> String {
        let area = Rect::new(0, 0, SIDEBAR_WIDTH, height);
        let mut buf = Buffer::empty(area);
        let _ = render(snapshot, area, &mut buf);
        (0..height)
            .map(|y| {
                (0..SIDEBAR_WIDTH)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample() -> SidebarSnapshot {
        SidebarSnapshot {
            model: "qwen3.8-flash-next-coder".to_string(),
            provider: "local".to_string(),
            effort: Some("medium".to_string()),
            cwd: "/home/user/testing/eslabon/codex-rs".to_string(),
            branch: Some("eslabon".to_string()),
            phi: Some(true),
            mcp_collapsed: false,
            working: false,
            speed: SpeedView::default(),
            server: None,
            context: Some(ContextUsage {
                used: 42_527,
                window: Some(131_072),
                last_input: 42_527,
                last_cached: 40_197,
                total_input: 95_000,
                total_cached: 80_000,
                total_output: 1_200,
            }),
            mcp: vec![
                McpRow {
                    name: "remote".to_string(),
                    state: McpState::Ready,
                },
                McpRow {
                    name: "repomix".to_string(),
                    state: McpState::Starting,
                },
                McpRow {
                    name: "gemini".to_string(),
                    state: McpState::Failed("handshake timed out after 30s".to_string()),
                },
            ],
        }
    }

    #[test]
    fn renders_full_snapshot() {
        assert_snapshot!(render_to_string(&sample(), 18));
    }

    #[test]
    fn renders_before_first_turn_without_servers() {
        let snapshot = SidebarSnapshot {
            context: None,
            mcp: Vec::new(),
            effort: None,
            ..sample()
        };
        assert_snapshot!(render_to_string(&snapshot, 10));
    }

    #[test]
    fn collapsed_mcp_list_shows_summary_and_rows_are_clickable() {
        let collapsed = SidebarSnapshot {
            mcp_collapsed: true,
            ..sample()
        };
        assert_snapshot!(render_to_string(&collapsed, 18));

        let area = Rect::new(10, 0, SIDEBAR_WIDTH, 30);
        let mut buf = Buffer::empty(area);
        let hits = render(&sample(), area, &mut buf);
        let row_of = |target: &SidebarTarget| {
            hits.iter()
                .find(|(_, t)| t == target)
                .map(|(rect, _)| rect.y)
                .expect("target rendered")
        };
        let gemini = SidebarTarget::McpServer("gemini".to_string());
        let y = row_of(&gemini);
        assert_eq!(hit_test(&hits, 12, y), Some(&gemini));
        // The error line under a failed server opens the same server.
        assert_eq!(hit_test(&hits, 12, y + 1), Some(&gemini));
        assert_eq!(
            hit_test(&hits, 12, row_of(&SidebarTarget::Cwd)),
            Some(&SidebarTarget::Cwd)
        );
        assert_eq!(hit_test(&hits, 9, y), None);
    }

    #[test]
    fn hides_on_narrow_terminals() {
        assert_eq!(split_width(200), None, "off unless the binary enables it");
        let _eslabon = crate::eslabon_stream::force_for_test();
        assert_eq!(split_width(MIN_MAIN_WIDTH + SIDEBAR_WIDTH - 1), None);
        assert_eq!(
            split_width(MIN_MAIN_WIDTH + SIDEBAR_WIDTH),
            Some((MIN_MAIN_WIDTH, SIDEBAR_WIDTH))
        );
    }

    fn session(phase: Phase, id: i64, context: i64, cached: i64) -> ServerSession {
        ServerSession {
            id: Some(id),
            model: "qwen-coder".to_string(),
            phase,
            context,
            window: 1_048_576,
            cached,
            generated: 2_300,
            max_tokens: Some(32_768),
            elapsed_secs: Some(62.0),
            state_bytes: Some(1_200_000_000),
            rate: Some(41.3),
            prefill_pct: Some(62.0),
        }
    }

    #[test]
    fn speed_and_server_blocks_fit_the_sidebar() {
        use crate::eslabon_speed::LastSpeed;
        use crate::eslabon_speed::ServerRequest;
        use crate::eslabon_speed::Throughput;
        let local = SidebarSnapshot {
            mcp_collapsed: true,
            speed: SpeedView {
                live: Some(LiveSpeed::Generating { approx_tps: Some(79.6), secs: 3.0 }),
                last: Some(LastSpeed {
                    decode_tps: Some(86.5),
                    output: 1_200,
                    prefill_tps: Some(684.0),
                    new_tokens: 14_984,
                    ttft_secs: 2.1,
                    ago_secs: 540,
                }),
            },
            server: Some(ServerStats {
                host: "example-host".to_string(),
                engine: "local",
                sessions: vec![
                    session(Phase::Decode, 381, 250_000, 249_000),
                    session(Phase::Prefill, 382, 9_000, 7_000),
                    session(Phase::Saved, 377, 53_000, 53_000),
                ],
                running: Some(2),
                waiting: 1,
                kv_pct: None,
                now: Some(Throughput { prompt_tps: 512.0, gen_tps: 41.3 }),
                last: Some(ServerRequest { label: "last", prefill_tps: Some(684.0), decode_tps: Some(119.5) }),
            }),
            ..sample()
        };
        let text = render_to_string(&local, 60);
        for line in text.lines() {
            assert!(line.chars().count() <= usize::from(SIDEBAR_WIDTH), "{line}");
        }
        for needle in [
            "Speed", "generating ~79.6 tok/s", "gen    86.5 tok/s · 1.2K", "prompt 684 tok/s · 15K new",
            "first token 2.1s · 9m ago", "Server local · example-host", "2 running · 1 waiting",
            "#381 decode", "41.3 tok/s", "250K · 249K cached · 1K new", "gen 2.3K/32.8K · 1m02s",
            "#382 prefill", "62%", "saved 53K · 1.2 GB", "now gen 41.3 · prompt 512 t/s",
            "last gen 120 · prompt 684 t/s",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }

        // vLLM-style engine: only load and throughput, no per-request rows.
        let vllm = SidebarSnapshot {
            server: Some(ServerStats {
                host: "gpu-box".to_string(),
                engine: "vllm",
                sessions: Vec::new(),
                running: Some(3),
                waiting: 0,
                kv_pct: Some(25.0),
                now: None,
                last: None,
            }),
            ..sample()
        };
        let text = render_to_string(&vllm, 40);
        assert!(text.contains("Server vllm · gpu-box"), "{text}");
        assert!(text.contains("3 running · KV 25%"), "{text}");
        assert!(!text.contains("Speed"), "no speed block before the first reply");
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("áéíóú añadido", 5), "áéíó…");
        assert_eq!(truncate_left("/home/user/año", 6), "…r/año");
    }

    #[test]
    fn activity_bar_bounces_within_width() {
        for phase in 0..200 {
            let line = activity_bar(30, phase);
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text.chars().count(), 30, "phase {phase}");
            assert_eq!(
                text.chars().filter(|c| *c == '━').count(),
                6,
                "phase {phase}"
            );
        }
    }

    #[test]
    fn cache_percent_is_clamped() {
        assert_eq!(cache_percent(150, 100), 100.0);
        assert_eq!(cache_percent(5, 0), 0.0);
    }
}
