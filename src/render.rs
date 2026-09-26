//! The length policy, the ssh prefix, the JSON-safety pass, and where the glyph
//! goes - applied in that order, which is load-bearing. The cap's ellipsis passes
//! THROUGH the sanitizer, so a hostile one is deleted; the glyph and the
//! sanitizer's own overflow marker are attached after it and are only escaped by
//! the writer.
//!
//! [`compose`] is the only way in, and the steps below it are private, so the two
//! callers that want a title - the paint path and `doctor` - cannot compose them
//! in different orders.
//!
//! Everything here is `&str` and `char`. `location::place` and
//! `location::hostname` repair their own output and `config` repairs the four
//! environment overrides, so what arrives is always valid UTF-8 that may still
//! hold characters JSON cannot.

use crate::config::{Cap, Config, GlyphPos};
use crate::edge::Paint;
use crate::location::{self, Place};
use crate::text;

/// One location walk, two answers.
///
/// Both come out of the same pipeline because there is exactly one caller that
/// wants each - the tab title outside tmux, the record inside it - and walking
/// twice would double the only part of this binary that touches the filesystem.
pub struct Composed {
    /// The location alone, after the cap, the ssh prefix and the JSON-safety
    /// pass. No glyph: inside tmux the glyph is not in the record at all.
    pub place: String,
    /// The glyph and the location, composed - what paints a tab.
    pub title: String,
}

/// Where this session is, painted: the location walk, the length policy, the ssh
/// prefix, the JSON-safety pass and the glyph, in the one order that is correct.
pub fn compose(paint: Paint, cfg: &Config) -> Composed {
    let cwd = location::cwd(cfg);
    let place = location::place(&cwd, cfg);
    let place = apply_length_cap(place, cfg);
    let place = apply_ssh_prefix(place, cfg);
    let place = sanitize(place, cfg);
    let title = title(paint, &place, cfg);
    Composed { place, title }
}

/// A tab is narrow: Konsole gives one roughly 49-60 columns and elides from the
/// LEFT, Windows Terminal truncates from the RIGHT, and neither default keeps the
/// half you want. So a path is cut at the FRONT on a component boundary, because
/// the last components say where you are, and a `repo@branch` is cut at the BACK,
/// because the repo name identifies the tab.
///
/// The unit is a CHARACTER, in every locale and with no exemption. A character is
/// not a COLUMN, so a CJK or emoji location is cut to 32 characters which may be
/// 64 columns and the terminal still elides it; closing that needs an East-Asian
/// width table this binary deliberately does not carry. README says so.
fn apply_length_cap(place: Place, cfg: &Config) -> String {
    let Cap::Max(max) = cfg.max_location else {
        return place.into_text();
    };
    if place.text().chars().count() <= max {
        return place.into_text();
    }
    match place {
        Place::Repo(s) => {
            // Keep the first max - 1 characters and spend the last one on the
            // marker.
            let mut out: String = s.chars().take(max - 1).collect();
            out.push_str(&cfg.ellipsis);
            out
        }
        Place::Path(s) => {
            // Drop whole leading components while that helps. The marker costs
            // two columns here, because a cut on a boundary reads as "…/".
            let mut rest: &str = &s;
            // Counted once and then decremented by what each peel removed: the
            // whole string is walked one time rather than once per component.
            let mut left = rest.chars().count();
            while left + 2 > max {
                match peel_leading_component(rest) {
                    Some(r) => {
                        left -= rest[..rest.len() - r.len()].chars().count();
                        rest = r;
                    }
                    None => break,
                }
            }
            if left + 2 > max {
                // One component left and it still does not fit, so cut inside it
                // and drop the "/" from the marker: there is no boundary left to
                // mark, and that buys back the column it was using.
                let inner: String = rest.chars().skip(left + 1 - max).collect();
                return format!("{}{}", cfg.ellipsis, inner);
            }
            format!("{}/{}", cfg.ellipsis, rest)
        }
    }
}

/// Everything after the first `/`, or `None` when that would leave nothing - or
/// when there was no `/` to cut at.
fn peel_leading_component(place: &str) -> Option<&str> {
    place
        .split_once('/')
        .map(|(_, rest)| rest)
        .filter(|rest| !rest.is_empty())
}

