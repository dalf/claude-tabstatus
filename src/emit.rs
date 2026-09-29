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
//!     bytes go to the pty, resolved through /proc. On Windows there is no pty but
//!     there is the console Claude Code runs in, and its TITLE is set instead
//!     ([`sys::set_session_title`]), which the pseudo console forwards to the tab as
//!     an OSC 0; the Konsole arming has no console form and is not sent. Both are
//!     behind a headless guard, and elsewhere (macOS) nothing is painted.

use crate::config::{Config, Terminal};
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
pub fn pane_title(title: &str, cfg: &Config) -> io::Result<()> {
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

/// Konsole's per-tab title format, set to "the title the shell sent" and back to
/// Konsole's COMPILED-IN defaults. Named here rather than spelled at each use,
/// because inside tmux the same two byte strings go to a tmux CLIENT's pty
/// instead of to this pane - and an arming with no matching restore is this
/// project's named defect.
///
/// SEAM: TabColor=#RRGGBB rides in this same property list - and whoever adds it
/// must add TabColor=#000000 to the restore, or the colour outlives the session.
pub const KONSOLE_ARM: &[u8] = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07";
pub const KONSOLE_RESTORE: &[u8] =
    b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07";

/// Arm this tab and paint it, in one write.
pub fn session_start(title: &str, cfg: &Config) -> io::Result<()> {
    let mut out: Vec<u8> = Vec::with_capacity(title.len() + 96);
    if cfg.terminal == Terminal::Konsole && cfg.tmux.is_none() {
        // %w makes the OSC 0 payload the entire tab text. Under Konsole's stock
        // formats an OSC 0 title is invisible in the tab, which is why Claude's
        // own title never shows up there. Konsole applies profile properties per
        // tab, at runtime, in memory, and never inherits them into new tabs or
        // writes them to disk, so every other tab keeps its default title by
        // construction.
        //
        // Inside tmux this pane is not the tab, so the sequence would be
        // swallowed; `tmux::arm_konsole` writes it to the attached client's pty
        // instead, which is why the Konsole test above also asks for no tmux.
        out.extend_from_slice(KONSOLE_ARM);
    }
    // The arming has to precede the title, or the tab is painted before it can
    // show what was painted.
    out.extend_from_slice(b"\x1b]0;");
    out.extend_from_slice(title.as_bytes());
    out.push(0x07);
    write_session(cfg, &out, title)
}

/// Restore the tab and blank its title, in one write.
pub fn session_end(cfg: &Config) -> io::Result<()> {
    let mut out: Vec<u8> = Vec::with_capacity(96);
    if cfg.terminal == Terminal::Konsole && cfg.tmux.is_none() {
        // We own restore: with the built-in terminal title disabled - which the
        // installer does, otherwise it repaints over ours every 960ms - Claude
        // Code no longer clears the title on exit either. The two formats below
        // are Konsole's COMPILED-IN defaults, not whatever a customized profile
        // had, because that is all we can know.
        out.extend_from_slice(KONSOLE_RESTORE);
    }
    out.extend_from_slice(b"\x1b]0;\x07");
    write_session(cfg, &out, "")
}

/// Deliver session-start or session-end: `bytes` to the pty - or, where the session's
/// tab is a console rather than a pty (Windows), `title` alone as that console's
/// title. Never inside tmux there: the title is then tmux's carrier, not a tab's.
fn write_session(cfg: &Config, bytes: &[u8], title: &str) -> io::Result<()> {
    if sys::HAS_SESSION_CONSOLE {
        return match (cfg.claude_pid.as_deref(), &cfg.tmux) {
            // Refused or painted, it is not a failure: see `write_pty`.
            (Some(pid), None) => sys::set_session_title(pid, title).map(drop),
            _ => Ok(()),
        };
    }
    write_pty(cfg, bytes)
}

/// Write to the session's pty, resolved by [`sys::session_tty`] under the
/// headless guard documented there.
fn write_pty(cfg: &Config, bytes: &[u8]) -> io::Result<()> {
    match cfg.claude_pid.as_deref().and_then(sys::session_tty) {
        Some(mut tty) => tty.write_all(bytes),
        // No pty is not a failure: a redirected `claude -p` has no tab, and there
        // is nothing to report to a hook whose output is a protocol.
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
