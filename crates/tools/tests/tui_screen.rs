//! End-to-end contract for the screen tools (`terminal_screen`,
//! `terminal_screenshot`) and for the drain they must not corrupt
//! (`terminal_read`).
//!
//! The tests drive a real pty through the agent tools, not the emulator
//! directly, so they prove the wiring an agent actually sees: bytes that
//! `terminal_read` drains are also fed to the screen (the screen is kept by
//! the registry's reader thread, not by these tools stealing the drain
//! buffer), and the screen view never hands anything back to the drain.
//!
//! The pty is real, so a command's result is waited for through
//! `terminal_read`'s own `timeout_ms` (the registry's wait mechanism) rather
//! than guessed with bare sleeps. Once a read has returned a mark, the mark
//! is provably in the screen already — the reader thread feeds both from the
//! same chunk — so no waiting is needed around the screen call itself.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};
use terminal::TerminalRegistry;
use tools::Tool;
use tools::jobs_terminal::{
    TerminalCreate, TerminalKill, TerminalRead, TerminalScreen, TerminalScreenshot, TerminalWrite,
};

/// The sentinel written through `terminal_write` and looked for in both views.
const MARK: &str = "KYMIDO_SCREEN_MARK";

/// A fresh registry with the terminal tools wired to it, the shape the
/// daemon's `session_tools` produces.
struct Kit {
    create: TerminalCreate,
    write: TerminalWrite,
    read: TerminalRead,
    screen: TerminalScreen,
    shot: TerminalScreenshot,
    kill: TerminalKill,
    signal: AtomicBool,
}

impl Kit {
    fn new() -> Self {
        let reg = Arc::new(TerminalRegistry::new());
        Self {
            create: TerminalCreate::new(Arc::clone(&reg)),
            write: TerminalWrite::new(Arc::clone(&reg)),
            read: TerminalRead::new(Arc::clone(&reg)),
            screen: TerminalScreen::new(Arc::clone(&reg)),
            shot: TerminalScreenshot::new(Arc::clone(&reg)),
            kill: TerminalKill::new(reg),
            signal: AtomicBool::new(false),
        }
    }

    fn exec(&self, tool: &impl Tool, args: Value) -> Result<String, tools::ToolError> {
        tool.execute(&args, &self.signal)
    }

    /// Open a session and pull its id out of `terminal_create`'s reply
    /// (`terminal term-N opened ...`).
    fn open(&self, shell: &str) -> String {
        let out = self
            .exec(&self.create, json!({"shell": shell}))
            .expect("terminal_create succeeds");
        out.split_whitespace()
            .find(|w| w.starts_with("term-"))
            .expect("terminal_create reports its id")
            .to_string()
    }

    fn close(&self, id: &str) {
        self.exec(&self.kill, json!({"id": id}))
            .expect("terminal_kill succeeds");
    }
}

/// The old tool's promise must survive the new tools: what a `terminal_write`
/// produced is still delivered by the first `terminal_read`, the screen shows
/// that same content even though the drain already took it, and the mark is
/// not delivered a second time.
#[test]
fn screen_shows_what_read_drained_without_stealing_any_of_it() {
    let kit = Kit::new();
    let id = kit.open("bash");
    kit.exec(
        &kit.write,
        json!({"id": id, "data": format!("echo {MARK}\n")}),
    )
    .expect("terminal_write succeeds");

    // The old tool, first: drain semantics are intact — the mark arrives.
    let first_read = kit
        .exec(&kit.read, json!({"id": id, "timeout_ms": 10_000}))
        .expect("terminal_read succeeds");
    assert!(
        first_read.contains(MARK),
        "terminal_read must still deliver what terminal_write produced: {first_read}"
    );

    // The new tool: the screen must show the content the drain just took.
    let frame = kit
        .exec(&kit.screen, json!({"id": id}))
        .expect("terminal_screen succeeds");
    assert!(
        frame.contains(MARK),
        "terminal_screen must show what the terminal printed: {frame}"
    );

    // And the drain must not hand the same bytes out again: the screen view
    // takes nothing from the buffer, so the second read sees no mark.
    let second_read = kit
        .exec(&kit.read, json!({"id": id, "timeout_ms": 500}))
        .expect("terminal_read succeeds");
    assert!(
        !second_read.contains(MARK),
        "the mark must not reappear in a later read — the screen view must not \
         feed the drain: {second_read}"
    );

    kit.close(&id);
}