/// Only when this really is an ssh session, because the ABSENCE of a prefix is
/// how a local session is recognized.
fn apply_ssh_prefix(place: String, cfg: &Config) -> String {
    if !cfg.ssh {
        return place;
    }
    let mut host = location::hostname(cfg).unwrap_or_default();
    // The domain goes in every case, so a.b.c renders as a - unless the name is
    // all digits and dots, because chopping 192.168.1.5 to `192` names nothing.
    if host.chars().any(|c| !(c.is_ascii_digit() || c == '.')) {
        if let Some(i) = host.find('.') {
            host.truncate(i);
        }
    }
    // A cap of its own, because dropping a DOMAIN only shortens a name that has
    // one, and the hosts one actually ssh into on cloud and k8s boxes are single
    // labels up to the kernel's 64 bytes. Windows Terminal truncates from the
    // RIGHT, so an uncapped host would keep the host and lose the location
    // entirely.
    if let Cap::Max(max) = cfg.max_host {
        if host.chars().count() > max {
            let mut cut: String = host.chars().take(max - 1).collect();
            cut.push_str(&cfg.ellipsis);
            host = cut;
        }
    }
    // An empty prefix would render byte for byte like a LOCAL session and invert
    // the one signal this design rests on, so it becomes a literal `ssh` instead.
    // There are two ways to arrive here with nothing, and the test covers both:
    // no name resolved at all (`hostname` gave `None` above), and a name that was
    // all domain, because `CCTAB_HOST=.example.com` chops to the empty string.
    if host.is_empty() {
        host.push_str("ssh");
    }
    host.push(':');
    host.push_str(&place);
    host
}

/// `"` and `\` would break the JSON string literal, control characters are
/// illegal inside one, and a newline would also break the one-line-per-hook
/// contract. Deleting them is enough - nothing downstream needs the original
/// characters.
///
/// Two properties are observable and therefore deliberate: the loop is SKIPPED
/// unless the string holds something hostile, so a long clean location with the cap
/// off is never truncated; and the 256-character budget counts INPUT characters,
/// dropped ones included, so a deleted quote costs a slot.
fn sanitize(place: String, cfg: &Config) -> String {
    let hostile = place
        .chars()
        .any(|c| c == '"' || c == '\\' || text::is_cntrl(c));
    if !hostile {
        return place;
    }
    let mut out = String::with_capacity(place.len());
    let mut budget: u32 = 256;
    for c in place.chars() {
        if budget == 0 {
            // Bound reached. Up to eight trailing characters outside printable
            // ASCII come off before the marker is attached. The reason that trim
            // originally existed - a BYTE count could stop inside a multibyte
            // sequence - no longer applies now the unit is a character, but the
            // trim is observable, so it stays until a slice allowed to change
            // behaviour retires it.
            strip_trailing_nonprint(&mut out);
            out.push_str(&cfg.ellipsis);
            break;
        }
        budget -= 1;
        if c == '"' || c == '\\' || text::is_cntrl(c) {
            continue;
        }
        out.push(c);
    }
    // Unreachable by construction - every location `location` can produce carries
    // a `/`, a `~` or an `@` - and kept as the belt to the braces.
    if out.is_empty() {
        out.push('?');
    }
    out
}

fn strip_trailing_nonprint(out: &mut String) {
    for _ in 0..8 {
        match out.chars().next_back() {
            Some(c) if text::is_nonprint(c) => {
                let keep = out.len() - c.len_utf8();
                out.truncate(keep);
            }
            _ => break,
        }
    }
}

