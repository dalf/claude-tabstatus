//! Sections 1d, 1e, 2, 2b, 2c and 3: the length policy, the ssh prefix, the
//! JSON-safety pass, and where the glyph goes.

use crate::location;
use crate::sh::{
    char_count, env_or, env_set, env_str, pat_first_chars, pat_has_cntrl, pat_strip_prefix,
    pat_strip_trailing_nonprint, pat_take_char, utf8_repair,
};

fn ellipsis() -> Vec<u8> {
    // Repaired like any other display string: the ellipsis is spliced into the
    // title after the sanitizer has run, so an invalid byte here would reach the
    // JSON line unexamined.
    utf8_repair(env_or("CCTAB_ELLIPSIS", "\u{2026}".as_bytes()))
}

/// `case $v in 0) off ;; [1-9]|[1-9][0-9]|[1-9][0-9][0-9]) ;; *) fallback ;;`
/// followed by a floor. Leading zeros are REJECTED rather than accepted:
/// `$((08))` is an illegal octal constant, which in dash aborts the script
/// outright and would render an empty title.
fn cap(var: &str, default: usize, floor: usize) -> Option<usize> {
    let raw = env_or(var, default.to_string().as_bytes());
    let n = if &raw[..] == b"0" {
        return None;
    } else if (1..=3).contains(&raw.len())
        && raw[0] >= b'1'
        && raw[0] <= b'9'
        && raw.iter().all(|b| b.is_ascii_digit())
    {
        let mut v = 0usize;
        for b in &raw {
            v = v * 10 + (b - b'0') as usize;
        }
        v
    } else {
        default
    };
    Some(if n < floor { floor } else { n })
}

/// Section 1d. A tab is narrow: Konsole gives one roughly 49-60 columns and
/// elides from the LEFT, Windows Terminal truncates from the RIGHT, and neither
/// default keeps the half you want.
///
///   * a path is cut at the FRONT, on a component boundary, because the last
///     components say where you are
///   * a repo@branch is cut at the BACK, because the repo name identifies the tab
///
/// The unit is a CHARACTER, in every locale, and there is no exemption: an
/// accented or emoji location is cut exactly like an ASCII one. The shell could
/// do neither. Its count was bytes or characters depending on the shell and the
/// locale (README limitation 3), so cutting a non-ASCII location by count risked
/// slicing a UTF-8 sequence in half, and the implementation therefore skipped the
/// cut entirely for any location OUTSIDE PRINTABLE ASCII - its guard was one
/// `case $s in *[!\ -~]*)`, so an ASCII control character or DEL exempted a
/// location too, not only a non-ASCII byte (limitation 4). That meant the one case
/// where a tab most needs shortening was the case that never got it. Decoding
/// UTF-8 here costs no fork and no locale lookup, so both go away together.
///
/// A character is not a COLUMN, and the budget above is columns: a CJK or emoji
/// location is now cut, but to 32 characters, which is up to 64 columns, so the
/// terminal still elides it. Closing that needs an East-Asian width table, which
/// this binary deliberately does not carry; the README says so.
pub fn apply_length_cap(place: Vec<u8>, in_repo: bool) -> Vec<u8> {
    let max = match cap("CCTAB_MAX_LOCATION", 32, 8) {
        Some(m) => m,
        None => return place,
    };
    if char_count(&place) <= max {
        return place;
    }
    let ell = ellipsis();
    let mut place = place;
    if in_repo {
        // Keep the first max - 1 characters and spend the last one on the marker.
        let mut out = pat_first_chars(&place, max - 1).to_vec();
        out.extend_from_slice(&ell);
        out
    } else {
        // Drop whole leading components while that helps. The marker costs two
        // columns here, because a cut on a boundary reads as "…/".
        let mut sep: &[u8] = b"/";
        while char_count(&place) + 2 > max {
            match location::peel_leading_component(&place) {
                Some(rest) => place = rest,
                None => break,
            }
        }
        if char_count(&place) + 2 > max {
            // One component left and it still does not fit, so cut inside it
            // and drop the "/" from the marker: there is no boundary left to
            // mark, and that buys back the column it was using.
            let n = char_count(&place) + 1 - max;
            place = pat_strip_prefix(&place, n).to_vec();
            sep = b"";
        }
        let mut out = ell;
        out.extend_from_slice(sep);
        out.extend_from_slice(&place);
        out
    }
}

/// Section 1e. Only when this really is an ssh session, because the ABSENCE of
/// a prefix is how a local session is recognized. If the session says ssh but
/// no name resolves, the prefix becomes a literal `ssh` rather than nothing: an
/// empty prefix would render byte for byte like a local session and invert the
/// one signal this design rests on.
pub fn apply_ssh_prefix(place: Vec<u8>) -> Vec<u8> {
    if !env_set("SSH_CONNECTION") && !env_set("SSH_TTY") {
        return place;
    }
    let mut h = location::hostname();
    // The domain goes in every case, so a.b.c renders as a - unless the name is
    // all digits and dots, because chopping 192.168.1.5 to `192` names nothing.
    if h.iter().any(|&b| !(b.is_ascii_digit() || b == b'.')) {
        if let Some(i) = h.iter().position(|&b| b == b'.') {
            h.truncate(i);
        }
    }
    // A cap of its own, because ${_h%%.*} only removes a DOMAIN and the hosts
    // one actually ssh into on cloud and k8s boxes are single labels up to the
    // kernel's 64 bytes. Windows Terminal truncates from the RIGHT, so an
    // uncapped host would keep the host and lose the location entirely.
    if let Some(hmax) = cap("CCTAB_MAX_HOST", 16, 4) {
        // Characters, and no non-ASCII exemption, for the same reason as the
        // location cap above.
        if char_count(&h) > hmax {
            let mut k = pat_first_chars(&h, hmax - 1).to_vec();
            k.extend_from_slice(&ellipsis());
            h = k;
        }
    }
    if h.is_empty() {
        h = b"ssh".to_vec();
    }
    h.push(b':');
    h.extend_from_slice(&place);
    h
}

