//! screen.rs — VT-emulation screen layer and frame artifacts (T0a + T0b).
//!
//! [`Screen`] answers one question: *what would a user see on this terminal
//! right now?* It replays raw pty bytes through `alacritty_terminal` — the
//! same state machine a real terminal runs — so escape-sequence-driven TUI
//! output (cursor moves, erases, colour, box drawing, wide glyphs) collapses
//! into a grid of cells instead of a byte stream an agent cannot read.
//!
//! # Reverse queries (the load-bearing contract)
//!
//! While parsing, the emulator *answers* the child: `ESC[6n` cursor-position
//! reports, device-attribute queries, colour and text-area queries. Those
//! answers arrive as [`Event`]s and [`Screen::feed`] hands them back as the
//! return value. The caller MUST write them to the pty: a real terminal
//! answers automatically, so a program that probes the terminal hangs — or
//! silently degrades — if the answers never come back.
//!
//! # A frame, not a delta
//!
//! [`Screen::text`] and [`Screen::styled_runs`] describe the whole visible
//! screen on every call. Nothing here buffers "bytes since the last read";
//! that is [`crate::TerminalRegistry::read`]'s incremental promise and the
//! two semantics must not be mixed.
//!
//! # Frame artifacts
//!
//! Three renderers turn a frame into files an agent can actually read:
//!
//! * [`render_ascii`] — the primary artifact. A bordered box, one line per
//!   row, per-line trailing whitespace stripped, width measured in display
//!   cells.
//! * [`render_svg`] — the human artifact. Hand-rolled: one background
//!   `<rect>` per styled run, one `<text>` per run, a cursor outline
//!   `<rect>`, a metadata line. No graphics crate.
//! * [`render_json`] — the machine artifact. The styled runs, cursor and
//!   frame metadata as JSON, serialized by `serde_json` (workspace-pinned,
//!   already in the lock graph — not a new dependency) and pretty-printed.
//!
//! `crates/tui` stays untouched on purpose: this layer observes the TUI from
//! the outside, through the pty, and `crates/tui` keeps its zero-IO
//! discipline.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, MIN_COLUMNS, MIN_SCREEN_LINES, Term};
use alacritty_terminal::vte::ansi::{self, Color, NamedColor, Rgb};

/// Display width of one cell in the SVG artifact.
const SVG_CHAR_WIDTH: usize = 8;
/// Line height of one row in the SVG artifact.
const SVG_LINE_HEIGHT: usize = 18;
/// Outer margin of the SVG artifact.
const SVG_MARGIN: usize = 16;
/// Default text fill when a run carries no foreground colour.
const SVG_DEFAULT_FG: &str = "#d4d4d4";
/// Fixed monospace cell metric the headless layer claims for pixel queries.
const PIXEL_CELL_WIDTH: u16 = 8;
const PIXEL_CELL_HEIGHT: u16 = 16;

/// The cursor block marker in ASCII artifacts.
pub const CURSOR_BLOCK: char = '█';

// ---------------------------------------------------------------------------
// Reverse-query plumbing
// ---------------------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Collects what the emulator wants written back to the child.
///
/// `Term` owns its event proxy by value and exposes no accessor, so the queue
/// is shared: the proxy the emulator holds and the one [`Screen`] drains are
/// two handles on the same buffer.
#[derive(Clone)]
struct ReplyCollector {
    replies: Arc<Mutex<Vec<u8>>>,
    cols: Arc<AtomicU16>,
    rows: Arc<AtomicU16>,
}

