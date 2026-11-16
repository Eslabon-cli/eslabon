//! eslabon: live model speed for the sidebar.
//!
//! Two sources:
//! - [`SpeedMeter`] measures every model from the client side: time to first token
//!   (prompt reading, plus any queue or cache restore) and generation speed, using the
//!   token usage each reply reports. Works with any provider.
//! - [`server_stats`] reads what a local inference server publishes about itself:
//!   the local server's `/metrics.json` and llama.cpp's `/slots` give every request with its phase
//!   (reading the prompt / generating / kept for reuse) and how much of its context came
//!   from the prompt cache; any engine with a Prometheus `/metrics` (llama.cpp
//!   `--metrics`, vLLM, SGLang, TGI) gives load, KV-cache use and current throughput.
//!   Only hosts on the local network are probed; a host without metrics is retried rarely.

use crate::tui::FrameRequester;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

/// Rough characters per token, only for the live estimate while a reply streams.
const CHARS_PER_TOKEN: f64 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LiveSpeed {
    /// Request sent, no output yet: the server is reading the prompt (or queued).
    Reading { secs: f64 },
    /// Output is streaming; the rate is estimated from characters until usage arrives.
    Generating { approx_tps: Option<f64>, secs: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LastSpeed {
    pub(crate) decode_tps: Option<f64>,
    pub(crate) output: i64,
    pub(crate) prefill_tps: Option<f64>,
    /// Prompt tokens not served from the cache.
    pub(crate) new_tokens: i64,
    pub(crate) ttft_secs: f64,
    pub(crate) ago_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SpeedView {
    pub(crate) live: Option<LiveSpeed>,
    pub(crate) last: Option<LastSpeed>,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    decode_tps: Option<f64>,
    output: i64,
    prefill_tps: Option<f64>,
    new_tokens: i64,
    ttft: Duration,
    at: Instant,
}

/// Client-side timing of each model request in a turn.
#[derive(Debug, Default)]
pub(crate) struct SpeedMeter {
    request_start: Option<Instant>,
    first_output: Option<Instant>,
    /// Latest streamed text. Usage is reported only after the reply's tools finish,
    /// so generation ends here, not when usage arrives.
    last_output: Option<Instant>,
    chars: usize,
    last: Option<Sample>,
    /// Session total at the last usage report; repeats of the same report are ignored.
    last_total: Option<i64>,
}

impl SpeedMeter {
    /// A turn started: the first request goes out now.
    pub(crate) fn turn_started(&mut self, now: Instant) {
        self.request_start = Some(now);
        self.first_output = None;
        self.chars = 0;
    }

    /// Streamed text (answer, reasoning or plan) arrived.
    pub(crate) fn output(&mut self, now: Instant, chars: usize) {
        if self.request_start.is_none() {
            return;
        }
        self.first_output.get_or_insert(now);
        self.last_output = Some(now);
        self.chars += chars;
    }

    /// A tool finished. Before the next reply starts streaming, that is when the
    /// next request is sent. Callers pass only tool items, never reasoning or
    /// messages (a hidden-reasoning item completes right before the answer streams).
    pub(crate) fn tool_completed(&mut self, now: Instant) {
        if self.request_start.is_some() && self.first_output.is_none() {
            self.request_start = Some(now);
        }
    }

    /// A reply finished and reported its usage. Requests without streamed output
    /// (a bare tool call) give no first-token time, so they keep the previous sample.
    pub(crate) fn reply_finished(
        &mut self,
        now: Instant,
        session_total: i64,
        input: i64,
        cached: i64,
        output: i64,
    ) {
        // Repeats of the same report (rate-limit refreshes, compaction) add no sample.
        let fresh = self.last_total.replace(session_total) != Some(session_total);
        if let (true, Some(start), Some(first)) = (fresh, self.request_start, self.first_output) {
            let ttft = first.saturating_duration_since(start);
            let end = self.last_output.unwrap_or(now);
            let decode = end.saturating_duration_since(first).as_secs_f64();
            let new_tokens = (input - cached.min(input)).max(0);
            let ttft_secs = ttft.as_secs_f64();
            self.last = Some(Sample {
                decode_tps: (output > 0 && decode > 0.05).then(|| output as f64 / decode),
                output,
                prefill_tps: (new_tokens > 0 && ttft_secs > 0.01)
                    .then(|| new_tokens as f64 / ttft_secs),
                new_tokens,
                ttft,
                at: now,
            });
        }
        // Whatever follows (tools, then the next request) starts from here.
        if self.request_start.is_some() {
            self.request_start = Some(now);
        }
        self.first_output = None;
        self.last_output = None;
        self.chars = 0;
    }

    pub(crate) fn turn_finished(&mut self) {
        self.request_start = None;
        self.first_output = None;
        self.last_output = None;
        self.chars = 0;
    }

    pub(crate) fn view(&self, now: Instant, working: bool) -> SpeedView {
        let live = match (working, self.request_start, self.first_output) {
            (true, Some(_), Some(first)) => {
                let secs = now.saturating_duration_since(first).as_secs_f64();
                Some(LiveSpeed::Generating {
                    approx_tps: (secs > 0.5 && self.chars > 0)
                        .then(|| self.chars as f64 / CHARS_PER_TOKEN / secs),
                    secs,
                })
            }
            (true, Some(start), None) => Some(LiveSpeed::Reading {
                secs: now.saturating_duration_since(start).as_secs_f64(),
            }),
            _ => None,
        };
        let last = self.last.map(|s| LastSpeed {
            decode_tps: s.decode_tps,
            output: s.output,
            prefill_tps: s.prefill_tps,
            new_tokens: s.new_tokens,
            ttft_secs: s.ttft.as_secs_f64(),
            ago_secs: now.saturating_duration_since(s.at).as_secs(),
        });
        SpeedView { live, last }
    }
}

// ---------------------------------------------------------------------------
// Server metrics (the local server's `/metrics.json`)

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Phase {
    Prefill,
    Decode,
    /// A finished conversation the server keeps for a fast follow-up.
    Saved,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerSession {
    pub(crate) id: Option<i64>,
    pub(crate) model: String,
    pub(crate) phase: Phase,
    pub(crate) context: i64,
    pub(crate) window: i64,
    pub(crate) cached: i64,
    pub(crate) generated: i64,
    pub(crate) max_tokens: Option<i64>,
    pub(crate) elapsed_secs: Option<f64>,
    pub(crate) state_bytes: Option<i64>,
    /// Generation rate between the last two polls.
    pub(crate) rate: Option<f64>,
    /// Prompt read so far, when this is the one request prefilling.
    pub(crate) prefill_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerRequest {
    /// "last" (one request, local metrics) or "avg" (llama.cpp's running average).
    pub(crate) label: &'static str,
    pub(crate) prefill_tps: Option<f64>,
    pub(crate) decode_tps: Option<f64>,
}

/// Tokens per second the whole server is processing right now (counter deltas).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Throughput {
    pub(crate) prompt_tps: f64,
    pub(crate) gen_tps: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerStats {
    pub(crate) host: String,
    /// "local-metrics", "llama.cpp", "vllm", "sglang", "tgi" or "prometheus".
    pub(crate) engine: &'static str,
    /// Per-request rows, when the engine publishes them (local metrics, llama.cpp).
    pub(crate) sessions: Vec<ServerSession>,
    pub(crate) running: Option<i64>,
    pub(crate) waiting: i64,
    pub(crate) kv_pct: Option<f64>,
    pub(crate) now: Option<Throughput>,
    pub(crate) last: Option<ServerRequest>,
}

/// One poll, before rates that need the previous poll are filled in.
#[derive(Debug, Default)]
pub(crate) struct Reading {
    pub(crate) sessions: Vec<ServerSession>,
    pub(crate) running: Option<i64>,
    pub(crate) waiting: i64,
    pub(crate) kv_pct: Option<f64>,
    pub(crate) last: Option<ServerRequest>,
    /// Newest finished request (local metrics), to ask only for newer ones next time.
    pub(crate) last_t_ms: Option<i64>,
    /// Cumulative (prompt, generated) token counters.
    pub(crate) counters: Option<(f64, f64)>,
}

fn num(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_f64).map_or(0, |v| v as i64)
}

fn float(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn rate(tokens: i64, secs: f64) -> Option<f64> {
    (tokens > 0 && secs > 0.0).then(|| tokens as f64 / secs)
}

/// The local server's `/metrics.json`. `None` when the body is not in that format.
pub(crate) fn parse_local_metrics(body: &Value) -> Option<Reading> {
    let sessions = body.get("sessions")?.as_array()?;
    let gauges = body.get("gauges").cloned().unwrap_or(Value::Null);
    let counters = body.get("counters").cloned().unwrap_or(Value::Null);
    let expected = num(&gauges, "prefill_tokens_expected");
    let done = num(&gauges, "prefill_tokens_live").min(expected);
    let prefilling = sessions
        .iter()
        .filter(|s| s.get("phase").and_then(Value::as_str) == Some("prefill"))
        .count();
    let rows = sessions
        .iter()
        .map(|s| {
            let phase = match s.get("phase").and_then(Value::as_str).unwrap_or_default() {
                "prefill" => Phase::Prefill,
                "decode" => Phase::Decode,
                "cached" => Phase::Saved,
                other => Phase::Other(other.to_string()),
            };
            // The server tracks one prefill at a time, so the share only fits a lone row.
            let prefill_pct = (phase == Phase::Prefill && prefilling == 1 && expected > 0)
                .then(|| 100.0 * done as f64 / expected as f64);
            ServerSession {
                id: s.get("request_id").and_then(Value::as_i64),
                model: s
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                phase,
                context: num(s, "context_tokens"),
                window: num(s, "context_length"),
                cached: num(s, "cached_tokens"),
                generated: num(s, "generated_tokens"),
                max_tokens: s.get("max_tokens").and_then(Value::as_i64),
                elapsed_secs: s.get("elapsed_seconds").and_then(Value::as_f64),
                state_bytes: s.get("state_bytes").and_then(Value::as_i64),
                rate: None,
                prefill_pct,
            }
        })
        .collect();
    let newest = body
        .get("requests")
        .and_then(Value::as_array)
        .and_then(|r| r.last());
    let last = newest.map(|r| ServerRequest {
        label: "last",
        prefill_tps: rate(num(r, "prompt_tokens"), float(r, "prefill_seconds")),
        decode_tps: rate(num(r, "generated_tokens"), float(r, "decode_seconds")),
    });
    let counters = counters
        .get("prefill_tokens_total")
        .and_then(Value::as_f64)
        .zip(counters.get("generation_tokens_total").and_then(Value::as_f64));
    Some(Reading {
        sessions: rows,
        running: gauges.get("requests_running").and_then(Value::as_i64),
        waiting: num(&gauges, "requests_waiting"),
        kv_pct: None,
        last,
        last_t_ms: newest.map(|r| num(r, "t_ms")),
        counters,
    })
}

/// llama.cpp's `/slots`: one row per slot that holds a conversation. Without
/// `LLAMA_SERVER_SLOTS_DEBUG` the server leaves the prompt text out.
pub(crate) fn parse_llama_slots(body: &Value) -> Option<Reading> {
    let slots = body.as_array()?;
    if slots.iter().any(|s| s.get("is_processing").is_none()) {
        return None;
    }
    let sessions: Vec<ServerSession> = slots
        .iter()
        .filter(|s| s.get("id_task").is_some())
        .map(|s| {
            let prompt = num(s, "n_prompt_tokens");
            let processed = num(s, "n_prompt_tokens_processed");
            let cache = num(s, "n_prompt_tokens_cache").min(prompt);
            let next = s
                .get("next_token")
                .and_then(|n| n.as_array().and_then(|a| a.first()).or(Some(n)))
                .cloned()
                .unwrap_or(Value::Null);
            let decoded = num(&next, "n_decoded");
            let remain = next.get("n_remain").and_then(Value::as_i64);
            let processing = s.get("is_processing").and_then(Value::as_bool) == Some(true);
            let phase = match (processing, decoded) {
                (true, 0) => Phase::Prefill,
                (true, _) => Phase::Decode,
                (false, _) => Phase::Saved,
            };
            let to_read = (prompt - cache).max(0);
            ServerSession {
                id: s.get("id").and_then(Value::as_i64),
                model: format!("slot {}", num(s, "id")),
                prefill_pct: (phase == Phase::Prefill && to_read > 0)
                    .then(|| (100.0 * processed as f64 / to_read as f64).min(100.0)),
                phase,
                context: prompt + decoded,
                window: num(s, "n_ctx"),
                cached: cache,
                generated: decoded,
                max_tokens: remain.filter(|r| *r >= 0).map(|r| decoded + r),
                elapsed_secs: None,
                state_bytes: None,
                rate: None,
            }
        })
        .collect();
    Some(Reading {
        running: Some(slots
            .iter()
            .filter(|s| s.get("is_processing").and_then(Value::as_bool) == Some(true))
            .count() as i64),
        sessions,
        ..Reading::default()
    })
}

/// Prometheus text exposition, summed per metric name (labels collapsed).
pub(crate) fn parse_prometheus(text: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, rest) = match line.find(['{', ' ']) {
            Some(i) => (&line[..i], &line[i..]),
            None => continue,
        };
        let value_part = match rest.rfind('}') {
            Some(i) if rest.starts_with('{') => &rest[i + 1..],
            _ => rest,
        };
        let Some(value) = value_part
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
        else {
            continue;
        };
        if value.is_finite() {
            *out.entry(name.to_string()).or_insert(0.0) += value;
        }
    }
    out
}

fn first(metrics: &HashMap<String, f64>, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|name| metrics.get(*name).copied())
}

fn prometheus_engine(metrics: &HashMap<String, f64>) -> &'static str {
    let has = |prefix: &str| metrics.keys().any(|k| k.starts_with(prefix));
    if has("llamacpp:") {
        "llama.cpp"
    } else if has("vllm:") {
        "vllm"
    } else if has("sglang:") {
        "sglang"
    } else if has("tgi_") {
        "tgi"
    } else {
        "prometheus"
    }
}

/// What a Prometheus `/metrics` says about load, cache use and token counters.
pub(crate) fn reading_from_prometheus(metrics: &HashMap<String, f64>) -> Reading {
    let prompt = first(
        metrics,
        &[
            "llamacpp:prompt_tokens_total",
            "vllm:prompt_tokens_total",
            "sglang:prompt_tokens_total",
            "tgi_request_input_length_sum",
        ],
    );
    let generated = first(
        metrics,
        &[
            "llamacpp:tokens_predicted_total",
            "vllm:generation_tokens_total",
            "sglang:generation_tokens_total",
            "tgi_request_generated_tokens_sum",
        ],
    );
    let avg_prompt = first(metrics, &["llamacpp:prompt_tokens_seconds"]).filter(|v| *v > 0.0);
    let avg_gen = first(metrics, &["llamacpp:predicted_tokens_seconds"]).filter(|v| *v > 0.0);
    Reading {
        running: first(
            metrics,
            &[
                "llamacpp:requests_processing",
                "vllm:num_requests_running",
                "sglang:num_running_reqs",
                "tgi_batch_current_size",
            ],
        )
        .map(|v| v as i64),
        waiting: first(
            metrics,
            &[
                "llamacpp:requests_deferred",
                "vllm:num_requests_waiting",
                "sglang:num_queue_reqs",
                "tgi_queue_size",
            ],
        )
        .map_or(0, |v| v as i64),
        kv_pct: first(
            metrics,
            &[
                "vllm:kv_cache_usage_perc",
                "vllm:gpu_cache_usage_perc",
                "sglang:token_usage",
                "llamacpp:kv_cache_usage_ratio",
            ],
        )
        .map(|v| (v * 100.0).clamp(0.0, 100.0)),
        last: (avg_prompt.is_some() || avg_gen.is_some()).then_some(ServerRequest {
            label: "avg",
            prefill_tps: avg_prompt,
            decode_tps: avg_gen,
        }),
        counters: prompt.zip(generated),
        ..Reading::default()
    }
}

/// Only servers on this machine or the local/tailnet network are asked for metrics.
pub(crate) fn metrics_origin(base_url: &str) -> Option<(String, String)> {
    let url = url::Url::parse(base_url).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.trim_matches(['[', ']']).to_string();
    let local = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => {
            host == "localhost"
                || !host.contains('.')
                || [".local", ".lan", ".internal", ".ts.net", ".home.arpa"]
                    .iter()
                    .any(|suffix| host.ends_with(suffix))
        }
    };
    if !local {
        return None;
    }
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let shown = if host.contains(':') { format!("[{host}]") } else { host.clone() };
    Some((format!("{}://{shown}{port}", url.scheme()), host))
}

