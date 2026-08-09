use crate::engine::TermScreen;

/// Answers the terminal queries applications send to their terminal.
///
/// The inner emulator (vt100) only parses — nothing ever replied — so apps
/// that probe their terminal on startup (vim's t_RV, crossterm's
/// keyboard-enhancement check, nvim's background-color query) hung until
/// their internal timeouts, which is why some TUIs took seconds to start
/// inside zodiac. This scans raw pty output for the common queries and
/// synthesizes the answers a plain xterm-256color would give, written back
/// to the pane as input. Sequences split across read chunks are joined via
/// a small carry buffer; anything longer than a real query (e.g. an OSC
/// title set spanning chunks) is not carried.
pub struct QueryScanner {
    carry: Vec<u8>,
}

const CARRY_MAX: usize = 256;

enum Parsed {
    /// Consumed this many bytes (any reply already appended).
    Done(usize),
    /// Sequence runs past the end of the buffer.
    Incomplete,
}

impl QueryScanner {
    pub fn new() -> Self {
        Self { carry: Vec::new() }
    }

    /// Scan one pty output chunk; returns reply bytes to feed back as input.
    /// `cell` is the outer terminal's cell size in px (0,0 = unknown) for
    /// the pixel-size window ops.
    pub fn scan(&mut self, chunk: &[u8], screen: &dyn TermScreen, cell: (u16, u16)) -> Vec<u8> {
        let owned;
        let buf: &[u8] = if self.carry.is_empty() {
            chunk
        } else {
            let mut c = std::mem::take(&mut self.carry);
            c.extend_from_slice(chunk);
            owned = c;
            &owned
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                i += 1;
                continue;
            }
            match parse_seq(&buf[i..], screen, cell, &mut out) {
                Parsed::Done(n) => i += n.max(1),
                Parsed::Incomplete => {
                    let tail = &buf[i..];
                    if tail.len() <= CARRY_MAX {
                        self.carry = tail.to_vec();
                    }
                    break;
                }
            }
        }
        out
    }
}

fn parse_seq(s: &[u8], screen: &dyn TermScreen, cell: (u16, u16), out: &mut Vec<u8>) -> Parsed {
    match s.get(1) {
        None => Parsed::Incomplete,
        Some(b'[') => parse_csi(s, screen, cell, out),
        Some(b']') => parse_string(s, out, respond_osc),
        Some(b'P') => parse_string(s, out, respond_dcs),
        Some(_) => Parsed::Done(1),
    }
}

fn parse_csi(s: &[u8], screen: &dyn TermScreen, cell: (u16, u16), out: &mut Vec<u8>) -> Parsed {
    let mut j = 2;
    // parameter bytes (0x30–0x3f) and intermediates (0x20–0x2f)
    while j < s.len() && (0x20..=0x3f).contains(&s[j]) {
        j += 1;
    }
    let Some(&fin) = s.get(j) else {
        return Parsed::Incomplete;
    };
    if !(0x40..=0x7e).contains(&fin) {
        return Parsed::Done(j);
    }
    respond_csi(&s[2..j], fin, screen, cell, out);
    Parsed::Done(j + 1)
}

/// OSC / DCS: body runs until BEL or ST (ESC \).
fn parse_string(s: &[u8], out: &mut Vec<u8>, respond: fn(&[u8], &mut Vec<u8>)) -> Parsed {
    let mut j = 2;
    loop {
        match s.get(j) {
            None => return Parsed::Incomplete,
            Some(0x07) => {
                respond(&s[2..j], out);
                return Parsed::Done(j + 1);
            }
            Some(0x1b) => match s.get(j + 1) {
                None => return Parsed::Incomplete,
                Some(b'\\') => {
                    respond(&s[2..j], out);
                    return Parsed::Done(j + 2);
                }
                // aborted string — rescan from the inner ESC
                Some(_) => return Parsed::Done(j),
            },
            Some(_) => j += 1,
        }
    }
}