impl EventListener for ReplyCollector {
    fn send_event(&self, event: Event) {
        match event {
            // Forward every write the emulator answered: cursor-position
            // reports, device attributes, mode reports, …
            Event::PtyWrite(text) => lock(&self.replies).extend_from_slice(text.as_bytes()),
            // Colour queries (OSC 4/10/11 "?") arrive as a formatter the
            // host must invoke. There is no real palette in a headless layer,
            // so answer with the xterm palette for indexed entries and the
            // fixed fg/bg defaults — enough that a probing program does not
            // hang; the exact value is not observable from the pty side.
            Event::ColorRequest(index, formatter) => {
                let rgb = match index {
                    256 => Rgb {
                        r: 0xe5,
                        g: 0xe5,
                        b: 0xe5,
                    },
                    257 => Rgb {
                        r: 0x1e,
                        g: 0x1e,
                        b: 0x1e,
                    },
                    258 => Rgb {
                        r: 0xff,
                        g: 0xff,
                        b: 0xff,
                    },
                    _ => indexed_rgb(index as u8),
                };
                lock(&self.replies).extend_from_slice(formatter(rgb).as_bytes());
            }
            // Text-area size in pixels (`CSI 14 t`) arrives as a callback
            // with no host metrics behind it; claim a fixed cell size so the
            // reply is well-formed. The char flavour (`CSI 18 t`) needs no
            // host knowledge — `Term` answers it with a `PtyWrite` itself.
            Event::TextAreaSizeRequest(formatter) => {
                lock(&self.replies).extend_from_slice(
                    formatter(WindowSize {
                        num_lines: self.rows.load(Ordering::Relaxed),
                        num_cols: self.cols.load(Ordering::Relaxed),
                        cell_width: PIXEL_CELL_WIDTH,
                        cell_height: PIXEL_CELL_HEIGHT,
                    })
                    .as_bytes(),
                );
            }
            // Damage, title, blink, … are for a GUI. Headless: ignore.
            _ => {}
        }
    }
}

/// [`Dimensions`] impl for the (cols, rows) pair `Term` wants.
#[derive(Clone, Copy)]
struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

// ---------------------------------------------------------------------------
// The screen layer
// ---------------------------------------------------------------------------

/// VT-emulation screen state: what a user would see on the terminal right
/// now.
///
/// Feed it raw pty bytes, then read [`Screen::text`] / [`Screen::styled_runs`]
/// for the current frame. [`Screen::feed`] returns the emulator's replies to
/// terminal queries (see the module docs) — write those back or probing
/// programs hang.
pub struct Screen {
    term: Term<ReplyCollector>,
    parser: ansi::Processor,
    replies: Arc<Mutex<Vec<u8>>>,
    cols: Arc<AtomicU16>,
    rows: Arc<AtomicU16>,
}

impl Screen {
    /// Create an empty screen of `cols` × `rows`. Dimensions below the
    /// emulator minimum (2 × 1) are clamped up, since a wide glyph needs two
    /// columns and a screen needs at least one row.
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = (cols as usize).max(MIN_COLUMNS) as u16;
        let rows = (rows as usize).max(MIN_SCREEN_LINES) as u16;
        let size = TermSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        let cols_shared = Arc::new(AtomicU16::new(cols));
        let rows_shared = Arc::new(AtomicU16::new(rows));
        let replies = Arc::new(Mutex::new(Vec::new()));
        let collector = ReplyCollector {
            replies: Arc::clone(&replies),
            cols: Arc::clone(&cols_shared),
            rows: Arc::clone(&rows_shared),
        };
        let term = Term::new(Config::default(), &size, collector);
        Self {
            term,
            parser: ansi::Processor::default(),
            replies,
            cols: cols_shared,
            rows: rows_shared,
        }
    }

    /// Feed raw pty output bytes into the emulator.
    ///
    /// Returns the emulator's reverse queries — cursor-position reports
    /// answering `ESC[6n`, device attributes, colour and text-area queries —
    /// concatenated in order. **Write the result back to the child process**,
    /// or a program that probes the terminal will hang or silently degrade.
    ///
    /// Feeding plain text produces an empty result.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        let mut queued = lock(&self.replies);
        std::mem::take(&mut queued)
    }

    /// Resize the screen. The emulator reflows wrapped lines and drops
    /// content that no longer fits, so shrinking can never leave stale
    /// rows behind.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = (cols as usize).max(MIN_COLUMNS) as u16;
        let rows = (rows as usize).max(MIN_SCREEN_LINES) as u16;
        self.cols.store(cols, Ordering::Relaxed);
        self.rows.store(rows, Ordering::Relaxed);
        self.term.resize(TermSize {
            cols: cols as usize,
            rows: rows as usize,
        });
    }

    /// The current frame as plain text: every visible row, one `\n`-separated
    /// line each, trailing whitespace per line stripped. Wide characters are
    /// emitted whole — the cells that belong to a wide glyph are never
    /// printed as spaces or half-chars.
    ///
    /// This is a *frame*, not a delta: every call returns the whole screen.
    pub fn text(&self) -> String {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let mut out = String::with_capacity(rows.saturating_mul(cols) + rows);
        for row in 0..rows {
            if row > 0 {
                out.push('\n');
            }
            out.push_str(row_text(&grid[Line(row as i32)], cols).trim_end());
        }
        out
    }

    /// The current frame as styled runs: one [`StyledLine`] per visible row,
    /// each row a run-length list of [`StyledRun`]s with uniform fg/bg/
    /// modifier. Runs carry their width in display cells (`cells`), so
    /// CJK and emoji never count as one cell each.
    pub fn styled_runs(&self) -> Vec<StyledLine> {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        (0..rows)
            .map(|row| styled_line(&grid[Line(row as i32)], cols, row + 1))
            .collect()
    }

    /// Cursor position as `(row, col)`, 0-indexed top-left, matching the
    /// emulator convention. Shape and blink are not emulated — only where the
    /// cursor is.
    pub fn cursor(&self) -> (u16, u16) {
        let point = self.term.grid().cursor.point;
        (point.line.0.max(0) as u16, point.column.0 as u16)
    }

    /// Screen dimensions as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        (
            self.cols.load(Ordering::Relaxed),
            self.rows.load(Ordering::Relaxed),
        )
    }
}