/// Section 2. `"` and `\` would break the JSON string literal, control
/// characters are illegal inside one, and a newline would also break the
/// one-line-per-hook contract. Deleting them is enough - nothing downstream
/// needs the original bytes.
///
/// The 256-character bound is carried over from the shell, where this loop was
/// quadratic (measured 2.2s at 2048 characters) and section 1d's cap does not
/// bound the input because of the non-ASCII exemption above. It is not needed
/// here, and it is kept anyway: it is observable, so removing it is a behaviour
/// change and belongs to the deliberate-fix pass, not to the port.
///
/// Invalid UTF-8 is NOT this function's business: `location::place` and
/// `location::hostname` repair their own output, so what arrives here is always
/// a valid UTF-8 string that may still hold characters JSON cannot.
pub fn sanitize(place: Vec<u8>) -> Vec<u8> {
    let hostile = place.iter().any(|&b| b == b'"' || b == b'\\') || pat_has_cntrl(&place);
    if !hostile {
        return place;
    }
    let mut rest: &[u8] = &place;
    let mut out: Vec<u8> = Vec::with_capacity(place.len());
    let mut budget: i64 = 256;
    while !rest.is_empty() {
        if budget <= 0 {
            // Bound reached. Drop any trailing non-ASCII unit before marking the
            // cut: where the count unit is a byte the stop can land inside a
            // multibyte sequence, and half a sequence is precisely the invalid
            // byte this section exists to keep out of the JSON.
            for _ in 0..8 {
                match pat_strip_trailing_nonprint(&out) {
                    Some(keep) => out.truncate(keep),
                    None => break,
                }
            }
            out.extend_from_slice(&ellipsis());
            break;
        }
        budget -= 1;
        let (len, drop) = pat_take_char(rest);
        let ch = &rest[..len];
        rest = &rest[len..];
        if drop {
            continue;
        }
        out.extend_from_slice(ch);
    }
    // Unreachable by construction - every location section 1 can produce
    // carries a `/`, a `~` or an `@` - and kept as the belt to the braces.
    if out.is_empty() {
        out = b"?".to_vec();
    }
    out
}

/// Section 2b. $TMUX / $STY take the multiplexer case out: KONSOLE_* leaks into
/// any child launched from a Konsole shell, and into every pane of a tmux server
/// that was first started under Konsole, so inside a multiplexer those variables
/// say nothing about the terminal actually drawing the tab.
pub fn is_konsole() -> bool {
    !env_set("TMUX")
        && !env_set("STY")
        && (env_set("KONSOLE_VERSION") || env_set("KONSOLE_DBUS_SESSION"))
}

/// Section 3, plus the composition in the block after it.
///
/// Konsole's tab bar elides from the LEFT - QTabBar::setElideMode(Qt::ElideLeft)
/// at a hardcoded call site, which no config key reads - so a LEADING glyph is
/// the first thing cut and under Konsole the glyph goes last. Windows Terminal
/// truncates from the RIGHT, so there it goes first, which is also the safe
/// default for any terminal we cannot identify - including every session
/// reached over ssh, because the local terminal's variables do not travel.
pub fn title(edge: &[u8], place: Vec<u8>, konsole: bool) -> Vec<u8> {
    if edge == b"session-end" {
        return Vec::new();
    }
    // Repaired, like every other display string: a glyph override is spliced in
    // after the sanitizer, so it is the last place an invalid byte could reach
    // the JSON line.
    let glyph = utf8_repair(match edge {
        b"working" => env_or("CCTAB_GLYPH_WORKING", "\u{1f535}".as_bytes()),
        b"waiting" => env_or("CCTAB_GLYPH_WAITING", "\u{1f7e0}".as_bytes()),
        // idle, session-start, and any edge a future hooks.json adds that this
        // version does not know about yet. `notify` never reaches here.
        _ => env_or("CCTAB_GLYPH_IDLE", "\u{26aa}".as_bytes()),
    });
    if glyph.is_empty() {
        return place;
    }
    let pos = {
        let p = env_str("CCTAB_GLYPH_POS");
        if !p.is_empty() {
            p
        } else if konsole {
            b"suffix".to_vec()
        } else {
            b"prefix".to_vec()
        }
    };
    let mut out = Vec::with_capacity(place.len() + 2 * glyph.len() + 2);
    match &pos[..] {
        b"suffix" => {
            out.extend_from_slice(&place);
            out.push(b' ');
            out.extend_from_slice(&glyph);
        }
        b"both" => {
            out.extend_from_slice(&glyph);
            out.push(b' ');
            out.extend_from_slice(&place);
            out.push(b' ');
            out.extend_from_slice(&glyph);
        }
        // An unrecognised value falls through to prefix rather than failing.
        _ => {
            out.extend_from_slice(&glyph);
            out.push(b' ');
            out.extend_from_slice(&place);
        }
    }
    out
}