/// The glyph and the location, composed. An empty glyph override paints the
/// location alone, and session end paints nothing at all: its empty title is what
/// unpaints the tab, so a glyph override cannot reach it.
fn title(paint: Paint, place: &str, cfg: &Config) -> String {
    let Some(which) = paint.glyph() else {
        return String::new();
    };
    let glyph = cfg.glyph(which);
    if glyph.is_empty() {
        return place.to_owned();
    }
    match cfg.glyph_pos {
        GlyphPos::Suffix => format!("{} {}", place, glyph),
        GlyphPos::Both => format!("{} {} {}", glyph, place, glyph),
        GlyphPos::Prefix => format!("{} {}", glyph, place),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::Glyph;

    /// A config with the compiled-in defaults and no environment at all, so these
    /// tests do not depend on the process they run in.
    fn cfg() -> Config {
        Config::for_test()
    }

    fn cap(place: Place, max: Cap) -> String {
        let mut c = cfg();
        c.max_location = max;
        apply_length_cap(place, &c)
    }

    #[test]
    fn a_short_location_is_left_alone() {
        let s = cap(Place::Path("~/code/x".into()), Cap::Max(32));
        assert_eq!(s, "~/code/x");
        let s = cap(Place::Repo("repo@master".into()), Cap::Max(32));
        assert_eq!(s, "repo@master");
    }

    #[test]
    fn the_cap_off_keeps_a_location_of_any_length() {
        let long = "~/".to_string() + &"a".repeat(400);
        assert_eq!(cap(Place::Path(long.clone()), Cap::Off), long);
    }

    #[test]
    fn a_repo_is_cut_at_the_back() {
        let s = cap(Place::Repo("streaming-browser@master".into()), Cap::Max(12));
        assert_eq!(s, "streaming-b\u{2026}");
        assert_eq!(s.chars().count(), 12);
    }

    #[test]
    fn a_path_loses_whole_leading_components() {
        let s = cap(Place::Path("~/a/bb/ccc/dddd".into()), Cap::Max(14));
        assert_eq!(s, "\u{2026}/bb/ccc/dddd");
        // Two more components have to go to fit 9.
        let s = cap(Place::Path("~/a/bb/ccc/dddd".into()), Cap::Max(9));
        assert_eq!(s, "\u{2026}/dddd");
    }

    #[test]
    fn one_overlong_component_is_cut_inside_and_loses_the_slash() {
        let s = cap(Place::Path("~/aaaaaaaaaaaaaaaaaaaa".into()), Cap::Max(8));
        assert_eq!(s, "\u{2026}aaaaaaa");
        assert_eq!(s.chars().count(), 8);
    }

    #[test]
    fn a_non_ascii_location_is_cut_like_any_other() {
        // Characters, not bytes: eight 3-byte characters are eight characters.
        let s = cap(Place::Repo("\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}@m".into()), Cap::Max(8));
        assert_eq!(s, "\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}@m");
        let s = cap(Place::Repo("\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}\u{516d}\u{4e03}@main".into()), Cap::Max(8));
        assert_eq!(s.chars().count(), 8);
        assert!(s.ends_with('\u{2026}'));
    }

    #[test]
    fn peeling_stops_when_there_is_nothing_left_to_peel() {
        assert_eq!(peel_leading_component("~/a/b"), Some("a/b"));
        assert_eq!(peel_leading_component("a/b"), Some("b"));
        assert_eq!(peel_leading_component("b"), None);
        assert_eq!(peel_leading_component("b/"), None);
        assert_eq!(peel_leading_component("/"), None);
        assert_eq!(peel_leading_component(""), None);
    }

    #[test]
    fn a_clean_location_skips_the_sanitizer_entirely() {
        let long = "~/".to_string() + &"a".repeat(400);
        assert_eq!(sanitize(long.clone(), &cfg()), long);
    }

    #[test]
    fn the_sanitizer_deletes_what_json_cannot_hold() {
        let s = sanitize("~/a\"b\\c\u{1}d".into(), &cfg());
        assert_eq!(s, "~/abcd");
        // U+2028 is a line separator, and would break one line per hook.
        assert_eq!(sanitize("~/a\u{2028}b".into(), &cfg()), "~/ab");
    }

    #[test]
    fn the_sanitizer_budget_counts_dropped_characters_too() {
        // 257 input characters, one of them a quote: 256 are consumed, so 255
        // survive and the marker says one was left behind.
        let input = format!("\"{}", "a".repeat(256));
        let s = sanitize(input, &cfg());
        assert_eq!(s.chars().count(), 256, "255 kept plus the marker");
        assert!(s.ends_with('\u{2026}'));
        // Exactly 256 input characters is not over the bound, so no marker.
        let input = format!("\"{}", "a".repeat(255));
        let s = sanitize(input, &cfg());
        assert_eq!(s, "a".repeat(255));
    }

    #[test]
    fn the_cut_drops_trailing_non_ascii_before_the_marker() {
        let input = format!("\"{}{}", "a".repeat(250), "\u{e9}".repeat(10));
        let s = sanitize(input, &cfg());
        assert_eq!(s, format!("{}\u{2026}", "a".repeat(250)));
    }

    #[test]
    fn a_location_of_nothing_but_hostile_characters_still_paints() {
        assert_eq!(sanitize("\"\\\u{1}".into(), &cfg()), "?");
    }

    #[test]
    fn the_glyph_goes_where_the_terminal_cannot_cut_it() {
        let mut c = cfg();
        let p = Paint::Line(Glyph::Working);
        c.glyph_pos = GlyphPos::Prefix;
        assert_eq!(title(p, "x", &c), "\u{1f535} x");
        c.glyph_pos = GlyphPos::Suffix;
        assert_eq!(title(p, "x", &c), "x \u{1f535}");
        c.glyph_pos = GlyphPos::Both;
        assert_eq!(title(p, "x", &c), "\u{1f535} x \u{1f535}");
    }

    #[test]
    fn each_state_has_its_own_glyph_and_session_end_has_none() {
        let c = cfg();
        assert_eq!(title(Paint::Line(Glyph::Working), "x", &c), "\u{1f535} x");
        assert_eq!(title(Paint::Line(Glyph::Waiting), "x", &c), "\u{1f7e0} x");
        assert_eq!(title(Paint::Line(Glyph::Idle), "x", &c), "\u{26aa} x");
        assert_eq!(title(Paint::SessionStart, "x", &c), "\u{26aa} x");
        assert_eq!(title(Paint::SessionEnd, "x", &c), "");
    }

    #[test]
    fn an_empty_glyph_override_paints_the_location_alone() {
        let mut c = cfg();
        c.set_glyph(Glyph::Idle, String::new());
        assert_eq!(title(Paint::Line(Glyph::Idle), "~/x", &c), "~/x");
        // And session end ignores the override either way.
        assert_eq!(title(Paint::SessionEnd, "~/x", &c), "");
    }
}