/// Plain text of one grid row: wide-char spacers skipped, zerowidth
/// combining characters reattached to their base cell.
fn row_text(row: &alacritty_terminal::grid::Row<Cell>, cols: usize) -> String {
    let mut s = String::with_capacity(cols);
    for col in 0..cols {
        let cell = &row[Column(col)];
        if is_wide_spacer(cell) {
            continue;
        }
        s.push(cell.c);
        if let Some(zw) = cell.zerowidth() {
            s.extend(zw.iter());
        }
    }
    s
}

fn is_wide_spacer(cell: &Cell) -> bool {
    cell.flags
        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
}

/// Display cells one grid cell holds: a wide glyph takes two, everything
/// else one. Exact — read from the grid, not measured.
fn cell_cells(cell: &Cell) -> u16 {
    if cell.flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        1
    }
}

fn styled_line(row: &alacritty_terminal::grid::Row<Cell>, cols: usize, line: usize) -> StyledLine {
    // The rightmost cell worth drawing: a blank cell with the default
    // background is padding, not content; a filled bar (a background colour
    // is set) counts as content and survives right-trimming.
    let last = (0..cols).rposition(|col| {
        let cell = &row[Column(col)];
        !is_wide_spacer(cell) && (cell.c != ' ' || color_css(&cell.bg).is_some())
    });
    let Some(last) = last else {
        return StyledLine {
            line,
            runs: Vec::new(),
        };
    };
    let mut runs: Vec<StyledRun> = Vec::new();
    for col in 0..=last {
        let cell = &row[Column(col)];
        if is_wide_spacer(cell) {
            continue;
        }
        let fg = color_css(&cell.fg);
        let bg = color_css(&cell.bg);
        let modifier = modifier_of(cell.flags);
        match runs.last_mut() {
            Some(last) if last.fg == fg && last.bg == bg && last.modifier == modifier => {}
            _ => runs.push(StyledRun {
                text: String::new(),
                fg,
                bg,
                modifier,
                cells: 0,
            }),
        }
        let run = runs.last_mut().expect("pushed in the match arm above");
        run.text.push(cell.c);
        if let Some(zw) = cell.zerowidth() {
            run.text.extend(zw.iter());
        }
        run.cells += cell_cells(cell);
    }
    StyledLine { line, runs }
}

// ---------------------------------------------------------------------------
// Style model
// ---------------------------------------------------------------------------