#[derive(Default)]
struct PollState {
    stats: Option<ServerStats>,
    last_wanted: Option<Instant>,
    working: bool,
}

struct Poller {
    state: Mutex<PollState>,
}

fn pollers() -> &'static Mutex<HashMap<String, Arc<Poller>>> {
    static POLLERS: OnceLock<Mutex<HashMap<String, Arc<Poller>>>> = OnceLock::new();
    POLLERS.get_or_init(Default::default)
}

/// Latest server metrics for the provider at `base_url`, starting a background
/// poller on first use. The poller stops by itself once nobody asks for 30 s.
pub(crate) fn server_stats(
    base_url: Option<&str>,
    working: bool,
    frames: &FrameRequester,
) -> Option<ServerStats> {
    let (origin, host) = metrics_origin(base_url?)?;
    let poller = {
        let mut map = pollers().lock().ok()?;
        if let Some(existing) = map.get(&origin) {
            existing.clone()
        } else {
            let handle = tokio::runtime::Handle::try_current().ok()?;
            let poller = Arc::new(Poller {
                state: Mutex::new(PollState::default()),
            });
            map.insert(origin.clone(), poller.clone());
            handle.spawn(poll(origin, host, poller.clone(), frames.clone()));
            poller
        }
    };
    let mut state = poller.state.lock().ok()?;
    state.last_wanted = Some(Instant::now());
    state.working = working;
    state.stats.clone()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Engine {
    LocalMetrics,
    /// `/slots`, plus `/metrics` when the server was started with `--metrics`.
    LlamaSlots { prometheus: bool },
    Prometheus,
}

async fn get_text(client: &reqwest::Client, url: &str) -> Option<String> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.text().await.ok()
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    serde_json::from_str(&get_text(client, url).await?).ok()
}

