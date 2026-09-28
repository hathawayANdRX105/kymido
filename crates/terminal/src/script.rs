//! script.rs — the declarative scenario runner (T0c).
//!
//! A scenario is a JSON document: a command to drive, a terminal geometry and
//! a list of steps. [`run_scenario`] spawns that command on a real pty —
//! through the existing [`TerminalRegistry`], because process management is
//! already there and re-implementing it would create a second truth — then
//! replays every byte the child produces through a [`Screen`].
//!
//! # Reverse queries must go back to the pty
//!
//! After **every** step the runner drains the child's output into the screen
//! and writes [`Screen::feed`]'s return value back to the pty. That return
//! value is the emulator's answers to the child's probes (`ESC[6n`, device
//! attributes, colour and text-area queries). A real terminal answers
//! automatically, so a program that probes the terminal hangs or silently
//! degrades when the answers never come back.
//!
//! # Reading the results
//!
//! [`RunReport`] holds one [`StepOutcome`] per step, and a failed step always
//! carries a note. A `wait_for_text` timeout names the ASCII snapshot taken at
//! that moment, so whoever reads the failure can see what the screen actually
//! looked like instead of guessing from the step list alone.
//!
//! # Artifacts
//!
//! `<artifact_dir>/<scenario>/` holds a `<NNN>-<name>.txt|.svg|.json` triple
//! per `screenshot` step, an ASCII snapshot per failed check, and
//! `report.json` — the serialized [`RunReport`]. Files go straight to their
//! final name; nothing temporary is left behind.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::screen::{Screen, render_ascii, render_json, render_svg};
use crate::{TerminalError, TerminalId, TerminalRegistry};

/// How long one `wait_for_text` poll waits for the child to produce
/// something before the screen is re-checked. The wait is a settle-wait, not
/// a sleep: the same discipline as the PTY smoke in `.github/workflows/ci.yml`
/// ("sentinels matched after a settle-wait (no bare sleeps guessing at
/// frames)").
const POLL_MS: u64 = 25;

/// Why a run could not be carried through. Both variants carry a note: a
/// failed step must never be a bare status.
#[derive(Debug)]
enum Failure {
    /// The child is gone. The step that noticed it failed, every later step
    /// is skipped, and the exit code is spelled out in the note.
    Child(String),
    /// The step itself could not be performed.
    Step(String),
}

// -----------------------------------------------------------------------------
// Scenario
// -----------------------------------------------------------------------------

/// One mouse button of a `mouse` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

impl MouseButton {
    /// SGR mouse press followed by release, xterm-style: `CSI < btn ; col ; row
    /// M` then the same with button 3 and the `m` final byte. Coordinates are
    /// 1-indexed here, as the protocol demands, while a step's `col`/`row` are
    /// screen coordinates.
    fn sequence(&self, col: u16, row: u16) -> String {
        let button = match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        };
        let (c, r) = (col.saturating_add(1), row.saturating_add(1));
        format!("\x1b[<{button};{c};{r}M\x1b[<3;{c};{r}m")
    }
}

/// The pty geometry the scenario drives the child at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalConfig {
    pub rows: u16,
    pub cols: u16,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig { rows: 24, cols: 80 }
    }
}

