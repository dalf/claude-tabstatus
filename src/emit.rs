//! The two delivery mechanisms, because neither one covers every edge.
//!
//!   * Outside tmux, most edges print ONE line of JSON carrying `terminalSequence`,
//!     and Claude Code emits the bytes to its terminal. Only notification/title OSCs
//!     (0, 1, 2, 9, 99, 777) and BEL are permitted there; anything else is
//!     silently dropped.
//!   * Inside tmux every painting edge writes raw OSC to the verified pane pty.
//!     Claude Code 2.1.274 wraps terminalSequence OSCs in tmux passthrough, which
//!     bypasses pane_title instead of updating our carrier. No verified pty means
//!     silence, never a JSON fallback that could leak the carrier to the outer tab.
//!   * session-start and session-end also paint DIRECTLY, because
//!     `terminalSequence` cannot carry them: SessionStart is too early - the TUI
//!     writer is not mounted yet, so the sequence is dropped - and the Konsole
//!     arming sequence is OSC 50, which is not on the allowlist above. On Linux the
//!     bytes go to the pty, resolved through /proc; macOS uses proc_pidfdinfo.
//!     On Windows there is no pty but
//!     there is the console Claude Code runs in, and its TITLE is set instead
//!     ([`sys::set_session_title`]), which the pseudo console forwards to the tab as
//!     an OSC 0; the Konsole arming has no console form and is not sent. Both are
//!     behind a headless guard. Native validation status is in docs/architecture.md.

use crate::config::Config;
use crate::mux::{Channel, Route};
use crate::surface::compose;
use crate::sys;
use std::fmt::Write as _;
use std::io::{self, Write};

/// Every byte this program prints on the paint path goes through here, and the
/// failure goes UP: `main` is the one place it becomes exit 0.
///
/// The flush runs even when the write failed, which is not belt and braces but
/// the pre-existing call order: a partially written line is still flushed to
/// whoever is reading, exactly as it was before this reported anything.
fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    let out = io::stdout();
    let mut lock = out.lock();
    let wrote = lock.write_all(bytes);
    lock.flush().and(wrote)
}

/// `CCTAB_DRY_RUN=1` prints the computed title and emits nothing at all. This is
/// what makes the edge table testable with no Claude session.
pub fn dry_run(title: &str) -> io::Result<()> {
    let mut out = String::with_capacity(title.len() + 1);
    out.push_str(title);
    out.push('\n');
    write_stdout(out.as_bytes())
}

/// The hook-protocol line. A raw ESC or BEL byte inside a JSON string is invalid
/// JSON, so those two travel as `\u001b` and `\u0007`.
///
/// The title is ESCAPED here rather than spliced in raw: the sanitizer cleans the
/// LOCATION, but the glyph and the ellipsis are attached after it straight from the
/// environment, so `CCTAB_GLYPH_IDLE='q"x'` would otherwise break the line. Making
/// the writer own its syntax turns "the hook line is valid JSON" from six upstream
/// promises into one property of this function.
pub fn json_line(title: &str) -> io::Result<()> {
    let mut out = String::with_capacity(title.len() + 72);
    out.push_str("{\"terminalSequence\":\"\\u001b]0;");
    escape_into(&mut out, title);
    out.push_str("\\u0007\",\"suppressOutput\":true}\n");
    write_stdout(out.as_bytes())
}

/// Update tmux's pane-title carrier without Claude Code's OSC passthrough layer.
/// Reuses the session pty guard and executes no subprocess on the hot path.
pub fn pane_title(title: &str, cfg: &Config) -> io::Result<bool> {
    let mut out = Vec::with_capacity(title.len() + 5);
    out.extend_from_slice(b"\x1b]0;");
    out.extend_from_slice(title.as_bytes());
    out.push(0x07);
    write_pty(cfg, &out)
}