/// A run of text with a uniform style, coalesced per identical cell style.
///
/// `cells` is the run's width in display cells: a wide glyph counts two, a
/// combining mark zero. Every renderer measures from this field — never from
/// `text.chars().count()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledRun {
    /// The visible characters. May include combining marks (zerowidth).
    pub text: String,
    /// Foreground as CSS hex, `None` = terminal default.
    pub fg: Option<String>,
    /// Background as CSS hex, `None` = terminal default.
    pub bg: Option<String>,
    /// Active attributes.
    pub modifier: Modifier,
    /// Width in display cells.
    pub cells: u16,
}

/// One visible row: an ordinal plus its styled runs (left to right).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    /// 1-indexed line ordinal; `cursor` coordinates stay 0-indexed.
    pub line: usize,
    pub runs: Vec<StyledRun>,
}

/// Cell attributes, one bit per attribute.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifier(u16);

impl Modifier {
    pub const NONE: Modifier = Modifier(0);
    pub const BOLD: Modifier = Modifier(1 << 0);
    pub const DIM: Modifier = Modifier(1 << 1);
    pub const ITALIC: Modifier = Modifier(1 << 2);
    pub const UNDERLINE: Modifier = Modifier(1 << 3);
    pub const STRIKEOUT: Modifier = Modifier(1 << 4);
    pub const INVERSE: Modifier = Modifier(1 << 5);
    pub const HIDDEN: Modifier = Modifier(1 << 6);

    /// Whether all bits of `other` are set in `self`.
    pub fn has(self, other: Modifier) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no attribute is set.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Union of two modifiers.
    pub const fn union(self, other: Modifier) -> Modifier {
        Modifier(self.0 | other.0)
    }

    /// Active attribute names, in declaration order (for the JSON artifact).
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let table = [
            (Modifier::BOLD, "bold"),
            (Modifier::DIM, "dim"),
            (Modifier::ITALIC, "italic"),
            (Modifier::UNDERLINE, "underline"),
            (Modifier::STRIKEOUT, "strikeout"),
            (Modifier::INVERSE, "inverse"),
            (Modifier::HIDDEN, "hidden"),
        ];
        for (bit, name) in table {
            if self.has(bit) {
                out.push(name);
            }
        }
        out
    }
}

/// Cell attributes as a [`Modifier`] mask. The underline flavours collapse
/// into one bit: an agent reading the artifact cares that a run is
/// underlined, not which line it is.
fn modifier_of(flags: Flags) -> Modifier {
    let mut m = Modifier::NONE;
    for (flag, bit) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::STRIKEOUT, Modifier::STRIKEOUT),
        (Flags::INVERSE, Modifier::INVERSE),
        (Flags::HIDDEN, Modifier::HIDDEN),
    ] {
        if flags.contains(flag) {
            m = m.union(bit);
        }
    }
    // `contains` needs every bit set, so the underline flavours are tested
    // one by one rather than as a union mask.
    if [
        Flags::UNDERLINE,
        Flags::DOUBLE_UNDERLINE,
        Flags::DOTTED_UNDERLINE,
        Flags::DASHED_UNDERLINE,
        Flags::UNDERCURL,
    ]
    .iter()
    .any(|flavour| flags.contains(*flavour))
    {
        m = m.union(Modifier::UNDERLINE);
    }
    m
}