/// One step of a scenario. The JSON tag is the action name
/// (`send_keys`, `wait_for_text`, …), matching the `open`/`send_keys`/
/// `snapshot` shape adopted from tuiwright so a future swap keeps the script
/// files readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Step {
    /// Named keys — `Down`, `Up`, `Enter`, `Escape`, `C-c`, `PageUp` — or
    /// literal single characters. Several may be given in one string
    /// (`"C-c Down Enter"`); one unknown name fails the whole step.
    SendKeys {
        keys: String,
    },
    /// Literal text, written verbatim (a trailing `\n` submits a line).
    Type {
        text: String,
    },
    /// One press/release at a screen cell.
    Mouse {
        row: u16,
        col: u16,
        button: MouseButton,
    },
    /// Resize the pty and the screen together.
    Resize {
        cols: u16,
        rows: u16,
    },
    /// A real sleep. Boring on purpose: a step whose effect is not observable
    /// on the screen needs no more.
    Wait {
        ms: u64,
    },
    /// Poll until the text is on screen or the timeout expires. Never a bare
    /// sleep guessing at frames.
    WaitForText {
        text: String,
        timeout_ms: u64,
    },
    /// Write the frame triple (`.txt` / `.svg` / `.json`).
    Screenshot {
        name: Option<String>,
    },
    AssertText {
        contains: String,
    },
    AssertNotText {
        contains: String,
    },
}

impl Step {
    /// The action name, spelled exactly as the JSON tag. Kept beside the
    /// serde attributes on purpose: a rename there must be mirrored here or
    /// `report.json` will name actions that no step in the file has.
    pub fn action(&self) -> &'static str {
        match self {
            Step::SendKeys { .. } => "send_keys",
            Step::Type { .. } => "type",
            Step::Mouse { .. } => "mouse",
            Step::Resize { .. } => "resize",
            Step::Wait { .. } => "wait",
            Step::WaitForText { .. } => "wait_for_text",
            Step::Screenshot { .. } => "screenshot",
            Step::AssertText { .. } => "assert_text",
            Step::AssertNotText { .. } => "assert_not_text",
        }
    }
}

/// A whole scenario: what to drive, at what geometry, in what order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario name; also the artifact directory name.
    pub name: String,
    /// The command to run on the pty, split on whitespace exactly as
    /// [`TerminalRegistry::create`] splits its `shell` argument.
    pub command: String,
    /// Working directory for the child. Defaults to the runner's own cwd.
    pub cwd: Option<String>,
    pub terminal: TerminalConfig,
    pub steps: Vec<Step>,
}

impl Scenario {
    /// Parse a scenario from JSON.
    pub fn from_json_str(json: &str) -> Result<Scenario, ScriptError> {
        serde_json::from_str(json).map_err(|e| ScriptError::Scenario(e.to_string()))
    }

    /// Read and parse a scenario file.
    pub fn from_path(path: &Path) -> Result<Scenario, ScriptError> {
        let text = std::fs::read_to_string(path).map_err(|e| ScriptError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Scenario::from_json_str(&text)
    }
}

// -----------------------------------------------------------------------------
// Report
// -----------------------------------------------------------------------------

/// How one step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Ok,
    /// The step could not be carried out, or its check did not hold. The note
    /// says why — a `Failed` with an empty note is a harness bug.
    Failed,
    /// Not attempted: the run had already been aborted.
    Skipped,
}

/// How the run as a whole ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
}

/// What one step did and how it went. `step` is the 0-based index into
/// [`Scenario::steps`], the same index the `NNN-` artifact prefix uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub step: usize,
    pub action: String,
    pub status: StepStatus,
    pub note: String,
}

/// The result of a run. Written to `<artifact_dir>/<scenario>/report.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    pub scenario: String,
    pub status: RunStatus,
    pub steps: Vec<StepOutcome>,
}

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------

/// Everything that stops a run before it can produce a report.
///
/// A step that fails is **not** this type: it is a [`StepStatus::Failed`]
/// outcome, because a report that never gets written is exactly what a
/// failing scenario must not do.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("scenario is not valid: {0}")]
    Scenario(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pty refused to run the scenario: {0}")]
    Pty(String),
}

impl ScriptError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        ScriptError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

// -----------------------------------------------------------------------------
// Key names
// -----------------------------------------------------------------------------