fn respond_csi(body: &[u8], fin: u8, screen: &dyn TermScreen, cell: (u16, u16), out: &mut Vec<u8>) {
    match fin {
        // DA1: VT220 with ANSI color. Also what resolves crossterm's
        // keyboard-enhancement probe (kitty query + DA1; a DA1 reply with
        // no kitty reply means "not supported" — no timeout).
        b'c' if body.is_empty() || body == b"0" => out.extend_from_slice(b"\x1b[?62;22c"),
        // DA2: xterm-ish version triple (vim's t_RV waits on this)
        b'c' if body.first() == Some(&b'>') => out.extend_from_slice(b"\x1b[>41;354;0c"),
        // DSR 5: device OK
        b'n' if body == b"5" => out.extend_from_slice(b"\x1b[0n"),
        // DSR 6 / DECXCPR: cursor position report
        b'n' if body == b"6" || body == b"?6" => {
            let (r, c) = screen.cursor_position();
            let q = if body.first() == Some(&b'?') { "?" } else { "" };
            out.extend_from_slice(format!("\x1b[{q}{};{}R", r + 1, c + 1).as_bytes());
        }
        // DECRQM: report mode state; 0 (not recognized) for anything vt100
        // doesn't emulate, so apps skip the feature instead of timing out.
        b'p' if body.first() == Some(&b'?') && body.last() == Some(&b'$') => {
            let digits = &body[1..body.len() - 1];
            if let Ok(mode) = std::str::from_utf8(digits).unwrap_or("").parse::<u32>() {
                let val = decrqm_value(mode, screen);
                out.extend_from_slice(format!("\x1b[?{mode};{val}$y").as_bytes());
            }
        }
        // XTVERSION
        b'q' if body == b">" || body == b">0" => {
            out.extend_from_slice(b"\x1bP>|zodiac 0.1.0\x1b\\")
        }
        // window ops: text-area size in characters
        b't' if body == b"18" => {
            let (rows, cols) = screen.size();
            out.extend_from_slice(format!("\x1b[8;{rows};{cols}t").as_bytes());
        }
        // text-area size in pixels — needed by image tools to derive cell
        // size; answered only once the attached client reported one.
        b't' if body == b"14" && cell.0 > 0 => {
            let (rows, cols) = screen.size();
            let (w, h) = (cols * cell.0, rows * cell.1);
            out.extend_from_slice(format!("\x1b[4;{h};{w}t").as_bytes());
        }
        // cell size in pixels
        b't' if body == b"16" && cell.0 > 0 => {
            out.extend_from_slice(format!("\x1b[6;{};{}t", cell.1, cell.0).as_bytes());
        }
        _ => {}
    }
}

fn decrqm_value(mode: u32, screen: &dyn TermScreen) -> u8 {
    // 1 = set, 2 = reset, 0 = not recognized
    let on = match mode {
        1 => screen.application_cursor(),
        25 => !screen.hide_cursor(),
        1049 => screen.alternate_screen(),
        2004 => screen.bracketed_paste(),
        2026 => screen.synchronized_update(),
        _ => return 0,
    };
    if on {
        1
    } else {
        2
    }
}