/// Terminal colour to CSS hex. The two "default" colours (foreground /
/// background) have no value of their own, so they map to `None`.
fn color_css(color: &Color) -> Option<String> {
    match color {
        Color::Spec(rgb) => Some(rgb_hex(rgb.r, rgb.g, rgb.b)),
        Color::Indexed(idx) => Some(indexed_hex(*idx)),
        Color::Named(name) => match name {
            NamedColor::Foreground | NamedColor::Background | NamedColor::Cursor => None,
            NamedColor::Black => Some(rgb_hex(0, 0, 0)),
            NamedColor::Red => Some(rgb_hex(205, 0, 0)),
            NamedColor::Green => Some(rgb_hex(0, 205, 0)),
            NamedColor::Yellow => Some(rgb_hex(205, 205, 0)),
            NamedColor::Blue => Some(rgb_hex(0, 0, 238)),
            NamedColor::Magenta => Some(rgb_hex(205, 0, 205)),
            NamedColor::Cyan => Some(rgb_hex(0, 205, 205)),
            NamedColor::White => Some(rgb_hex(229, 229, 229)),
            NamedColor::BrightBlack => Some(rgb_hex(127, 127, 127)),
            NamedColor::BrightRed => Some(rgb_hex(255, 0, 0)),
            NamedColor::BrightGreen => Some(rgb_hex(0, 255, 0)),
            NamedColor::BrightYellow => Some(rgb_hex(255, 255, 0)),
            NamedColor::BrightBlue => Some(rgb_hex(92, 92, 255)),
            NamedColor::BrightMagenta => Some(rgb_hex(255, 0, 255)),
            NamedColor::BrightCyan => Some(rgb_hex(0, 255, 255)),
            NamedColor::BrightWhite => Some(rgb_hex(255, 255, 255)),
            // The dim palette entries have no fixed value of their own;
            // answering them with the default keeps the artifact honest.
            NamedColor::DimBlack => Some(rgb_hex(0, 0, 0)),
            NamedColor::DimRed => Some(rgb_hex(128, 0, 0)),
            NamedColor::DimGreen => Some(rgb_hex(0, 128, 0)),
            NamedColor::DimYellow => Some(rgb_hex(128, 128, 0)),
            NamedColor::DimBlue => Some(rgb_hex(0, 0, 128)),
            NamedColor::DimMagenta => Some(rgb_hex(128, 0, 128)),
            NamedColor::DimCyan => Some(rgb_hex(0, 128, 128)),
            NamedColor::DimWhite => Some(rgb_hex(127, 127, 127)),
            NamedColor::BrightForeground => None,
            NamedColor::DimForeground => None,
        },
    }
}

