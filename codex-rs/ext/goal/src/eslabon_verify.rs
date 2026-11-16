//! eslabon: goals that only complete on evidence ("loop on evidence, not on confidence").
//!
//! A user-written goal may end with `[verify: <command>]`, e.g.
//! `/goal Fix the parser [verify: cargo test -p parser]`. When the model calls
//! `update_goal(status="complete")`, the command runs in the turn's working
//! directory; a non-zero exit rejects the completion, hands the failure back to the
//! model and keeps the goal active. After [`MAX_FAILED_VERIFICATIONS`] failures the
//! goal is marked blocked for the user to look at.
//!
//! Only the user can attach a verifier: `create_goal` (model-authored objectives)
//! rejects the marker, so the model never chooses a command that runs on its own.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

pub(crate) const VERIFY_MARKER: &str = "[verify:";
pub(crate) const MAX_FAILED_VERIFICATIONS: u32 = 5;
const VERIFY_TIMEOUT: Duration = Duration::from_secs(600);
const OUTPUT_TAIL_LINES: usize = 40;

/// The command of a trailing `[verify: ...]` marker. The marker must end the
/// objective, so the command itself may contain brackets (`[ -f out ] && make`).
pub(crate) fn extract_verifier(objective: &str) -> Option<String> {
    let trimmed = objective.trim_end();
    let start = trimmed.rfind(VERIFY_MARKER)?;
    let inner = trimmed[start + VERIFY_MARKER.len()..].strip_suffix(']')?;
    let command = inner.trim();
    (!command.is_empty()).then(|| command.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifyOutcome {
    pub(crate) command: String,
    /// `None` when the command could not run or timed out.
    pub(crate) exit_code: Option<i32>,
    pub(crate) output_tail: String,
    pub(crate) elapsed: Duration,
}

impl VerifyOutcome {
    pub(crate) fn passed(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub(crate) fn summary(&self) -> String {
        let status = match self.exit_code {
            Some(code) => format!("exited {code}"),
            None => "did not finish".to_string(),
        };
        format!(
            "`{}` {status} after {:.1}s",
            self.command,
            self.elapsed.as_secs_f64()
        )
    }
}

/// Output kept in memory while the verifier runs; the rest is discarded.
const OUTPUT_CAP_BYTES: usize = 256 * 1024;

pub(crate) async fn run_verifier(command: &str, cwd: &Path) -> VerifyOutcome {
    use tokio::io::AsyncReadExt;
    let started = Instant::now();
    let mut builder = tokio::process::Command::new("sh");
    builder
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so a timeout also stops background children (`make test &`).
    #[cfg(unix)]
    builder.process_group(0);
    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            return VerifyOutcome {
                command: command.to_string(),
                exit_code: None,
                output_tail: format!("failed to start: {err}"),
                elapsed: started.elapsed(),
            };
        }
    };
    let pid = child.id();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // Bounded reads: keep the first OUTPUT_CAP_BYTES of each stream, drain the rest.
    async fn read_capped(stream: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
        let Some(mut stream) = stream else {
            return Vec::new();
        };
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = OUTPUT_CAP_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        kept
    }
    let run = async {
        let (out, err) = tokio::join!(read_capped(stdout.take()), read_capped(stderr.take()));
        let status = child.wait().await;
        (status, out, err)
    };
    let (exit_code, output_tail) = match tokio::time::timeout(VERIFY_TIMEOUT, run).await {
        Ok((Ok(status), out, err)) => {
            let mut text = String::from_utf8_lossy(&out).into_owned();
            text.push_str(&String::from_utf8_lossy(&err));
            (status.code().or(Some(-1)), tail(&text))
        }
        Ok((Err(err), _, _)) => (None, format!("failed to wait: {err}")),
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
                // SAFETY: signalling our own child's process group.
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
            (
                None,
                format!("timed out after {}s", VERIFY_TIMEOUT.as_secs()),
            )
        }
    };
    VerifyOutcome {
        command: command.to_string(),
        exit_code,
        output_tail,
        elapsed: started.elapsed(),
    }
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    // A single huge line is clipped too.
    lines[start..]
        .iter()
        .map(|line| {
            if line.len() > 500 {
                let cut = line.floor_char_boundary(500);
                format!("{}…", &line[..cut])
            } else {
                (*line).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Failed verifications per goal id, for this process.
static FAILURES: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

pub(crate) fn record_failure(goal_id: &str) -> u32 {
    let mut failures = FAILURES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = failures.entry(goal_id.to_string()).or_default();
    *count += 1;
    *count
}

pub(crate) fn clear_failures(goal_id: &str) {
    FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(goal_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_a_trailing_marker_only() {
        assert_eq!(
            extract_verifier("Fix the parser [verify: cargo test -p parser]"),
            Some("cargo test -p parser".to_string())
        );
        assert_eq!(
            extract_verifier("Ship it [verify: [ -f out.bin ] && make check]  "),
            Some("[ -f out.bin ] && make check".to_string())
        );
        assert_eq!(extract_verifier("No verifier here"), None);
        assert_eq!(extract_verifier("Empty [verify: ]"), None);
        assert_eq!(
            extract_verifier("[verify: make] then keep talking"),
            None,
            "the marker must end the objective"
        );
    }

    #[tokio::test]
    async fn runs_in_the_working_directory_and_reports_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.txt"), "ready").expect("write");
        let pass = run_verifier("test -f ok.txt && echo good", dir.path()).await;
        assert!(pass.passed(), "{pass:?}");
        assert_eq!(pass.output_tail, "good");

        let fail = run_verifier("echo boom >&2; exit 3", dir.path()).await;
        assert!(!fail.passed());
        assert_eq!(fail.exit_code, Some(3));
        assert_eq!(fail.output_tail, "boom");
        assert!(fail.summary().starts_with("`echo boom >&2; exit 3` exited 3 after"));

        // Endless output is capped instead of filling memory.
        let flood = run_verifier("yes | head -c 5000000; exit 1", dir.path()).await;
        assert_eq!(flood.exit_code, Some(1));
        assert!(flood.output_tail.len() < 64 * 1024, "{}", flood.output_tail.len());
    }

    #[test]
    fn failures_are_counted_per_goal() {
        clear_failures("goal-a");
        assert_eq!(record_failure("goal-a"), 1);
        assert_eq!(record_failure("goal-a"), 2);
        assert_eq!(record_failure("goal-b"), 1);
        clear_failures("goal-a");
        assert_eq!(record_failure("goal-a"), 1);
    }
}