fn is_prometheus(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("# TYPE "))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

async fn detect(client: &reqwest::Client, origin: &str) -> Option<Engine> {
    // `since=0` would return the whole history (hundreds of KB): ask only for now.
    if let Some(body) = get_json(client, &format!("{origin}/metrics.json?since={}", now_ms())).await
        && parse_local_metrics(&body).is_some()
    {
        return Some(Engine::LocalMetrics);
    }
    let metrics = get_text(client, &format!("{origin}/metrics"))
        .await
        .is_some_and(|t| is_prometheus(&t));
    if let Some(body) = get_json(client, &format!("{origin}/slots")).await
        && parse_llama_slots(&body).is_some()
    {
        return Some(Engine::LlamaSlots { prometheus: metrics });
    }
    metrics.then_some(Engine::Prometheus)
}

async fn read(
    client: &reqwest::Client,
    origin: &str,
    engine: Engine,
    since_ms: i64,
) -> Option<(&'static str, Reading)> {
    match engine {
        Engine::LocalMetrics => {
            let body = get_json(client, &format!("{origin}/metrics.json?since={since_ms}")).await?;
            Some(("local-metrics", parse_local_metrics(&body)?))
        }
        Engine::LlamaSlots { prometheus } => {
            let body = get_json(client, &format!("{origin}/slots")).await?;
            let mut reading = parse_llama_slots(&body)?;
            if prometheus
                && let Some(text) = get_text(client, &format!("{origin}/metrics")).await
            {
                let extra = reading_from_prometheus(&parse_prometheus(&text));
                reading.waiting = extra.waiting;
                reading.kv_pct = extra.kv_pct;
                reading.last = extra.last;
                reading.counters = extra.counters;
            }
            Some(("llama.cpp", reading))
        }
        Engine::Prometheus => {
            let metrics = parse_prometheus(&get_text(client, &format!("{origin}/metrics")).await?);
            Some((prometheus_engine(&metrics), reading_from_prometheus(&metrics)))
        }
    }
}

