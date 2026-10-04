// Integration tests for the VT-emulation screen layer (`src/screen.rs`).
//
// The tests below are the contracts the screen layer has to hold for a
// probing TUI: the emulator's reverse queries must come back out of `feed`,
// wide glyphs must stay whole, a resize must not leave stale rows, malformed
// input must not panic, and the three frame artifacts must be readable and
// correctly escaped.

use terminal::screen::{
    CURSOR_BLOCK, Modifier, Screen, StyledRun, display_width, render_ascii, render_svg,
};

/// Width of a run's text in display cells, measured through the public
/// `display_width` — the same measure the renderers use.
fn cells_of(text: &str) -> u16 {
    text.chars().map(|c| display_width(&c.to_string())).sum()
}

fn run_text(runs: &[StyledRun], i: usize) -> &str {
    &runs[i].text
}

/// The head-of-list regression: a child that probes the terminal (DA, cursor
/// position, colour) hangs or silently degrades unless the emulator's answers
/// come back out of `feed` for the caller to write back.
#[test]
fn feed_returns_emulator_replies() {
    let mut screen = Screen::new(20, 5);

    // Cursor position report, 1-indexed as the protocol demands.
    assert_eq!(screen.feed(b"\x1b[6n"), b"\x1b[1;1R");
    screen.feed(b"hello");
    assert_eq!(screen.feed(b"\x1b[6n"), b"\x1b[1;6R");
    assert_eq!(screen.feed(b"\x1b[c"), b"\x1b[?6c");
    assert_eq!(screen.feed(b"\x1b[5n"), b"\x1b[0n");
    // Char-flavour text-area size is answered by the emulator itself; the
    // pixel flavour needs a host cell metric and must not be left silent.
    assert_eq!(screen.feed(b"\x1b[18t"), b"\x1b[8;5;20t");
    assert_eq!(screen.feed(b"\x1b[14t"), b"\x1b[4;80;160t");

    // Colour queries arrive as a callback to invoke, not as a write.
    let osc11 = screen.feed(b"\x1b]11;?\x07");
    assert!(osc11.starts_with(b"\x1b]11;rgb:"), "{osc11:?}");
    assert!(osc11.ends_with(b"\x07"), "{osc11:?}");
    assert_eq!(
        screen.feed(b"\x1b]4;3;?\x07"),
        b"\x1b]4;3;rgb:cdcd/cdcd/0000\x07"
    );

    // Text asks nothing, and each query is answered fresh rather than
    // replaying a stale queue.
    assert!(screen.feed(b"plain text").is_empty());
    assert_eq!(screen.feed(b"\x1b[6n"), b"\x1b[1;16R");
}

#[test]
fn wide_chars_never_split_cells() {
    let mut screen = Screen::new(4, 3);
    screen.feed("ab你".as_bytes());
    let runs = screen.styled_runs();
    assert_eq!(runs[0].runs.len(), 1);
    assert_eq!(run_text(&runs[0].runs, 0), "ab你");
    assert_eq!(
        runs[0].runs[0].cells, 4,
        "a wide glyph is two cells, not one"
    );
    assert_eq!(runs[0].runs[0].cells, cells_of("ab你"));

    screen.feed("好cd".as_bytes());
    assert_eq!(screen.text(), "ab你\n好cd\n");

    // Too narrow for the glyph: the row wraps the whole glyph, never half.
    let mut screen = Screen::new(4, 3);
    screen.feed("你好啊".as_bytes());
    let wrapped = screen.text();
    let lines: Vec<&str> = wrapped.split('\n').collect();
    assert_eq!(lines, ["你好", "啊", ""]);
    let glyphs: Vec<char> = wrapped.chars().filter(|c| *c != '\n').collect();
    assert_eq!(glyphs, ['你', '好', '啊'], "each glyph appears once, whole");

    for line in screen.styled_runs() {
        for run in &line.runs {
            assert_eq!(
                run.cells,
                cells_of(&run.text),
                "cells must match the text's display width"
            );
        }
    }
    assert_eq!(display_width("你"), 2);
    assert_eq!(display_width("ab"), 2);
}