/// A session that has produced no bytes is an empty frame with an
/// explanation, in both formats, not an error.
#[test]
fn screen_of_a_quiet_session_is_an_empty_frame_not_an_error() {
    let kit = Kit::new();
    let id = kit.open("sleep 30");

    let frame = kit
        .exec(&kit.screen, json!({"id": id}))
        .expect("terminal_screen on a quiet session succeeds");
    assert!(
        frame.contains("no output received yet"),
        "an empty frame must say why it is empty: {frame}"
    );

    let json_frame = kit
        .exec(&kit.screen, json!({"id": id, "format": "json"}))
        .expect("terminal_screen in json format succeeds");
    let parsed: Value = serde_json::from_str(&json_frame).expect("the json frame parses as JSON");
    assert_eq!(
        parsed["frame_meta"]["cursor"]["row"],
        json!(0),
        "{json_frame}"
    );

    kit.close(&id);
}

/// `terminal_screenshot` writes the .txt/.svg/.json triple under a
/// per-(session, process) directory, reports every path, and leaves the files
/// readable. The name is model input, so it is checked to stay where it was
/// told.
#[test]
fn screenshot_writes_the_triple_and_reports_the_paths() {
    let kit = Kit::new();
    let id = kit.open("bash");
    kit.exec(
        &kit.write,
        json!({"id": id, "data": format!("echo {MARK}\n")}),
    )
    .expect("terminal_write succeeds");
    kit.exec(&kit.read, json!({"id": id, "timeout_ms": 10_000}))
        .expect("terminal_read succeeds");

    let out = kit
        .exec(&kit.shot, json!({"id": id, "name": "after-echo"}))
        .expect("terminal_screenshot succeeds");
    let txt = out
        .lines()
        .find(|l| l.ends_with(".txt"))
        .expect("the report lists the .txt path");
    let svg = out
        .lines()
        .find(|l| l.ends_with(".svg"))
        .expect("the report lists the .svg path");
    let json_path = out
        .lines()
        .find(|l| l.ends_with(".json"))
        .expect("the report lists the .json path");

    let dir = Path::new(txt)
        .parent()
        .expect("screenshot paths carry a directory");
    assert!(
        dir.to_str()
            .unwrap()
            .contains(&format!("{id}-{}", std::process::id())),
        "the triple must live in this process's directory for the session: {dir:?}"
    );
    for path in [txt, svg, json_path] {
        assert!(
            Path::new(path).exists(),
            "the reported path must exist: {path}"
        );
    }

    assert!(
        fs::read_to_string(txt)
            .expect("txt artifact")
            .contains(MARK)
    );
    let svg_body = fs::read_to_string(svg).expect("svg artifact");
    assert!(svg_body.contains("<svg"), "the SVG artifact must be XML");
    let json_parsed: Value =
        serde_json::from_str(&fs::read_to_string(json_path).expect("json artifact"))
            .expect("the json artifact parses");
    assert!(
        json_parsed["frame_meta"]["cursor"].is_object(),
        "the json artifact must carry the cursor: {json_parsed}"
    );

    // The directory is pid-scoped, so only this test can have written into it.
    let _ = fs::remove_dir_all(dir);
    kit.close(&id);
}

/// An unknown session id is a clear error, not a session that quietly
/// appeared.
#[test]
fn screen_of_an_unknown_session_is_a_clear_error() {
    let kit = Kit::new();
    let err = kit
        .exec(&kit.screen, json!({"id": "term-never-made"}))
        .expect_err("an unknown session must not succeed");
    assert!(
        err.to_string().contains("unknown terminal"),
        "the error must name the failure: {err}"
    );
    let err = kit
        .exec(&kit.shot, json!({"id": "term-never-made"}))
        .expect_err("an unknown session must not succeed");
    assert!(
        err.to_string().contains("unknown terminal"),
        "the error must name the failure: {err}"
    );
}