/// Bytes for one named key, or `None` when the name is unknown.
///
/// An unknown name must never resolve to "nothing happens": a scenario that
/// skipped the key it could not spell would report green while driving
/// nothing at all.
fn named_key(name: &str) -> Option<Vec<u8>> {
    let named: &[(&str, &[u8])] = &[
        ("Enter", b"\r"),
        ("Tab", b"\t"),
        ("Escape", b"\x1b"),
        ("Backspace", b"\x7f"),
        ("Delete", b"\x1b[3~"),
        ("Insert", b"\x1b[2~"),
        ("Up", b"\x1b[A"),
        ("Down", b"\x1b[B"),
        ("Right", b"\x1b[C"),
        ("Left", b"\x1b[D"),
        ("Home", b"\x1b[H"),
        ("End", b"\x1b[F"),
        ("PageUp", b"\x1b[5~"),
        ("PageDown", b"\x1b[6~"),
        ("F1", b"\x1bOP"),
        ("F2", b"\x1bOQ"),
        ("F3", b"\x1bOR"),
        ("F4", b"\x1bOS"),
        ("F5", b"\x1b[15~"),
        ("F6", b"\x1b[17~"),
        ("F7", b"\x1b[31~"),
        ("F8", b"\x1b[32~"),
        ("F9", b"\x1b[33~"),
        ("F10", b"\x1b[34~"),
        ("F11", b"\x1b[23~"),
        ("F12", b"\x1b[24~"),
    ];
    if let Some((_, bytes)) = named.iter().find(|(n, _)| *n == name) {
        return Some(bytes.to_vec());
    }
    let lower = name.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix("c-"))
    {
        // A single ASCII letter, or the one C- key with a name (`C-Space`).
        let code = match rest {
            "space" => 0u8,
            "[" => 0x1b,
            "]" => 0x1d,
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => c as u8 & 0x1f,
                    _ => return None,
                }
            }
        };
        return Some(vec![code]);
    }
    if let Some(rest) = lower
        .strip_prefix("alt-")
        .or_else(|| lower.strip_prefix("m-"))
    {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let mut out = vec![0x1b];
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            return Some(out);
        }
        return None;
    }
    // A single printable character (any script — CJK keys are common) is
    // typed as itself.
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => {
            let mut out = Vec::new();
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            Some(out)
        }
        _ => None,
    }
}

/// Resolve a `send_keys` payload: whitespace-separated key names or literal
/// single characters. Any unknown name fails the whole step with a note naming
/// it.
fn encode_keys(keys: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for token in keys.split_whitespace() {
        match named_key(token) {
            Some(bytes) => out.extend_from_slice(&bytes),
            None => {
                return Err(format!(
                    "unknown key name {token:?} in send_keys {keys:?}; \
                     nothing was sent for this step"
                ));
            }
        }
    }
    if out.is_empty() {
        return Err(format!("send_keys {keys:?} named no keys"));
    }
    Ok(out)
}

// -----------------------------------------------------------------------------
// Runner
// -----------------------------------------------------------------------------

/// One screenshot step's artifacts; the `.txt` is kept because it is the
/// primary artifact an agent reads, with the `.svg` and `.json` siblings
/// next to it under the same stem.
struct Frame {
    txt: PathBuf,
}

/// A scenario in flight: the pty session, the screen fed from it and the
/// artifact directory.
struct Runner<'r> {
    registry: &'r TerminalRegistry,
    id: TerminalId,
    screen: Screen,
    dir: PathBuf,
}

impl<'r> Runner<'r> {
    /// Spawn the scenario's command and create the artifact directory.
    ///
    /// The artifact directory is created up front rather than on first write:
    /// a run that fails on its first step must still leave a directory whose
    /// `report.json` says why.
    fn start(
        registry: &'r TerminalRegistry,
        scenario: &Scenario,
        artifact_dir: &Path,
    ) -> Result<Self, ScriptError> {
        let id = registry
            .create(
                &scenario.command,
                scenario.cwd.as_deref().unwrap_or("."),
                scenario.terminal.cols,
                scenario.terminal.rows,
            )
            .map_err(|e| ScriptError::Pty(e.to_string()))?;
        let runner = Runner {
            registry,
            id,
            screen: Screen::new(scenario.terminal.cols, scenario.terminal.rows),
            dir: artifact_dir.join(slug(&scenario.name)),
        };
        runner.ensure_dir()?;
        Ok(runner)
    }