#[test]
fn resize_reflows_and_drops_stale_rows() {
    let mut screen = Screen::new(40, 3);
    screen.feed(b"0123456789012345678901234567890123456789\r\nrow2\r\nrow3");

    screen.resize(20, 3);
    assert_eq!(screen.size(), (20, 3));
    let frame = screen.text();
    let lines: Vec<&str> = frame.split('\n').collect();
    assert_eq!(
        lines[0], "01234567890123456789",
        "narrowing reflows the wrapped row"
    );
    assert_eq!(lines[1], "row2");
    // Losing a row drops its content rather than leaving it behind.
    let mut screen = Screen::new(40, 3);
    screen.feed(b"top\r\nmiddle\r\nbottom");
    screen.resize(40, 2);
    assert_eq!(screen.text(), "middle\nbottom");
    screen.resize(40, 4);
    assert_eq!(
        screen.text(),
        "top\nmiddle\nbottom\n",
        "growing back must not scramble rows"
    );

    // Below the emulator's minimum the size clamps instead of panicking.
    screen.resize(0, 0);
    assert_eq!(screen.size(), (2, 1));
}

#[test]
fn malformed_escape_does_not_panic() {
    let mut screen = Screen::new(20, 4);
    for bytes in [
        &b"\x1b[38;2;1"[..],    // truncated SGR
        b"\x1b]0;unterminated", // OSC string that ends at the BEL below
        b"\x1b",                // lone escape
        b"\x07",
        b"\x1b[9999999999Z",    // out-of-range parameter
        b"\x1b_p junk \x1b\\",  // DCS that never opens
        b"\x1b[2K\x1b[?9999$p", // erase + an unsupported DECRQM
    ] {
        let _ = screen.feed(bytes);
    }
    screen.feed(b"ok\r\ndone");
    let text = screen.text();
    assert_eq!(
        text, "ok\ndone\n\n",
        "the frame survives the garbage intact"
    );
    assert!(
        !text.contains('\x1b'),
        "no escape byte leaks into the frame: {text:?}"
    );
}

#[test]
fn screen_is_full_frame_not_incremental() {
    let mut screen = Screen::new(20, 3);
    screen.feed(b"first\r\nsecond\r\n");

    // The first row is still on screen: a read is the whole frame, not the
    // bytes since the last one.
    assert_eq!(screen.text(), "first\nsecond\n");
    assert_eq!(
        screen.text().split('\n').count(),
        3,
        "one line per row, blanks included"
    );
    assert_eq!(
        screen.text(),
        screen.text(),
        "reading a frame does not consume it"
    );

    // And the frame tracks the screen, not the byte stream: clearing removes
    // the old rows instead of stacking them up.
    screen.feed(b"\x1b[2J\x1b[Hfresh");
    assert_eq!(screen.text(), "fresh\n\n");
    assert!(!screen.text().contains("second"));
}

#[test]
fn ascii_strips_trailing_whitespace() {
    let mut screen = Screen::new(20, 3);
    screen.feed(b"padded   \r\nnext");
    let runs = screen.styled_runs();

    // The frame itself carries no padding after the content.
    assert_eq!(run_text(&runs[0].runs, 0), "padded");
    assert_eq!(runs[0].runs[0].cells, 6);
    assert!(
        runs.iter()
            .all(|l| l.runs.iter().all(|r| r.text == r.text.trim_end()))
    );

    let ascii = render_ascii(&runs, 20, screen.cursor(), "");
    let inner: Vec<&str> = ascii
        .lines()
        .filter_map(|l| l.strip_prefix('│'))
        .map(|l| l.strip_suffix('│').unwrap_or(l))
        .collect();
    assert_eq!(inner[0].trim(), "padded");
    assert_eq!(inner[1].trim(), "next");
    assert_eq!(inner[2].trim(), "", "an empty row is blank inside the box");

    let widths: Vec<u16> = ascii.lines().map(display_width).collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "one width for every line: {widths:?}"
    );
    assert_eq!(widths[0], 23, "20 inner cells plus both borders");
}