async fn poll(origin: String, host: String, poller: Arc<Poller>, frames: FrameRequester) {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
    else {
        return;
    };
    let mut since_ms: i64 = now_ms().saturating_sub(15 * 60 * 1000);
    let mut engine: Option<Engine> = None;
    let mut last_request: Option<ServerRequest> = None;
    let mut previous_gen: HashMap<i64, (i64, Instant)> = HashMap::new();
    let mut previous_counters: Option<((f64, f64), Instant)> = None;
    loop {
        let (idle, working) = match poller.state.lock() {
            Ok(state) => (
                state
                    .last_wanted
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(30)),
                state.working,
            ),
            Err(_) => (true, false),
        };
        if idle {
            break;
        }
        if engine.is_none() {
            engine = detect(&client, &origin).await;
        }
        let reading = match engine {
            Some(found) => read(&client, &origin, found, since_ms).await,
            None => None,
        };
        let Some((name, mut reading)) = reading else {
            // Nothing that publishes metrics here (or it went down): look again later.
            engine = None;
            previous_counters = None;
            if let Ok(mut state) = poller.state.lock()
                && state.stats.take().is_some()
            {
                frames.schedule_frame();
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        };
        let now = Instant::now();
        if let Some(t_ms) = reading.last_t_ms {
            since_ms = since_ms.max(t_ms);
        }
        if reading.last.is_some() {
            last_request = reading.last.clone();
        }
        let mut seen = HashMap::new();
        for session in &mut reading.sessions {
            if session.phase != Phase::Decode {
                continue;
            }
            let Some(id) = session.id else { continue };
            if let Some((prev, t)) = previous_gen.get(&id) {
                let secs = now.saturating_duration_since(*t).as_secs_f64();
                if secs > 0.2 && session.generated >= *prev {
                    session.rate = Some((session.generated - prev) as f64 / secs);
                }
            }
            seen.insert(id, (session.generated, now));
        }
        previous_gen = seen;
        let throughput = match (reading.counters, previous_counters) {
            (Some((prompt, generated)), Some(((p0, g0), t0))) => {
                let secs = now.saturating_duration_since(t0).as_secs_f64();
                (secs > 0.2 && prompt >= p0 && generated >= g0).then(|| Throughput {
                    prompt_tps: (prompt - p0) / secs,
                    gen_tps: (generated - g0) / secs,
                })
            }
            _ => None,
        };
        previous_counters = reading.counters.map(|c| (c, now));
        let stats = ServerStats {
            host: host.clone(),
            engine: name,
            sessions: reading.sessions,
            running: reading.running,
            waiting: reading.waiting,
            kv_pct: reading.kv_pct,
            now: throughput.filter(|t| t.prompt_tps > 0.0 || t.gen_tps > 0.0),
            last: last_request.clone(),
        };
        let changed = match poller.state.lock() {
            Ok(mut state) => {
                let changed = state.stats.as_ref() != Some(&stats);
                state.stats = Some(stats);
                changed
            }
            Err(_) => false,
        };
        if changed {
            frames.schedule_frame();
        }
        let pause = if working { 1000 } else { 4000 };
        tokio::time::sleep(Duration::from_millis(pause)).await;
    }
    if let Ok(mut map) = pollers().lock() {
        map.remove(&origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn meter_measures_first_token_and_generation() {
        let t0 = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.turn_started(t0);
        assert!(matches!(
            meter.view(t0 + Duration::from_secs(2), true).live,
            Some(LiveSpeed::Reading { .. })
        ));
        meter.output(t0 + Duration::from_secs(2), 40);
        meter.output(t0 + Duration::from_secs(4), 400);
        // Usage arrives after a 60 s tool: it must not slow the generation figure.
        meter.reply_finished(t0 + Duration::from_secs(64), 1, 10_000, 8_000, 200);
        let last = meter.view(t0 + Duration::from_secs(64), true).last.expect("sample");
        assert_eq!(last.new_tokens, 2_000);
        assert_eq!(last.prefill_tps, Some(1_000.0));
        assert_eq!(last.decode_tps, Some(100.0));
        assert_eq!(last.ttft_secs, 2.0);
    }

    #[test]
    fn next_request_starts_after_the_last_tool() {
        let t0 = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.turn_started(t0);
        meter.output(t0 + Duration::from_secs(1), 10);
        meter.reply_finished(t0 + Duration::from_secs(2), 1, 100, 0, 10);
        // A tool runs for 10 s; the second request starts when it returns.
        meter.tool_completed(t0 + Duration::from_secs(12));
        meter.output(t0 + Duration::from_secs(13), 10);
        meter.output(t0 + Duration::from_secs(14), 10);
        meter.reply_finished(t0 + Duration::from_secs(14), 2, 1_100, 100, 50);
        let last = meter.view(t0 + Duration::from_secs(14), false).last.expect("sample");
        assert_eq!(last.ttft_secs, 1.0);
        assert_eq!(last.prefill_tps, Some(1_000.0));
        assert_eq!(last.decode_tps, Some(50.0));
    }

    #[test]
    fn reply_without_streamed_output_keeps_previous_sample() {
        let t0 = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.turn_started(t0);
        meter.output(t0 + Duration::from_secs(1), 10);
        meter.reply_finished(t0 + Duration::from_secs(2), 1, 100, 0, 10);
        meter.reply_finished(t0 + Duration::from_secs(3), 2, 200, 0, 5);
        // The same report again adds no sample but still closes the request.
        meter.output(t0 + Duration::from_secs(4), 10);
        meter.reply_finished(t0 + Duration::from_secs(5), 2, 200, 0, 5);
        assert!(matches!(
            meter.view(t0 + Duration::from_secs(6), true).live,
            Some(LiveSpeed::Reading { .. })
        ));
        let last = meter.view(t0 + Duration::from_secs(3), false).last.expect("sample");
        assert_eq!(last.output, 10);
        meter.turn_finished();
        assert_eq!(meter.view(t0, true).live, None);
    }

    #[test]
    fn parses_local_metrics() {
        let body = serde_json::json!({
            "counters": {"prefill_tokens_total": 500, "generation_tokens_total": 90},
            "gauges": {"prefill_tokens_live": 300, "prefill_tokens_expected": 1200, "requests_waiting": 1, "requests_running": 1},
            "sessions": [
                {"model": "qwen-coder", "request_id": 381, "phase": "prefill",
                 "context_tokens": 1228, "context_length": 1048576, "cached_tokens": 28,
                 "generated_tokens": 0, "max_tokens": 400, "elapsed_seconds": 1.5, "state_bytes": 0},
                {"model": "mlx/Qwen", "request_id": null, "phase": "cached",
                 "context_tokens": 53000, "context_length": 1048576, "cached_tokens": 53000,
                 "generated_tokens": 0, "elapsed_seconds": 0, "state_bytes": 1200000000}
            ],
            "requests": [
                {"request_id": 380, "t_ms": 1_794_358_283_814_i64, "prompt_tokens": 87, "cached_tokens": 53645,
                 "prefill_seconds": 0.5, "generated_tokens": 87, "decode_seconds": 0.87}
            ]
        });
        let reading = parse_local_metrics(&body).expect("local-metrics format");
        assert_eq!(reading.waiting, 1);
        assert_eq!(reading.running, Some(1));
        assert_eq!(reading.sessions[0].phase, Phase::Prefill);
        assert_eq!(reading.sessions[0].prefill_pct, Some(25.0));
        assert_eq!(reading.sessions[1].phase, Phase::Saved);
        assert_eq!(reading.sessions[1].model, "Qwen");
        assert_eq!(reading.sessions[1].id, None);
        assert_eq!(reading.last_t_ms, Some(1794358283814));
        assert_eq!(reading.counters, Some((500.0, 90.0)));
        let last = reading.last.expect("last request");
        assert_eq!(last.prefill_tps, Some(174.0));
        assert_eq!(last.decode_tps, Some(100.0));
        assert!(parse_local_metrics(&serde_json::json!({"status": "ok"})).is_none());
    }

    #[test]
    fn parses_llama_cpp_slots() {
        // Shape of llama.cpp tools/server `server_slot::to_json(only_metrics = true)`.
        let body = serde_json::json!([
            {"id": 0, "n_ctx": 32768, "speculative": false, "is_processing": true, "id_task": 7,
             "n_prompt_tokens": 9000, "n_prompt_tokens_processed": 1000, "n_prompt_tokens_cache": 7000,
             "params": {}, "next_token": [{"has_next_token": true, "has_new_line": false, "n_remain": -1, "n_decoded": 0}]},
            {"id": 1, "n_ctx": 32768, "speculative": false, "is_processing": true, "id_task": 8,
             "n_prompt_tokens": 500, "n_prompt_tokens_processed": 500, "n_prompt_tokens_cache": 0,
             "params": {}, "next_token": [{"has_next_token": true, "has_new_line": false, "n_remain": 100, "n_decoded": 300}]},
            {"id": 2, "n_ctx": 32768, "speculative": false, "is_processing": false},
            {"id": 3, "n_ctx": 32768, "speculative": false, "is_processing": false, "id_task": 2,
             "n_prompt_tokens": 4000, "n_prompt_tokens_processed": 10, "n_prompt_tokens_cache": 3990,
             "params": {}, "next_token": [{"has_next_token": false, "has_new_line": false, "n_remain": 0, "n_decoded": 50}]}
        ]);
        let reading = parse_llama_slots(&body).expect("llama.cpp slots");
        assert_eq!(reading.running, Some(2));
        assert_eq!(reading.sessions.len(), 3, "the empty slot is skipped");
        let prefill = &reading.sessions[0];
        assert_eq!(prefill.phase, Phase::Prefill);
        assert_eq!(prefill.prefill_pct, Some(50.0));
        assert_eq!((prefill.context, prefill.cached, prefill.window), (9000, 7000, 32768));
        let decode = &reading.sessions[1];
        assert_eq!(decode.phase, Phase::Decode);
        assert_eq!((decode.generated, decode.max_tokens), (300, Some(400)));
        assert_eq!(reading.sessions[2].phase, Phase::Saved);
        assert!(parse_llama_slots(&serde_json::json!([{"id": 0}])).is_none());
        assert!(parse_llama_slots(&serde_json::json!({"sessions": []})).is_none());
    }

    #[test]
    fn reads_prometheus_from_any_engine() {
        let llama = "# HELP llamacpp:prompt_tokens_total x\n# TYPE llamacpp:prompt_tokens_total counter\n\
llamacpp:prompt_tokens_total 1200\nllamacpp:tokens_predicted_total 300\n\
llamacpp:prompt_tokens_seconds 650.5\nllamacpp:predicted_tokens_seconds 41.2\n\
llamacpp:requests_processing 1\nllamacpp:requests_deferred 2\n";
        let metrics = parse_prometheus(llama);
        assert_eq!(prometheus_engine(&metrics), "llama.cpp");
        let reading = reading_from_prometheus(&metrics);
        assert_eq!((reading.running, reading.waiting), (Some(1), 2));
        assert_eq!(reading.counters, Some((1200.0, 300.0)));
        let avg = reading.last.expect("averages");
        assert_eq!((avg.label, avg.prefill_tps, avg.decode_tps), ("avg", Some(650.5), Some(41.2)));

        let vllm = "# TYPE vllm:num_requests_running gauge\n\
vllm:num_requests_running{model_name=\"a\"} 2\nvllm:num_requests_running{model_name=\"b\"} 1\n\
vllm:num_requests_waiting{model_name=\"a\"} 0\nvllm:kv_cache_usage_perc{model_name=\"a\"} 0.25\n\
vllm:prompt_tokens_total{model_name=\"a\"} 1e4\nvllm:generation_tokens_total{model_name=\"a\"} 500\n";
        let metrics = parse_prometheus(vllm);
        assert_eq!(prometheus_engine(&metrics), "vllm");
        let reading = reading_from_prometheus(&metrics);
        assert_eq!(reading.running, Some(3), "labels are summed");
        assert_eq!(reading.kv_pct, Some(25.0));
        assert_eq!(reading.counters, Some((10_000.0, 500.0)));
        assert!(reading.last.is_none());
        assert!(is_prometheus(vllm));
        assert!(!is_prometheus("{\"status\":\"ok\"}"));
    }

    #[test]
    fn probes_only_local_hosts() {
        assert_eq!(
            metrics_origin("http://example-host:8086/v1"),
            Some(("http://example-host:8086".to_string(), "example-host".to_string()))
        );
        assert!(metrics_origin("http://127.0.0.1:8093/v1").is_some());
        assert!(metrics_origin("http://100.101.1.2:8086/v1").is_some());
        assert!(metrics_origin("http://192.168.1.20:8086/v1").is_some());
        assert!(metrics_origin("https://api.novita.ai/openai").is_none());
        assert!(metrics_origin("https://us-aiplatform.googleapis.com/v1").is_none());
        assert!(metrics_origin("http://8.8.8.8/v1").is_none());
    }
}
