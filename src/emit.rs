//! Sections 4 and 5: the two delivery mechanisms, because neither one covers
//! every edge.
//!
//!   * Most edges print ONE line of JSON carrying `terminalSequence` and Claude
//!     Code emits the bytes to its own terminal. Only notification/title OSCs
//!     (0, 1, 2, 9, 99, 777) and BEL are permitted there; anything else is
//!     silently dropped.
//!   * session-start and session-end write the pty DIRECTLY, because
//!     terminalSequence cannot carry them: SessionStart is too early - the TUI
//!     writer is not mounted yet, so the sequence is dropped - and the Konsole
//!     arming sequence is OSC 50, which is not on the allowlist above. Those two
//!     edges resolve the pty through /proc, so they are Linux-only.

use crate::sh::{self, env_set, env_str};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::FileTypeExt;

fn write_stdout(bytes: &[u8]) {
    let out = std::io::stdout();
    let mut lock = out.lock();
    let _ = lock.write_all(bytes);
    let _ = lock.flush();
}

/// Section 4. CCTAB_DRY_RUN=1 prints the computed title and emits nothing at
/// all. This is what makes the edge table testable with no Claude session.
pub fn dry_run(title: &[u8]) {
    let mut out = Vec::with_capacity(title.len() + 1);
    out.extend_from_slice(title);
    out.push(b'\n');
    write_stdout(&out);
}

/// The hook-protocol line. A raw ESC or BEL byte inside a JSON string is invalid
/// JSON, so those two travel as \u001b and \u0007.
///
/// The title is ESCAPED here rather than spliced in raw as the shell's
/// `printf '...%s...'` did. Section 2 does delete the `"`, `\` and control
/// characters it finds in the LOCATION, but the glyph and the ellipsis are
/// attached after it and came straight from the environment, so the shell could
/// be made to emit a broken line through `CCTAB_GLYPH_IDLE='q"x'` - and did emit
/// one for any name that was not valid UTF-8 (README limitation 2).
///
/// Making the writer responsible for its own syntax is what turns "the hook line
/// is valid JSON" from six upstream promises into one property of this function.
/// The repair in `location::place` and `location::hostname` still matters: it
/// decides whether an invalid byte becomes a visible U+FFFD before the length cap
/// counts it, rather than being escaped here at the very end.
pub fn json_line(title: &[u8]) {
    let mut out = Vec::with_capacity(title.len() + 72);
    out.extend_from_slice(b"{\"terminalSequence\":\"\\u001b]0;");
    escape_into(&mut out, title);
    out.extend_from_slice(b"\\u0007\",\"suppressOutput\":true}\n");
    write_stdout(&out);
}

/// Append `s` as the contents of a JSON string literal: `"` and `\` escaped,
/// control characters as \u00xx, and any byte that starts no valid UTF-8
/// sequence as U+FFFD, because JSON text has to be UTF-8.
fn escape_into(out: &mut Vec<u8>, s: &[u8]) {
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'"' || b == b'\\' {
            out.push(b'\\');
            out.push(b);
            i += 1;
        } else if b < 0x20 || b == 0x7f {
            out.extend_from_slice(format!("\\u{:04x}", b).as_bytes());
            i += 1;
        } else if b < 0x80 {
            out.push(b);
            i += 1;
        } else {
            match sh::utf8_len(&s[i..]) {
                Some(n) => {
                    out.extend_from_slice(&s[i..i + n]);
                    i += n;
                }
                None => {
                    out.extend_from_slice("\u{fffd}".as_bytes());
                    i += 1;
                }
            }
        }
    }
}

/// Resolve the session's pty from the environment. Hook subprocesses are
/// detached - fd 0 is /dev/null and `exec 3>/dev/tty` fails - so /dev/tty is not
/// usable here. CLAUDE_PID is exported into every hook subprocess.
///
/// Guard: if CLAUDE_PID is unset, or fd 1 does not resolve to a writable
/// character device under /dev/pts or /dev/tty, do nothing rather than retitle
/// an unrelated terminal. That covers a redirected or piped `claude -p`, and
/// every platform without /proc.
fn session_tty() -> Option<std::fs::File> {
    if !env_set("CLAUDE_PID") {
        return None;
    }
    let mut link = b"/proc/".to_vec();
    link.extend_from_slice(&env_str("CLAUDE_PID"));
    link.extend_from_slice(b"/fd/1");
    let target = sh::from_os(std::fs::read_link(sh::as_path(&link)).ok()?.into_os_string());
    if !(target.starts_with(b"/dev/pts/") || target.starts_with(b"/dev/tty")) {
        return None;
    }
    let md = std::fs::metadata(sh::as_path(&target)).ok()?;
    if !md.file_type().is_char_device() {
        return None;
    }
    // `[ -w "$tty" ]` in the shell; opening it is the same question asked once.
    OpenOptions::new().write(true).open(sh::as_path(&target)).ok()
}

/// One write, the way one `printf` was one write.
pub fn session_edge(is_start: bool, title: &[u8], konsole: bool) {
    let mut tty = match session_tty() {
        Some(f) => f,
        None => return,
    };
    let mut out: Vec<u8> = Vec::with_capacity(title.len() + 96);
    if is_start {
        if konsole {
            // Arm this tab: %w makes the OSC 0 payload the entire tab text.
            // Under Konsole's stock formats an OSC 0 title is invisible in the
            // tab, which is why Claude's own title never shows up there.
            // Konsole applies profile properties per tab, at runtime, in memory,
            // and never inherits them into new tabs or writes them to disk, so
            // every other tab keeps its default title by construction.
            //
            // SEAM: TabColor=#RRGGBB rides in this same OSC 50 property list -
            // and whoever adds it must also add TabColor=#000000 to the
            // session-end list below, or the colour outlives the session.
            out.extend_from_slice(
                b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07",
            );
        }
        out.extend_from_slice(b"\x1b]0;");
        out.extend_from_slice(title);
        out.push(0x07);
    } else {
        // We own restore: with the built-in terminal title disabled - which the
        // installer does, otherwise it repaints over ours every 960ms - Claude
        // Code no longer clears the title on exit either. The two formats below
        // are Konsole's compiled-in defaults, not whatever a customized profile
        // had.
        if konsole {
            out.extend_from_slice(
                b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07",
            );
        }
        out.extend_from_slice(b"\x1b]0;\x07");
    }
    let _ = tty.write_all(&out);
}