#[test]
fn ascii_width_uses_display_cells() {
    let mut screen = Screen::new(20, 3);
    screen.feed("一二三四五六七八九十\r\n".as_bytes()); // 10 glyphs, 20 cells
    screen.feed("abcdefghijklmnopqrst".as_bytes()); // 20 chars, 20 cells
    screen.feed("> 你好".as_bytes());
    let runs = screen.styled_runs();

    // The CJK row is 10 characters but fills the frame; measuring by
    // character count would make this line half as wide as its neighbours.
    assert_eq!(runs[0].runs[0].cells, 20);
    assert_eq!(runs[0].runs[0].text.chars().count(), 10);

    let ascii = render_ascii(&runs, 20, (2, 4), "cells");
    let widths: Vec<u16> = ascii.lines().map(display_width).collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "one width for every line: {widths:?}"
    );
    let cjk = ascii
        .lines()
        .find(|l| l.contains('一'))
        .expect("the CJK row");
    assert_eq!(display_width(cjk), 23);
    assert_eq!(
        cjk.chars().count(),
        13,
        "the same line is far shorter in characters"
    );

    // The cursor block lands on a cell, and a wide glyph under it is not
    // split in half.
    let composer = ascii
        .lines()
        .find(|l| l.contains("> "))
        .expect("the composer row");
    assert!(
        composer.contains(CURSOR_BLOCK),
        "no cursor block in: {composer}"
    );
    assert_eq!(display_width(composer), 23);
}

#[test]
fn svg_escapes_xml_metacharacters() {
    let mut screen = Screen::new(30, 2);
    screen.feed(b"<tag> & \"q\" x");
    let svg = render_svg(&screen.styled_runs(), screen.cursor(), "note <&>");

    assert!(svg.contains("&lt;tag&gt; &amp; \"q\" x"), "{svg}");
    assert!(!svg.contains("<tag>"), "a raw < would break the XML: {svg}");
    assert!(svg.contains("note &lt;&amp;&gt;"), "{svg}");

    // Still a well-formed document: one open tag per close tag, wrapped in
    // a single <svg> root.
    assert!(svg.starts_with("<svg "), "{svg}");
    assert!(svg.trim_end().ends_with("</svg>"), "{svg}");
    assert_eq!(
        svg.matches("<text").count(),
        svg.matches("</text>").count(),
        "{svg}"
    );
}

#[test]
fn svg_cursor_outline_present() {
    let mut screen = Screen::new(30, 2);
    screen.feed(b"ab");

    let svg = render_svg(&screen.styled_runs(), (1, 4), "");
    let rects: Vec<&str> = svg
        .lines()
        .filter(|l| l.contains("fill=\"none\""))
        .collect();
    assert_eq!(rects.len(), 1, "exactly one cursor outline: {svg}");
    assert!(
        rects[0].contains("x=\"48\""),
        "16 margin + 4 cells of 8 px: {}",
        rects[0]
    );
    assert!(
        rects[0].contains("y=\"34\""),
        "16 margin + 1 row of 18 px: {}",
        rects[0]
    );
    assert!(rects[0].contains("stroke=\"#ffffff\""), "{}", rects[0]);

    // It follows the cursor.
    let home = render_svg(&screen.styled_runs(), (0, 0), "");
    assert!(
        home.contains("x=\"16\"") && home.contains("y=\"16\""),
        "{home}"
    );

    // A styled run gets its background rect in front of its own text.
    let mut screen = Screen::new(20, 1);
    screen.feed(b"\x1b[41mR");
    let svg = render_svg(&screen.styled_runs(), screen.cursor(), "");
    assert!(
        svg.contains("<rect x=\"16\" y=\"16\" width=\"8\" height=\"18\" fill=\"#cd0000\"/>"),
        "{svg}"
    );
    assert!(svg.contains(">R</text>"), "{svg}");
}

/// Styles are the reason the frame is captured as runs rather than text: an
/// agent reading the artifact has to see what was emphasised.
#[test]
fn styled_runs_carry_fg_bg_and_modifier() {
    let mut screen = Screen::new(20, 1);
    screen.feed(b"\x1b[1;31;4mB\x1b[0mplain\x1b[48;2;1;2;3m G");
    let runs = screen.styled_runs();
    let row = &runs[0].runs;
    assert_eq!(row.len(), 3, "{row:?}");

    assert_eq!(run_text(row, 0), "B");
    assert_eq!(row[0].fg.as_deref(), Some("#cd0000"));
    assert!(row[0].modifier.has(Modifier::BOLD));
    assert!(row[0].modifier.has(Modifier::UNDERLINE));
    assert!(!row[0].modifier.has(Modifier::ITALIC));
    assert_eq!(row[0].modifier.names(), ["bold", "underline"]);

    assert_eq!(run_text(row, 1), "plain");
    assert_eq!(row[1].fg, None);
    assert!(row[1].modifier.is_empty());

    assert_eq!(run_text(row, 2), " G");
    assert_eq!(row[2].bg.as_deref(), Some("#010203"));
    assert_eq!(
        row[2].cells, 2,
        "the leading space is content the run keeps"
    );
}