    fn ensure_dir(&self) -> Result<(), ScriptError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| ScriptError::io(&self.dir, e))
    }

    /// Write raw bytes to the child.
    fn send(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        let text = String::from_utf8_lossy(bytes);
        match self.registry.write(&self.id, &text) {
            Ok(()) => Ok(()),
            Err(TerminalError::Exited(_)) => Err(Failure::Child(self.death_note())),
            Err(e) => Err(Failure::Step(format!("write failed: {e}"))),
        }
    }

    /// Drain the child's output into the screen and write the emulator's
    /// reverse queries back to the pty.
    ///
    /// `wait_ms` of `None` returns immediately with whatever is buffered;
    /// `Some(ms)` blocks until the child produces something, which is what
    /// makes `wait_for_text` a settle-wait instead of a sleep. Returns `true`
    /// once the child has exited — the reader thread saw EOF — so the caller
    /// can stop before driving a corpse.
    fn drain(&mut self, wait_ms: Option<u64>) -> Result<bool, Failure> {
        let out = match self.registry.read(&self.id, wait_ms) {
            Ok(out) => out,
            Err(TerminalError::Exited(_)) => return Ok(true),
            Err(e) => return Err(Failure::Step(format!("read failed: {e}"))),
        };
        if !out.bytes.is_empty() {
            let replies = self.screen.feed(&out.bytes);
            if !replies.is_empty() {
                self.send(&replies)?;
            }
        }
        Ok(out.exited)
    }

    /// Why the child is gone, with the exit code when the pty layer knows it.
    /// `None` means the child was signalled rather than exited, which is
    /// spelled out rather than reported as a success.
    fn death_note(&self) -> String {
        match self
            .registry
            .status(&self.id)
            .ok()
            .and_then(|s| s.exit_code)
        {
            Some(code) => format!("child exited mid-scenario (exit code {code})"),
            None => "child terminated mid-scenario (no exit code: killed by a signal)".to_string(),
        }
    }

    /// The current frame as an ASCII box, written to
    /// `<NNN>-<label>.txt` and returned as the path to put in a note.
    fn snapshot(&mut self, step: usize, label: &str) -> Result<String, Failure> {
        let (cols, rows) = self.screen.size();
        let path = self.dir.join(format!("{:03}-{label}.txt", step));
        let art = render_ascii(
            &self.screen.styled_runs(),
            cols,
            self.screen.cursor(),
            &format!("step {step} · {label} · {cols}x{rows}"),
        );
        std::fs::write(&path, art)
            .map_err(|e| Failure::Step(format!("write {}: {e}", path.display())))?;
        Ok(path.display().to_string())
    }

    /// Write the `.txt` / `.svg` / `.json` triple for one frame.
    fn frame(&mut self, step: usize, name: &str) -> Result<Frame, Failure> {
        let (cols, rows) = self.screen.size();
        let runs = self.screen.styled_runs();
        let cursor = self.screen.cursor();
        let note = format!("step {step} · {name} · {cols}x{rows}");
        let stem = self.dir.join(format!("{:03}-{}", step, slug(name)));
        let txt = stem.with_extension("txt");
        let svg = stem.with_extension("svg");
        let json = stem.with_extension("json");
        let write = |path: &Path, body: String| -> Result<(), Failure> {
            std::fs::write(path, body)
                .map_err(|e| Failure::Step(format!("write {}: {e}", path.display())))
        };
        write(&txt, render_ascii(&runs, cols, cursor, &note))?;
        write(&svg, render_svg(&runs, cursor, &note))?;
        write(&json, render_json(&runs, cols, rows, cursor, &note))?;
        Ok(Frame { txt })
    }

    /// What a `wait_for_text` step observed: `Some(elapsed_ms)` once the text
    /// is on screen, `None` when the timeout expired first.
    fn wait_for_text(&mut self, text: &str, timeout_ms: u64) -> Result<Option<u64>, Failure> {
        let start = Instant::now();
        let deadline = start + Duration::from_millis(timeout_ms);
        // Check what is already on screen before blocking on the pty.
        if self.drain(None)? {
            return Err(Failure::Child(self.death_note()));
        }
        if self.screen.text().contains(text) {
            return Ok(Some(0));
        }
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = (deadline - now).as_millis() as u64;
            let poll = remaining.min(POLL_MS);
            if self.drain(Some(poll))? {
                return Err(Failure::Child(self.death_note()));
            }
            if self.screen.text().contains(text) {
                return Ok(Some(start.elapsed().as_millis() as u64));
            }
        }
    }

    /// An `assert_text` / `assert_not_text` step.
    fn assert(
        &mut self,
        step: usize,
        label: &str,
        needle: &str,
        want_present: bool,
    ) -> Result<String, Failure> {
        let present = self.screen.text().contains(needle);
        if present == want_present {
            return Ok(if present {
                format!("screen contains {needle:?}")
            } else {
                format!("screen does not contain {needle:?}")
            });
        }
        let path = self.snapshot(step, label)?;
        Err(Failure::Step(if want_present {
            format!("expected {needle:?} on screen, it is absent; snapshot at {path}")
        } else {
            format!("expected no {needle:?} on screen, it is present; snapshot at {path}")
        }))
    }

    /// One step, followed by the mandatory settle: drain the child's output
    /// into the screen and write the reverse queries back.
    fn step(&mut self, idx: usize, step: &Step) -> Result<String, Failure> {
        let note = self.execute(idx, step)?;
        if self.drain(None)? {
            return Err(Failure::Child(self.death_note()));
        }
        Ok(note)
    }

    /// The step itself, without the trailing settle.
    fn execute(&mut self, idx: usize, step: &Step) -> Result<String, Failure> {
        match step {
            Step::SendKeys { keys } => {
                let bytes = encode_keys(keys).map_err(Failure::Step)?;
                self.send(&bytes)?;
                Ok(format!("sent keys {keys:?} ({} bytes)", bytes.len()))
            }
            Step::Type { text } => {
                self.send(text.as_bytes())?;
                Ok(format!("typed {} bytes", text.len()))
            }
            Step::Mouse { row, col, button } => {
                self.send(button.sequence(*col, *row).as_bytes())?;
                Ok(format!(
                    "sent {button:?} press+release at col {col}, row {row}"
                ))
            }
            Step::Resize { cols, rows } => {
                self.registry
                    .resize(&self.id, *cols, *rows)
                    .map_err(|e| Failure::Step(format!("resize failed: {e}")))?;
                self.screen.resize(*cols, *rows);
                Ok(format!("resized to {cols}x{rows}"))
            }
            Step::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(format!("waited {ms}ms"))
            }
            Step::WaitForText { text, timeout_ms } => {
                match self.wait_for_text(text, *timeout_ms)? {
                    Some(elapsed) => Ok(format!("saw {text:?} after {elapsed}ms")),
                    None => {
                        let path = self.snapshot(idx, "wait")?;
                        Err(Failure::Step(format!(
                            "gave up after {timeout_ms}ms waiting for {text:?}; \
                             screen at that moment: {path}"
                        )))
                    }
                }
            }
            Step::Screenshot { name } => {
                let frame = self.frame(idx, name.as_deref().unwrap_or("frame"))?;
                Ok(format!(
                    "frame artifacts written: {}.txt/.svg/.json",
                    frame.txt.display()
                ))
            }
            Step::AssertText { contains } => self.assert(idx, "assert", contains, true),
            Step::AssertNotText { contains } => self.assert(idx, "assert-not", contains, false),
        }
    }

    /// Close the pty session. The reader thread and the child are ours from
    /// `start` on, so they are ours to clean up whether the run passed, failed
    /// or panicked.
    fn close(&self) {
        let _ = self.registry.close(&self.id);
    }
}