fn respond_osc(body: &[u8], out: &mut Vec<u8>) {
    match body {
        // fg/bg color queries: report white-on-black (apps that pick a theme
        // from the background will detect a dark terminal)
        b"10;?" => out.extend_from_slice(b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
        b"11;?" => out.extend_from_slice(b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        _ => {}
    }
}

fn respond_dcs(body: &[u8], out: &mut Vec<u8>) {
    // XTGETTCAP: a definitive "invalid request" beats a timeout
    if body.starts_with(b"+q") {
        out.extend_from_slice(b"\x1bP0+r\x1b\\");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> vt100::Parser {
        vt100::Parser::new(24, 80, 0)
    }

    #[test]
    fn da1_and_cpr() {
        let p = screen();
        let mut q = QueryScanner::new();
        let out = q.scan(b"\x1b[c\x1b[6n", p.screen(), (0, 0));
        assert_eq!(out, b"\x1b[?62;22c\x1b[1;1R");
    }

    #[test]
    fn split_across_chunks() {
        let p = screen();
        let mut q = QueryScanner::new();
        assert!(q.scan(b"hello\x1b[", p.screen(), (0, 0)).is_empty());
        assert_eq!(q.scan(b">c world", p.screen(), (0, 0)), b"\x1b[>41;354;0c");
    }

    #[test]
    fn osc_color_query_but_not_title_set() {
        let p = screen();
        let mut q = QueryScanner::new();
        let out = q.scan(b"\x1b]0;my title\x07\x1b]11;?\x1b\\", p.screen(), (0, 0));
        assert_eq!(out, b"\x1b]11;rgb:0000/0000/0000\x1b\\");
    }

    #[test]
    fn decrqm_known_and_unknown() {
        let p = screen();
        let mut q = QueryScanner::new();
        let out = q.scan(b"\x1b[?2026$p\x1b[?2004$p", p.screen(), (0, 0));
        assert_eq!(out, b"\x1b[?2026;2$y\x1b[?2004;2$y");
    }

    #[test]
    fn xtgettcap_gets_invalid_reply() {
        let p = screen();
        let mut q = QueryScanner::new();
        let out = q.scan(b"\x1bP+q544e\x1b\\", p.screen(), (0, 0));
        assert_eq!(out, b"\x1bP0+r\x1b\\");
    }

    #[test]
    fn kitty_probe_no_reply() {
        let p = screen();
        let mut q = QueryScanner::new();
        assert!(q.scan(b"\x1b[?u", p.screen(), (0, 0)).is_empty());
    }

    #[test]
    fn oversized_partial_not_carried() {
        let p = screen();
        let mut q = QueryScanner::new();
        let mut chunk = b"\x1b]0;".to_vec();
        chunk.extend(std::iter::repeat_n(b'x', CARRY_MAX + 10));
        assert!(q.scan(&chunk, p.screen(), (0, 0)).is_empty());
        // the dangling title set was dropped, not carried into the next chunk
        assert_eq!(
            q.scan(b"more\x07\x1b[c", p.screen(), (0, 0)),
            b"\x1b[?62;22c"
        );
    }

    /// One row per query the scanner answers (or deliberately ignores) on a
    /// fresh default screen. Phase 1 shrinks the scanner to whatever the
    /// replacement engine doesn't answer natively — this table is the
    /// behavior contract it must preserve.
    #[test]
    fn query_reply_table() {
        type Case = (&'static str, &'static [u8], (u16, u16), &'static [u8]);
        #[rustfmt::skip]
        let cases: &[Case] = &[
            ("DA1",                 b"\x1b[c",        (0, 0),   b"\x1b[?62;22c"),
            ("DA1 explicit 0",      b"\x1b[0c",       (0, 0),   b"\x1b[?62;22c"),
            ("DA2",                 b"\x1b[>c",       (0, 0),   b"\x1b[>41;354;0c"),
            ("DSR 5 device ok",     b"\x1b[5n",       (0, 0),   b"\x1b[0n"),
            ("DSR 6 CPR",           b"\x1b[6n",       (0, 0),   b"\x1b[1;1R"),
            ("DECXCPR",             b"\x1b[?6n",      (0, 0),   b"\x1b[?1;1R"),
            ("DECRQM 1 app cursor", b"\x1b[?1$p",     (0, 0),   b"\x1b[?1;2$y"),
            ("DECRQM 25 cursor",    b"\x1b[?25$p",    (0, 0),   b"\x1b[?25;1$y"),
            ("DECRQM 1049 primary", b"\x1b[?1049$p",  (0, 0),   b"\x1b[?1049;2$y"),
            ("DECRQM 2004 paste",   b"\x1b[?2004$p",  (0, 0),   b"\x1b[?2004;2$y"),
            ("DECRQM 2026 sync",    b"\x1b[?2026$p",  (0, 0),   b"\x1b[?2026;2$y"),
            ("DECRQM 1000 mouse",   b"\x1b[?1000$p",  (0, 0),   b"\x1b[?1000;0$y"),
            ("XTVERSION",           b"\x1b[>q",       (0, 0),   b"\x1bP>|zodiac 0.1.0\x1b\\"),
            ("XTVERSION explicit",  b"\x1b[>0q",      (0, 0),   b"\x1bP>|zodiac 0.1.0\x1b\\"),
            ("18t chars",           b"\x1b[18t",      (0, 0),   b"\x1b[8;24;80t"),
            ("16t cell px",         b"\x1b[16t",      (10, 20), b"\x1b[6;20;10t"),
            ("16t no cell yet",     b"\x1b[16t",      (0, 0),   b""),
            ("14t text px",         b"\x1b[14t",      (10, 20), b"\x1b[4;480;800t"),
            ("14t no cell yet",     b"\x1b[14t",      (0, 0),   b""),
            ("OSC 10 fg",           b"\x1b]10;?\x07", (0, 0),   b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            ("OSC 11 bg",           b"\x1b]11;?\x07", (0, 0),   b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
            ("XTGETTCAP",           b"\x1bP+q544e\x1b\\", (0, 0), b"\x1bP0+r\x1b\\"),
            ("kitty kbd probe",     b"\x1b[?u",       (0, 0),   b""),
            ("DSR bare",            b"\x1b[n",        (0, 0),   b""),
        ];
        for (name, input, cell, expect) in cases {
            let p = screen();
            let mut q = QueryScanner::new();
            let out = q.scan(input, p.screen(), *cell);
            assert_eq!(&out, expect, "case {name}");
        }
    }

    /// The carry buffer must assemble a query delivered one byte per chunk —
    /// the worst read-boundary case a slow pty can produce.
    #[test]
    fn byte_at_a_time_carry() {
        let p = screen();
        let mut q = QueryScanner::new();
        let query = b"\x1b[?2026$p";
        let mut out = Vec::new();
        for b in query {
            out.extend(q.scan(&[*b], p.screen(), (0, 0)));
        }
        assert_eq!(out, b"\x1b[?2026;2$y");
    }

    /// DECRQM answers must track live screen state, not defaults.
    #[test]
    fn decrqm_tracks_screen_state() {
        let mut p = screen();
        p.process(b"\x1b[?1049h\x1b[?2004h");
        let mut q = QueryScanner::new();
        let out = q.scan(b"\x1b[?1049$p\x1b[?2004$p", p.screen(), (0, 0));
        assert_eq!(out, b"\x1b[?1049;1$y\x1b[?2004;1$y");
    }

    /// CPR reports the cursor where the app actually moved it.
    #[test]
    fn cpr_tracks_cursor_position() {
        let mut p = screen();
        p.process(b"\x1b[5;10H");
        let mut q = QueryScanner::new();
        assert_eq!(q.scan(b"\x1b[6n", p.screen(), (0, 0)), b"\x1b[5;10R");
    }
}