fn rgb_hex(r: u8, g: u8, b: u8) -> String {
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

/// xterm 256-palette entry as RGB: the standard 16, the 6×6×6 cube, and the
/// grey ramp. One table feeds both the CSS colours and the answers to
/// `Event::ColorRequest`.
fn indexed_rgb(idx: u8) -> Rgb {
    const STANDARD16: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    let component = |level: u8| if level == 0 { 0 } else { 55 + 40 * level };
    let rgb = if (0..16).contains(&idx) {
        STANDARD16[idx as usize]
    } else if (16..=231).contains(&idx) {
        let i = idx - 16;
        [component(i / 36), component((i % 36) / 6), component(i % 6)]
    } else {
        let v = 8 + 10 * (idx - 232);
        [v, v, v]
    };
    Rgb {
        r: rgb[0],
        g: rgb[1],
        b: rgb[2],
    }
}

fn indexed_hex(idx: u8) -> String {
    let rgb = indexed_rgb(idx);
    rgb_hex(rgb.r, rgb.g, rgb.b)
}

// ---------------------------------------------------------------------------
// Display-cell width (table-free)
// ---------------------------------------------------------------------------

/// Display width of `s` in terminal cells: East-Asian wide and most emoji
/// code points count two, combining marks zero, everything else one.
///
/// This is a judgement by code-point range — no `unicode-width` table, and
/// no second dependency: this crate may add exactly one. It mirrors the
/// table-free heuristic `crates/tui` already uses
/// (`crates/tui/src/inline.rs::char_cells`). Grid-derived runs carry their
/// *exact* cell count in [`StyledRun::cells`]; this function exists for
/// text the grid did not measure (the cursor overlay, the box labels) and
/// may misjudge one exotic glyph, which costs one off-by-one in a label,
/// never in a run.
pub fn display_width(s: &str) -> u16 {
    s.chars().map(char_cells).sum()
}

/// Cells one character occupies: 0 for combining marks, 2 for East-Asian
/// wide glyphs, 1 otherwise. Same range table as
/// `crates/tui/src/inline.rs::char_cells`.
fn char_cells(c: char) -> u16 {
    let cp = c as u32;
    if matches!(
        cp,
        // decimal code-point ranges; hex kept in comments only
        768..=879 // 0300..036F combining diacritics
            | 6832..=6911 // 1AB0..1AFF
            | 7616..=7679 // 1DC0..1DFF
            | 8400..=8447 // 20D0..20FF
            | 8203..=8207 // 200B..200F zero-width space / direction marks
            | 8288..=8292 // 2060..2064
            | 65024..=65039 // FE00..FE0F variation selectors
            | 65056..=65071 // FE20..FE2F
    ) {
        return 0;
    }
    if matches!(
        cp,
        4352..=4447 // 1100..115F hangul jamo
            | 11904..=12350 // 2E80..303E CJK radicals / symbols
            | 12353..=13311 // 3041..33FF
            | 13312..=19903 // 3400..4DBF
            | 19968..=40959 // 4E00..9FFF unified ideographs
            | 40960..=42191 // A000..A4CF
            | 44032..=55203 // AC00..D7A3 hangul syllables
            | 63744..=64255 // F900..FAFF
            | 65072..=65135 // FE30..FE6F
            | 65280..=65376 // FF00..FF60 fullwidth
            | 65504..=65510 // FFE0..FFE6
            | 127744..=128591 // 1F300..1F64F emoji block
            | 129280..=129535 // 1F900..1F9FF
            | 131072..=262141 // 20000..3FFFD CJK extension B+
    ) {
        return 2;
    }
    1
}

// ---------------------------------------------------------------------------
// T0b: frame artifacts
// ---------------------------------------------------------------------------

/// The frame as a bordered ASCII box.
///
/// ```text
/// ┌─ 80 x 24 ─────────────────────────┐
/// │ transcript row                    │
/// │ composer row █ here               │
/// ├─ <note> ──────────────────────────┤
/// └─ cursor 5,7 ──────────────────────┘
/// ```
///
/// * content lines are right-trimmed per row and padded to the box border in
///   display cells — a CJK row is *not* padded by character count;
/// * the cursor row gets a [`CURSOR_BLOCK`] at the cursor cell;
/// * every line of the box has the same display width, so a reader (or an
///   LLM) can diff frames line by line.
pub fn render_ascii(runs: &[StyledLine], cols: u16, cursor: (u16, u16), note: &str) -> String {
    let inner = cols as usize;
    let mut out = String::new();

    // header: ┌─ label … ┐
    out.push_str(&box_line(
        '┌',
        '─',
        '┐',
        &format!("{cols} x {}", runs.len()),
        inner,
    ));

    for (i, line) in runs.iter().enumerate() {
        let overlay = (i as u16 == cursor.0).then_some(cursor.1);
        out.push('\n');
        out.push_str(&content_line(line_text(line, overlay), inner));
    }

    if !note.is_empty() {
        out.push('\n');
        out.push_str(&box_line('├', '─', '┤', note, inner));
    }

    out.push('\n');
    out.push_str(&box_line(
        '└',
        '─',
        '┘',
        &format!("cursor {},{}", cursor.0, cursor.1),
        inner,
    ));
    out.push('\n');
    out
}

/// Assemble one content line: the row's text, a cursor block at `col` when
/// requested, padded with spaces to `inner` display cells, framed as
/// `│ … │`. Padding is measured in cells, not characters.
fn content_line(text: String, inner: usize) -> String {
    let mut out = format!("│ {text}");
    let used = display_width(&text) as usize;
    if used < inner {
        out.push_str(&" ".repeat(inner - used));
    }
    out.push('│');
    out
}

/// The row's visible text, with a [`CURSOR_BLOCK`] at the cursor cell.
///
/// The overlay walks the run's characters counting display cells with
/// [`char_cells`] — the only place the table-free heuristic does work:
/// grid-exact `cells` counts only run boundaries, not individual glyphs.
fn line_text(line: &StyledLine, cursor_col: Option<u16>) -> String {
    let mut text = String::new();
    let mut cell_offset: u16 = 0;
    for run in &line.runs {
        if let Some(cursor_col) = cursor_col {
            if (cell_offset..cell_offset.saturating_add(run.cells)).contains(&cursor_col) {
                text.push_str(&overlay_cursor(&run.text, cursor_col - cell_offset));
            } else {
                text.push_str(&run.text);
            }
        } else {
            text.push_str(&run.text);
        }
        cell_offset = cell_offset.saturating_add(run.cells);
    }
    text
}

/// Replace the character sitting at display cell `col` with the cursor
/// block. A wide glyph under the cursor is replaced by block + one filler
/// space, so the glyph is never split and the rest of the line stays
/// cell-aligned. A cursor in the *second* half of a wide glyph is not
/// overlaid (there is no whole glyph there to replace).
fn overlay_cursor(text: &str, col: u16) -> String {
    let mut out = String::with_capacity(text.len() + 1);
    let mut cell = 0u16;
    for c in text.chars() {
        let w = char_cells(c);
        if w == 0 {
            // combining marks follow their base glyph; keep them attached
            out.push(c);
            continue;
        }
        out.push(if cell == col { CURSOR_BLOCK } else { c });
        for _ in 1..w {
            out.push(' ');
        }
        cell = cell.saturating_add(w);
    }
    out
}

/// A box border line: `start` + fill + label + fill*`n` + `end`, where
/// `n` is chosen so the whole line spans exactly the same display width as a
/// content line (inner + 3 cells: `│` + pad + inner + `│`). A label too long
/// for the frame is cut with an ellipsis instead of widening the line, so
/// every line of a box keeps one width.
fn box_line(start: char, fill: char, end: char, label: &str, inner: usize) -> String {
    let label = truncate_cells(label, inner.saturating_sub(2));
    let label_cells = display_width(&label) as usize;
    // total = label_cells + 5 + dashes; content lines span inner + 3.
    let dashes = inner.saturating_sub(label_cells).saturating_sub(2);
    let mut out = format!("{start}{fill} {label} ");
    out.push_str(&fill.to_string().repeat(dashes));
    out.push(end);
    out
}

/// Cut `s` to at most `budget` display cells, marking the cut with an ellipsis.
fn truncate_cells(s: &str, budget: usize) -> String {
    if display_width(s) as usize <= budget {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = char_cells(c) as usize;
        // keep one cell in hand for the ellipsis
        if used + w > budget.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The frame as a hand-rolled SVG: one background `<rect>` per styled run,
/// one `<text>` per run, a cursor outline `<rect>`, and a metadata line.
/// No graphics crate — the artifact is text, diff-able and greppable.
///
/// Widths advance by display cells (8 px each), so CJK rows stay aligned
/// with the cursor outline and the box edges.
pub fn render_svg(runs: &[StyledLine], cursor: (u16, u16), note: &str) -> String {
    let cols = runs
        .iter()
        .map(|line| line.runs.iter().map(|run| run.cells).sum::<u16>() as usize)
        .max()
        .unwrap_or(0)
        .max(1);
    let rows = runs.len().max(1);
    let note_height = if note.is_empty() {
        0
    } else {
        SVG_LINE_HEIGHT + 8
    };
    let width = cols * SVG_CHAR_WIDTH + SVG_MARGIN * 2;
    let height = rows * SVG_LINE_HEIGHT + SVG_MARGIN * 2 + note_height;

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"#1e1e1e\"/>\n\
         <style>text {{ font-family: Menlo, Monaco, Consolas, monospace; font-size: 14px; white-space: pre; }}</style>\n"
    );

    if !note.is_empty() {
        svg.push_str(&format!(
            "<text x=\"{SVG_MARGIN}\" y=\"{}\" fill=\"#9cdcfe\">{}</text>\n",
            SVG_MARGIN + 12,
            escape_xml(note),
        ));
    }

    let y_offset = SVG_MARGIN + note_height;
    for (i, line) in runs.iter().enumerate() {
        let mut x = SVG_MARGIN;
        let y = y_offset + i * SVG_LINE_HEIGHT + 14;
        for run in &line.runs {
            let StyledRun {
                text,
                fg,
                bg,
                modifier,
                cells,
            } = run;
            // inverse swaps the pair; otherwise text uses fg, rect uses bg
            let (text_fill, rect_fill) = if modifier.has(Modifier::INVERSE) {
                (bg.as_deref(), fg.as_deref())
            } else {
                (fg.as_deref(), bg.as_deref())
            };
            let run_width = *cells as usize * SVG_CHAR_WIDTH;
            if let Some(bg) = rect_fill {
                svg.push_str(&format!(
                    "<rect x=\"{x}\" y=\"{}\" width=\"{run_width}\" height=\"{SVG_LINE_HEIGHT}\" fill=\"{}\"/>\n",
                    y - 14,
                    escape_attr(bg),
                ));
            }
            let mut attrs = format!(
                "fill=\"{}\"",
                escape_attr(text_fill.unwrap_or(SVG_DEFAULT_FG))
            );
            if modifier.has(Modifier::BOLD) {
                attrs.push_str(" font-weight=\"700\"");
            }
            if modifier.has(Modifier::ITALIC) {
                attrs.push_str(" font-style=\"italic\"");
            }
            let mut deco = Vec::new();
            if modifier.has(Modifier::UNDERLINE) {
                deco.push("underline");
            }
            if modifier.has(Modifier::STRIKEOUT) {
                deco.push("line-through");
            }
            if !deco.is_empty() {
                attrs.push_str(&format!(" text-decoration=\"{}\"", deco.join(" ")));
            }
            if modifier.has(Modifier::DIM) {
                attrs.push_str(" opacity=\"0.55\"");
            }
            svg.push_str(&format!(
                "<text x=\"{x}\" y=\"{y}\" {attrs}>{}</text>\n",
                escape_xml(text),
            ));
            x += run_width;
        }
    }

    let cursor_x = SVG_MARGIN + (cursor.1 as usize) * SVG_CHAR_WIDTH;
    let cursor_y = y_offset + (cursor.0 as usize) * SVG_LINE_HEIGHT;
    svg.push_str(&format!(
        "<rect x=\"{cursor_x}\" y=\"{cursor_y}\" width=\"{SVG_CHAR_WIDTH}\" height=\"{SVG_LINE_HEIGHT}\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"1\"/>\n"
    ));

    let meta = if note.is_empty() {
        format!("{cols} x {rows}")
    } else {
        format!("{cols} x {rows} · {note}")
    };
    svg.push_str(&format!(
        "<text x=\"{}\" y=\"{}\" fill=\"#7f7f7f\" font-size=\"12\">{}</text>\n",
        SVG_MARGIN,
        height - 6,
        escape_xml(&meta),
    ));

    svg.push_str("</svg>\n");
    svg
}

/// The frame as JSON: styled runs, cursor and frame metadata, serialized
/// with `serde_json` (workspace-pinned, brief §3 T0b) and pretty-printed so
/// an agent can read it in a diff.
pub fn render_json(
    runs: &[StyledLine],
    cols: u16,
    rows: u16,
    cursor: (u16, u16),
    note: &str,
) -> String {
    let lines: Vec<serde_json::Value> = runs
        .iter()
        .map(|line| {
            let runs: Vec<serde_json::Value> = line
                .runs
                .iter()
                .map(|run| {
                    serde_json::json!({
                        "text": run.text,
                        "cells": run.cells,
                        "fg": run.fg,
                        "bg": run.bg,
                        "modifier": run.modifier.names(),
                    })
                })
                .collect();
            serde_json::json!({ "line": line.line, "runs": runs })
        })
        .collect();
    let frame = serde_json::json!({
        "frame_meta": {
            "cols": cols,
            "rows": rows,
            "cursor": { "row": cursor.0, "col": cursor.1 },
            "note": note,
        },
        "lines": lines,
    });
    // Every value above came from `json!`, so serialization cannot fail.
    serde_json::to_string_pretty(&frame).expect("json! values always serialize")
}

/// XML text-content escaping. Screen text may contain `<` `>` `&` — exactly
/// the characters that would break the SVG structure if left raw.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Attribute-value escaping: text escaping plus the two quote forms, since
/// SVG attributes are double-quoted.
fn escape_attr(s: &str) -> String {
    escape_xml(s).replace('"', "&quot;").replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------
// Tests live in `crates/terminal/tests/screen.rs` (integration, run by CI).
// ---------------------------------------------------------------------------