impl Drop for Runner<'_> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Filesystem-safe form of a scenario or step name. Empty or all-replaced
/// names fall back, so an artifact always has a stem to be named after.
fn slug(name: &str) -> String {
    let out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        "scenario".to_string()
    } else {
        out
    }
}

// -----------------------------------------------------------------------------
// Entry point
// -----------------------------------------------------------------------------

/// Drive a scenario on a real pty and write its artifacts.
///
/// A failed step stops the run: the step that failed is `Failed`, the rest
/// are `Skipped`, and the report still lands on disk. A run only comes back
/// as [`Err`] when it could not be set up at all (bad scenario JSON, a pty
/// the host would not give, a report that could not be written).
pub fn run_scenario(
    registry: &TerminalRegistry,
    scenario: &Scenario,
    artifact_dir: &Path,
) -> Result<RunReport, ScriptError> {
    let mut outcomes: Vec<StepOutcome> = Vec::with_capacity(scenario.steps.len());
    let mut runner = Runner::start(registry, scenario, artifact_dir)?;
    // Set once the run is over; the note is copied into every later step.
    let mut aborted: Option<String> = None;

    for (idx, step) in scenario.steps.iter().enumerate() {
        let action = step.action();
        if let Some(reason) = aborted.as_ref() {
            outcomes.push(StepOutcome {
                step: idx,
                action: action.to_string(),
                status: StepStatus::Skipped,
                note: reason.clone(),
            });
            continue;
        }
        // Before the step: the screen must be current, and a child that has
        // already died is reported on the step that could not run rather than
        // on whichever step happened to notice later.
        let result = match runner.drain(None) {
            Ok(true) => Err(Failure::Child(runner.death_note())),
            Ok(false) => runner.step(idx, step),
            Err(failure) => Err(failure),
        };
        match result {
            Ok(note) => outcomes.push(StepOutcome {
                step: idx,
                action: action.to_string(),
                status: StepStatus::Ok,
                note,
            }),
            Err(failure) => {
                let note = match failure {
                    Failure::Child(note) | Failure::Step(note) => note,
                };
                aborted = Some(format!("not run: step {idx} ({action}) did not complete"));
                outcomes.push(StepOutcome {
                    step: idx,
                    action: action.to_string(),
                    status: StepStatus::Failed,
                    note,
                });
            }
        }
    }

    let status = if outcomes
        .iter()
        .all(|outcome| outcome.status == StepStatus::Ok)
    {
        RunStatus::Passed
    } else {
        RunStatus::Failed
    };
    let report = RunReport {
        scenario: scenario.name.clone(),
        status,
        steps: outcomes,
    };
    write_report(&runner, &report)?;
    Ok(report)
}

/// Load a scenario file and run it. The entry point the smoke uses.
pub fn run_scenario_file(
    registry: &TerminalRegistry,
    path: &Path,
    artifact_dir: &Path,
) -> Result<RunReport, ScriptError> {
    run_scenario(registry, &Scenario::from_path(path)?, artifact_dir)
}

/// Serialize the report next to the frame artifacts. Written directly to its
/// final name: a `.tmp` that survives a crashed run is litter in a directory
/// people are meant to read.
fn write_report(runner: &Runner<'_>, report: &RunReport) -> Result<(), ScriptError> {
    let path = runner.dir.join("report.json");
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| ScriptError::Scenario(format!("report does not serialize: {e}")))?;
    std::fs::write(&path, format!("{json}\n")).map_err(|e| ScriptError::io(&path, e))
}