/// Append `s` as the contents of a JSON string literal: `"` and `\` escaped, and
/// control characters as `\u00xx`. Nothing here has to worry about encoding,
/// because a `&str` cannot be invalid UTF-8 - which is the whole reason the title
/// is text by the time it arrives.
fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// Arm this tab and paint it, in one ordered buffer (not an atomic write).
///
/// One buffer and one guarded acquisition of the session's tab are why
/// the arming is composed here rather than sent by whoever decided it: the arming
/// has to precede the title in the same byte stream. WHETHER it belongs in this
/// buffer is [`crate::mux::route`]'s answer - `Channel::Direct` means the session's
/// own tab, which is this buffer, and `Channel::Clients` means the multiplexer's
/// clients, which is not.
pub fn session_start(title: &str, cfg: &Config, route: Route) -> io::Result<bool> {
    write_session(cfg, &startup_bytes(title, cfg, route), title)
}

fn startup_bytes(title: &str, cfg: &Config, route: Route) -> Vec<u8> {
    let caps = cfg.stack.leaf.caps();
    let mut out: Vec<u8> = Vec::with_capacity(title.len() + 96);
    if let Some(surface) = route.arms(Channel::Direct) {
        // %w makes the OSC 0 payload the entire tab text. Under Konsole's stock
        // formats an OSC 0 title is invisible in the tab, which is why Claude's
        // own title never shows up there. Konsole applies profile properties per
        // tab, at runtime, in memory, and never inherits them into new tabs or
        // writes them to disk, so every other tab keeps its default title by
        // construction.
        //
        // Inside tmux this pane is not the tab, so the sequence would be
        // swallowed; `tmux::arm_konsole` writes it to the attached client's pty
        // instead, which is why `route` answers `Channel::Clients` there and this
        // block is skipped.
        let _ = compose::push_arm(&mut out, surface.caps());
    }
    // The arming has to precede the title, or the tab is painted before it can
    // show what was painted.
    let _ = compose::push_title(&mut out, caps, title);
    out
}

/// Restore the tab and blank its title, in one ordered buffer.
pub fn session_end(cfg: &Config, route: Route) -> io::Result<bool> {
    let caps = cfg.stack.leaf.caps();
    let mut out: Vec<u8> = Vec::with_capacity(96);
    if let Some(surface) = route.arms(Channel::Direct) {
        // We own restore: with the built-in terminal title disabled - which the
        // installer does, otherwise it repaints over ours every 960ms - Claude
        // Code no longer clears the title on exit either. The two formats in the
        // capability row are Konsole's COMPILED-IN defaults, not whatever a
        // customized profile had, because that is all we can know.
        //
        // WHOSE row is [`crate::armed`]'s answer and not `cfg.stack.leaf`'s: this
        // hook runs an unbounded time after the one that armed, and a
        // `CCTAB_TERMINAL` changed in between used to turn this whole block off
        // and leave the tab governed by `%w` forever.
        let _ = compose::push_restore(&mut out, surface.caps());
    }
    // An EMPTY title is the unpaint, so it is composed as a title rather than
    // spelled as its own literal.
    let _ = compose::push_title(&mut out, caps, "");
    write_session(cfg, &out, "")
}

/// Deliver session-start or session-end: `bytes` to the pty - or, where the session's
/// tab is a console rather than a pty (Windows), `title` alone as that console's
/// title. Never where the layer above RENDERS the tab: the title is then that
/// layer's carrier, not a tab's. The test is the cap and not `mux.is_some()`,
/// because screen renders nothing and its title is still a tab's.
///
/// `Ok(false)` means skipped by the guard; `Ok(true)` means the write/API call
/// completed, not that the terminal applied it. An error can follow a partial
/// write, including a complete arm followed by a failed title. None of these
/// outcomes cancels a previously recorded restore obligation.
fn write_session(cfg: &Config, bytes: &[u8], title: &str) -> io::Result<bool> {
    if sys::HAS_SESSION_CONSOLE {
        return match (cfg.claude_pid.as_deref(), cfg.stack.renders_title()) {
            (Some(pid), false) => sys::set_session_title(pid, title),
            _ => Ok(false),
        };
    }
    write_pty(cfg, bytes)
}

