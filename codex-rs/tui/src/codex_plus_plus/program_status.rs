//! Negotiated root Program Status Protocol (OSC 7501), following Pi's interactive reporting.
//! https://www.superlogical.com/rex/docs/build/program-status

use base64::Engine;
use std::io::Write;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

const QUERY: &[u8] = b"\x1b]7501;?\x1b\\\x1b[c";
const PREFIX: &[u8] = b"\x1b]7501;?";
const MAX_MESSAGE_BYTES: usize = 2048;
static SUPPORTED: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum State {
    #[default]
    Idle,
    Working,
    Done,
    Error,
    Blocked,
    Clear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Permission,
    Question,
    Auth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Status {
    pub(crate) state: State,
    pub(crate) kind: Option<Kind>,
    pub(crate) message: Option<String>,
}

impl Status {
    pub(crate) fn encode(&self) -> String {
        let state = match self.state {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Error => "error",
            State::Blocked => "blocked",
            State::Clear => "clear",
        };
        let mut body = format!("state={state}:app=codex");
        if self.state == State::Blocked
            && let Some(kind) = self.kind
        {
            let kind = match kind {
                Kind::Permission => "permission",
                Kind::Question => "question",
                Kind::Auth => "auth",
            };
            body.push_str(&format!(":kind={kind}"));
        }
        if let Some(message) = &self.message {
            let mut line = String::new();
            for ch in message.chars() {
                let ch = if ch.is_control() { ' ' } else { ch };
                if line.len() + ch.len_utf8() > MAX_MESSAGE_BYTES {
                    break;
                }
                line.push(ch);
            }
            let line = line.trim();
            if !line.is_empty() {
                body.push_str(":msg=");
                body.push_str(&base64::engine::general_purpose::STANDARD.encode(line));
            }
        }
        format!("\x1b]7501;{body}\x1b\\")
    }
}

#[derive(Default)]
pub(crate) struct Reporter {
    pub(crate) resting: State,
    last_report: Option<(u64, String)>,
}

impl Reporter {
    pub(crate) fn publish(&mut self, status: Status) {
        let override_value = std::env::var("CODEX_PROGRAM_STATUS").ok();
        let enabled = match override_value.as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => SUPPORTED.load(Ordering::Relaxed),
        };
        if enabled {
            let generation = GENERATION.load(Ordering::Relaxed);
            let mut writer = std::io::stdout().lock();
            if let Err(error) = self.write(&mut writer, generation, status) {
                tracing::debug!("program status report failed: {error}");
            }
        }
    }

    fn write(
        &mut self,
        writer: &mut impl Write,
        generation: u64,
        status: Status,
    ) -> std::io::Result<()> {
        let report = (generation, status.encode());
        if self.last_report.as_ref() != Some(&report) {
            writer.write_all(report.1.as_bytes())?;
            writer.flush()?;
            self.last_report = Some(report);
        }
        Ok(())
    }
}

/// Clear while handing the terminal to the shell/editor; the next frame republishes its state.
pub(crate) fn clear() {
    Reporter::default().publish(Status {
        state: State::Clear,
        kind: None,
        message: None,
    });
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

fn auto_probe() -> bool {
    !matches!(
        std::env::var("CODEX_PROGRAM_STATUS").as_deref(),
        Ok("1" | "0")
    ) && codex_terminal_detection::terminal_info()
        .multiplexer
        .is_none()
}

/// Share the existing startup deadline and input replay; never probe through tmux or screen.
pub(crate) fn probe_query() -> &'static [u8] {
    if auto_probe() { QUERY } else { b"" }
}

pub(crate) fn probe_complete(input: &[u8]) -> bool {
    !auto_probe() || sentinel_received(input)
}

pub(crate) fn finish_probe(input: &[u8]) {
    if auto_probe() {
        SUPPORTED.store(detected_support(input) == Some(true), Ordering::Relaxed);
    }
}

// Even after support is confirmed, consume the query's trailing DA before returning input.
fn sentinel_received(input: &[u8]) -> bool {
    response_ranges(input)
        .iter()
        .any(|range| !input[range.clone()].starts_with(PREFIX))
}

fn detected_support(input: &[u8]) -> Option<bool> {
    response_ranges(input)
        .first()
        .map(|range| input[range.clone()].starts_with(PREFIX))
}

/// Replies and the DA sentinel outside bracketed paste, for native Windows record replay.
pub(crate) fn response_ranges(input: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < input.len() {
        let rest = &input[cursor..];
        if rest.starts_with(b"\x1b[200~") {
            let Some(end) = rest
                .windows(/*size*/ 6)
                .position(|bytes| bytes == b"\x1b[201~")
            else {
                break;
            };
            cursor += end + 6;
            continue;
        }
        let end = if rest.starts_with(PREFIX) {
            let bounded = &rest[PREFIX.len()..rest.len().min(/*other*/ 4096)];
            bounded
                .iter()
                .position(|byte| matches!(*byte, 7 | 27))
                .and_then(|end| {
                    let end = PREFIX.len() + end;
                    if rest[end] == 7 {
                        Some(end + 1)
                    } else if rest.get(end..end + 2) == Some(b"\x1b\\") {
                        Some(end + 2)
                    } else {
                        None
                    }
                })
                .filter(|end| *end <= 4096)
        } else if rest.starts_with(b"\x1b[?") {
            rest.iter()
                .enumerate()
                .skip(/*n*/ 3)
                .take(/*n*/ 64)
                .find(|(_, byte)| !byte.is_ascii_digit() && **byte != b';')
                .and_then(|(i, byte)| (*byte == b'c').then_some(i + 1))
        } else {
            None
        };
        if let Some(end) = end {
            ranges.push(cursor..cursor + end);
            cursor += end;
        } else {
            cursor += 1;
        }
    }
    ranges
}

#[cfg(test)]
#[path = "program_status_tests.rs"]
mod tests;