/// Write to the session's pty, resolved by [`sys::session_tty`] under the
/// headless guard documented there.
fn write_pty(cfg: &Config, bytes: &[u8]) -> io::Result<bool> {
    write_to(cfg.claude_pid.as_deref().and_then(sys::session_tty), bytes)
}

fn write_to(tty: Option<impl Write>, bytes: &[u8]) -> io::Result<bool> {
    match tty {
        Some(mut tty) => tty.write_all(bytes).map(|()| true),
        // No pty is not a failure: a redirected `claude -p` has no tab, and there
        // is nothing to report to a hook whose output is a protocol.
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::armed::Armed;
    use crate::edge::Paint;
    use crate::surface::Surface;

    #[test]
    fn missing_or_invalid_session_destination_is_skipped_not_written() {
        let mut cfg = Config::for_test();
        for pid in [None, Some("0"), Some("not-a-pid")] {
            cfg.claude_pid = pid.map(Into::into);
            assert!(!write_session(&cfg, b"unused", "unused").unwrap());
            assert!(!pane_title("unused", &cfg).unwrap());
        }
    }

    #[test]
    fn combined_startup_can_fail_after_the_arm_was_written() {
        if !sys::HAS_SESSION_TTY {
            return;
        }
        let mut cfg = Config::for_test();
        cfg.stack.leaf = Surface::Konsole;
        let route = crate::mux::route(&cfg.stack, Paint::SessionStart, Armed::assumed(cfg.stack.leaf));
        let bytes = startup_bytes("title", &cfg, route);
        let arm = Surface::Konsole.caps().arming.as_ref().unwrap().pair().0;
        assert_eq!(bytes, [arm, b"\x1b]0;title\x07"].concat());

        struct FailAfter {
            accepted: Vec<u8>,
            limit: usize,
        }
        impl Write for FailAfter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let n = bytes.len().min(self.limit - self.accepted.len());
                if n == 0 {
                    return Err(io::Error::other("injected write failure"));
                }
                self.accepted.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        for limit in [0, 1, arm.len(), arm.len() + 3] {
            let mut writer = FailAfter { accepted: Vec::new(), limit };
            assert!(write_to(Some(&mut writer), &bytes).is_err());
            assert_eq!(writer.accepted, bytes[..limit]);
        }
        let mut complete = Vec::new();
        assert!(write_to(Some(&mut complete), &bytes).unwrap());
        assert_eq!(complete, bytes);
        assert!(!write_to(None::<Vec<u8>>, &bytes).unwrap());
    }

    fn escaped(s: &str) -> String {
        let mut out = String::new();
        escape_into(&mut out, s);
        out
    }

    #[test]
    fn a_quote_or_a_backslash_from_a_glyph_override_cannot_break_the_line() {
        assert_eq!(escaped("q\"x"), "q\\\"x");
        assert_eq!(escaped("a\\b"), "a\\\\b");
    }

    #[test]
    fn control_characters_travel_as_escapes() {
        assert_eq!(escaped("\u{1b}"), "\\u001b");
        assert_eq!(escaped("\u{7}"), "\\u0007");
        assert_eq!(escaped("\n"), "\\u000a");
        assert_eq!(escaped("\u{7f}"), "\\u007f");
    }

    #[test]
    fn everything_else_travels_raw() {
        assert_eq!(escaped("\u{26aa} ~/code/x"), "\u{26aa} ~/code/x");
        // U+0085 is a control character to the sanitizer but legal raw in JSON.
        assert_eq!(escaped("\u{85}"), "\u{85}");
        assert_eq!(escaped("\u{fffd}"), "\u{fffd}");
    }
}
