//! Where this session is, as a tab wants to read it.
//!
//!     streaming-browser@master       inside a git repo
//!     ~/code/bug_fedora              not a repo: the home-relative path
//!
//! This module is the crossing: it works on `Path`, because that is what the
//! filesystem deals in, and returns a [`Place`] and a hostname that are `String`,
//! because from there on the value is only ever measured, cut and shown.

use crate::config::Config;
use crate::git;
use crate::sys;
use crate::text;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The two forms of the current directory. They differ, and the difference is
/// load-bearing:
///
///   * `phys` has every symlink resolved and is what the repository walk uses. A
///     cwd reached through a symlink has no `.git` on its LOGICAL ancestors, so
///     walking the logical path loses the repo and the branch entirely.
///   * `logical` is `$PWD` as inherited, and is what the `~` abbreviation uses,
///     so that a distro whose /home is a symlink to /var/home keeps its `~`.
pub struct Cwd {
    pub logical: PathBuf,
    pub phys: PathBuf,
}

/// The composed location, and - as the variant rather than as a flag - which END
/// the length policy is allowed to cut at.
pub enum Place {
    /// `repo@branch`, cut at the BACK: the repo name is what identifies the tab.
    Repo(String),
    /// A home-relative or absolute path, cut at the FRONT on a component
    /// boundary: the last components are the ones that say where you are.
    Path(String),
}

impl Place {
    pub fn text(&self) -> &str {
        match self {
            Place::Repo(s) | Place::Path(s) => s,
        }
    }

    pub fn into_text(self) -> String {
        match self {
            Place::Repo(s) | Place::Path(s) => s,
        }
    }
}

pub fn cwd(cfg: &Config) -> Cwd {
    let real = std::env::current_dir().ok();
    let logical = logical_pwd(cfg.pwd.as_deref(), real.as_deref());
    // `cd -P .` rewrites $PWD to getcwd(); a failure - an unreadable or deleted
    // cwd - leaves the logical path in place.
    let phys = match &real {
        Some(p) if p.is_absolute() => p.clone(),
        _ => logical.clone(),
    };
    Cwd { logical, phys }
}

/// `$PWD` the way a shell would have had it, and always absolute.
///
/// `$PWD` inside a shell is NOT simply the inherited variable: bash re-validates it
/// at startup (`set_pwd()` in variables.c) and replaces it with `getcwd()` unless it
/// is absolute AND names the same directory as `.` by device and inode. A `$PWD`
/// that passes is never canonicalized. This binary is invoked directly, with no
/// shell to do that, so it does it itself - otherwise a stale `$PWD` decides the
/// tab. The cases are pinned by the unit tests below; the golden corpus does not
/// reach this, because all six of its `$PWD` cases sit inside a repo where the walk
/// uses the physical path anyway.
fn logical_pwd(inherited: Option<&OsStr>, real: Option<&Path>) -> PathBuf {
    let inherited = inherited.map(Path::new);
    if let Some(p) = inherited {
        if p.is_absolute() && same_dir_as_cwd(p) {
            return p.to_path_buf();
        }
    }
    // `getcwd()` failing leaves whatever `$PWD` already held; a relative or empty
    // answer would make every path operation below meaningless, so `/` is the one
    // explicit default.
    match real.or(inherited) {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        _ => PathBuf::from("/"),
    }
}

/// bash's `same_file(path, ".")`: same device and inode, symlinks followed. False
/// when either stat fails, which is what rejects a stale or nonexistent `$PWD`.
///
/// Where the platform offers no file identity (Windows), the two canonical paths
/// stand in for it: canonicalization follows symlinks just as the stat does.
fn same_dir_as_cwd(path: &Path) -> bool {
    let (a, b) = match (std::fs::metadata(path), std::fs::metadata(".")) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return false,
    };
    match (sys::file_id(&a), sys::file_id(&b)) {
        (Some(a), Some(b)) => a == b,
        _ => match (std::fs::canonicalize(path), std::fs::canonicalize(".")) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
    }
}

pub fn place(c: &Cwd, cfg: &Config) -> Place {
    match git::find_repo(&c.logical, &c.phys, cfg.git_dir.as_deref()) {
        Some(r) => {
            // A GIT_DIR carrying dot components leaves one as the label -
            // `GIT_DIR=.` inside a bare repo is a real idiom - and a tab labelled
            // `.` says nothing, so the working directory names it instead.
            //
            // The fallback is taken VERBATIM, dots included: it is deliberately
            // not filtered a second time, so a `$PWD` of `/x/.` with `GIT_DIR=.`
            // really does label the tab `.`. Measured against the reference
            // implementation and pinned below.
            let name = label(&r.top)
                .unwrap_or_else(|| strip_git_suffix(last_component(&c.logical)));
            // The display boundary for the repo name: it came from a path, so it
            // may not be valid UTF-8. The branch crossed already, in `git`.
            let mut out = text::repair(&sys::display_bytes(name));
            // A repo checked out at / has no label to show.
            if out.is_empty() {
                out.push('/');
            }
            out.push('@');
            out.push_str(&r.branch);
            Place::Repo(out)
        }
        // Not a repo: the home-relative path.
        None => Place::Path(text::repair(&abbreviate(&c.logical, cfg.home.as_deref()))),
    }
}

/// A path separator, as a byte of a path's encoded form: `/`, and on Windows `\`
/// too. Both are ASCII, so no byte of a multi-byte character can match.
fn is_sep(b: u8) -> bool {
    std::path::is_separator(b as char)
}

/// The path's last component, as a tab label wants it: exactly ONE trailing slash
/// is tolerated and nothing else is normalised.
///
/// Close to `Path::file_name` but deliberately not it: `file_name` normalises
/// EVERY trailing slash away, so a `GIT_DIR` a user spelled `/a///.git` by hand
/// would still yield `a` where this yields nothing.
///
/// Bytes rather than `&OsStr`: the only consumer is `text::repair`, and a slice
/// of an `OsStr` has no safe, portable way back into one.
fn last_component(path: &Path) -> &[u8] {
    let b = path.as_os_str().as_encoded_bytes();
    let b = match b.split_last() {
        Some((&c, rest)) if is_sep(c) => rest,
        _ => b,
    };
    match b.iter().rposition(|&c| is_sep(c)) {
        Some(i) => &b[i + 1..],
        None => b,
    }
}

/// [`last_component`], unless it is nothing a tab can be named after. `/`, `.`,
/// `..` and nothing-at-all are all `None`, which is `place`'s signal to fall back
/// to the working directory.
fn label(path: &Path) -> Option<&[u8]> {
    match last_component(path) {
        b"" | b"." | b".." => None,
        name => Some(name),
    }
}

/// `repo.git` -> `repo`, so the working directory of a bare checkout names the
/// tab after the repository rather than after its `.git`.
fn strip_git_suffix(name: &[u8]) -> &[u8] {
    name.strip_suffix(b".git").unwrap_or(name)
}

/// `~` for HOME itself, `~/x/y` beneath it, and anything outside HOME left
/// absolute.
///
/// Whether the path lies under HOME is [`sys::strip_home_prefix`]'s answer, the same
/// comparison install uses, minus the disk: on Unix exact bytes, with the `/` after
/// HOME required, so `/home/alex` cannot claim `/home/alex2` and a doubled slash a
/// `$PWD` kept verbatim still counts; on Windows component-wise and case-blind, so
/// `c:\users\me`, `C:/Users/me` and `\\?\C:\Users\me` all paint `~` - but not the
/// 8.3 `C:\Users\ALEXAN~1`, which only the disk could expand. What follows HOME keeps
/// the path's own spelling.
///
/// Every separator comes out as `/`. On Unix that is every byte `is_sep` matches, so
/// nothing changes; on Windows `C:\x` and the `C:/x` Git Bash hands a program are the
/// same directory and now paint the same text - and `\` is a byte `render` DELETES as
/// JSON-hostile, so a native-shell cwd used to paint `C:Usersalexcode`. An unpaired
/// surrogate in a Windows name comes out as one U+FFFD ([`sys::display_bytes`]).
fn abbreviate(logical: &Path, home: Option<&Path>) -> Vec<u8> {
    let slashed = |p: &OsStr| -> Vec<u8> {
        sys::display_bytes(p.as_encoded_bytes()).iter().map(|&c| if is_sep(c) { b'/' } else { c }).collect()
    };
    match home.and_then(|h| sys::strip_home_prefix(logical, h)) {
        Some(rest) => {
            let mut out = b"~".to_vec();
            out.extend(slashed(&rest));
            out
        }
        None => slashed(logical.as_os_str()),
    }
}

/// The host name, in order: `CCTAB_HOST` overrides outright, `/proc` is the
/// fork-free path (where there is one - [`sys::kernel_hostname_file`]), `$HOSTNAME` is next (bash sets it, dash and ash do not), and
/// `hostname` is the last resort and the ONLY fork in this binary.
///
/// `None` means no name resolved, and what to paint instead is the caller's
/// business. This never returns `Some("")`: a source that answers with nothing is
/// not a name, so each step rejects an empty answer and tries the next.
pub fn hostname(cfg: &Config) -> Option<String> {
    hostname_raw(cfg).map(|b| text::repair(&b))
}

fn hostname_raw(cfg: &Config) -> Option<Vec<u8>> {
    if let Some(h) = &cfg.host_override {
        return Some(h.as_encoded_bytes().to_vec());
    }
    // A one-line system file, read the same way git's own metadata is - where
    // the platform has one.
    sys::kernel_hostname_file()
        .and_then(git::first_line)
        .filter(|h| !h.is_empty())
        .or_else(|| {
            cfg.hostname_env
                .as_ref()
                .map(|h| h.as_encoded_bytes().to_vec())
                .filter(|h| !h.is_empty())
        })
        .or_else(hostname_command)
}

fn hostname_command() -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};
    let out = Command::new("hostname")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Command substitution strips every trailing newline, and a name that is
    // nothing but newlines is no name. Windows' `hostname.exe` ends its line with
    // CRLF, and there the CR belongs to the line ending too.
    let mut v = out.stdout;
    while v.last().is_some_and(|&c| sys::is_line_end(c)) {
        v.pop();
    }
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on this platform: `/x` on Unix, and `C:\x` on Windows,
    /// where a bare `/x` is not absolute.
    fn abs(s: &str) -> PathBuf {
        let cwd = std::env::current_dir().expect("a cwd");
        cwd.ancestors().last().expect("a root").join(s)
    }

    #[test]
    fn a_stale_or_relative_pwd_is_replaced_by_getcwd() {
        let real = abs("real/cwd");
        let p = |pwd: &OsStr| logical_pwd(Some(pwd), Some(&real));
        // None of these names the cwd by device and inode.
        assert_eq!(p(OsStr::new("relative/dir")), real);
        assert_eq!(p(OsStr::new(".")), real);
        assert_eq!(p(OsStr::new("")), real);
        assert_eq!(p(abs("no/such/directory/anywhere").as_os_str()), real);
    }

    #[test]
    fn a_pwd_that_names_the_cwd_is_kept_verbatim() {
        // `/a/b/../b` names the cwd and must survive uncanonicalized, so this is
        // run against the real cwd - the check is a device-and-inode test, not a
        // string test.
        let real = std::env::current_dir().expect("a cwd");
        let name = real.file_name().expect("the cwd has a name").to_owned();
        let dotdot = real.join("..").join(name);
        let got = logical_pwd(Some(dotdot.as_os_str()), Some(&real));
        assert_eq!(got, dotdot, "a passing $PWD is not canonicalized");
    }

    #[test]
    fn a_relative_answer_from_everywhere_falls_back_to_root() {
        assert_eq!(logical_pwd(None, None), PathBuf::from("/"));
        assert_eq!(logical_pwd(Some(OsStr::new("rel")), None), PathBuf::from("/"));
        assert_eq!(logical_pwd(None, Some(Path::new("rel"))), PathBuf::from("/"));
    }

    #[test]
    fn an_absolute_pwd_survives_a_getcwd_that_failed() {
        let gone = abs("gone/away");
        let got = logical_pwd(Some(gone.as_os_str()), None);
        assert_eq!(got, gone);
    }

    #[test]
    fn the_label_is_the_last_component_with_one_trailing_slash_tolerated() {
        let l = |s: &str| String::from_utf8(last_component(Path::new(s)).to_vec()).expect("ascii");
        assert_eq!(l("/a/b/repo"), "repo");
        assert_eq!(l("/a/b/repo/"), "repo");
        assert_eq!(l("repo"), "repo");
        assert_eq!(l("/"), "");
        assert_eq!(l(""), "");
        assert_eq!(l("//"), "");
        // A second trailing slash is where `Path::file_name` would disagree.
        assert_eq!(l("/a//"), "");
        // Dot components come back as themselves.
        assert_eq!(l("/a/."), ".");
        assert_eq!(l("/a/.."), "..");
    }

    #[test]
    fn nothing_a_tab_can_be_named_after_is_no_label() {
        for s in ["/", "", "//", "/a//", "/a/.", "/a/..", ".", ".."] {
            assert_eq!(label(Path::new(s)), None, "{:?}", s);
        }
        assert_eq!(label(Path::new("/a/repo")), Some(&b"repo"[..]));
        // `.git` is stripped only on the FALLBACK, never on the label itself.
        assert_eq!(label(Path::new("/a/r.git")), Some(&b"r.git"[..]));
    }

    #[test]
    fn the_fallback_label_is_taken_verbatim_dots_included() {
        // What `place` does when `label(top)` is None: the working directory's
        // last component, with `.git` off and NO second filtering pass. A $PWD of
        // `/x/.` therefore labels the tab `.`, which the reference implementation
        // does too - it is reproduced here rather than fixed.
        fn fallback(s: &str) -> &[u8] {
            strip_git_suffix(last_component(Path::new(s)))
        }
        assert_eq!(fallback("/x/repo"), b"repo");
        assert_eq!(fallback("/x/repo.git"), b"repo");
        assert_eq!(fallback("/x/."), b".");
        assert_eq!(fallback("/x/.."), b"..");
        assert_eq!(fallback("/"), b"");
    }

    #[test]
    fn the_home_abbreviation_replaces_only_a_whole_component_prefix() {
        let a = |cwd: &str, home: Option<&str>| {
            String::from_utf8(abbreviate(Path::new(cwd), home.map(Path::new))).expect("ascii")
        };
        assert_eq!(a("/home/alex", Some("/home/alex")), "~");
        assert_eq!(a("/home/alex/code", Some("/home/alex")), "~/code");
        assert_eq!(a("/home/alex/", Some("/home/alex")), "~/");
        // What follows HOME is shown as spelled, a doubled slash included.
        assert_eq!(a("/home/alex//code", Some("/home/alex")), "~//code");
        // The sibling a naive string prefix would swallow.
        assert_eq!(a("/home/alex2/code", Some("/home/alex")), "/home/alex2/code");
        assert_eq!(a("/etc", Some("/home/alex")), "/etc");
        assert_eq!(a("/home/alex", None), "/home/alex");
    }

    #[cfg(windows)]
    #[test]
    fn a_backslash_after_home_is_a_component_boundary_on_windows() {
        let a = |cwd: &str, home: &str| {
            String::from_utf8(abbreviate(Path::new(cwd), Some(Path::new(home)))).expect("ascii")
        };
        assert_eq!(a(r"C:\Users\alex\code", r"C:\Users\alex"), "~/code");
        assert_eq!(a(r"C:\Users\alex2\code", r"C:\Users\alex"), "C:/Users/alex2/code");
        // Git Bash hands a program `C:/...`; either spelling of either side matches.
        assert_eq!(a("C:/Users/alex/code", r"C:\Users\alex"), "~/code");
        assert_eq!(a(r"C:\Users\alex", "C:/Users/alex"), "~");
        assert_eq!(last_component(Path::new(r"C:\code\repo\")), b"repo");
    }

    /// Windows names are case-blind, and `\\?\` is only a spelling: any of them paints
    /// `~`, the rest as the path spells it. An 8.3 short name is NOT seen through - that
    /// takes the disk, and this runs on every hook.
    #[cfg(windows)]
    #[test]
    fn any_spelling_of_home_paints_a_tilde_on_windows_but_a_short_name() {
        let a = |cwd: &str, home: &str| {
            String::from_utf8(abbreviate(Path::new(cwd), Some(Path::new(home)))).expect("ascii")
        };
        let home = r"C:\Users\Alex";
        assert_eq!(a(r"c:\users\alex\code\x", home), "~/code/x");
        assert_eq!(a(r"\\?\C:\Users\Alex\code\x", home), "~/code/x");
        assert_eq!(a(r"C:\USERS\ALEX\Code", home), "~/Code", "the rest keeps its spelling");
        assert_eq!(a(r"C:\Users\Alex\\x\", home), "~//x/");
        assert_eq!(a(r"C:\Users\Alex2\x", home), "C:/Users/Alex2/x");
        assert_eq!(
            a(r"C:\Users\ALEXAN~1\code", r"C:\Users\Alexandre Flament"),
            "C:/Users/ALEXAN~1/code"
        );
    }

    /// A HOME whose name holds an unpaired surrogate is still HOME - exactly, not by a
    /// lossy decode - so a sibling differing only in that unit, or holding a real
    /// U+FFFD there, keeps its full path. The surrogate itself shows as U+FFFD.
    #[cfg(windows)]
    #[test]
    fn a_home_named_with_an_unpaired_surrogate_matches_itself_and_no_sibling() {
        use std::os::windows::ffi::OsStringExt;
        let d = std::env::temp_dir().join(format!("cctab-loc-surrogate-{}", std::process::id()));
        let named = |units: &[u16]| d.join(std::ffi::OsString::from_wide(units));
        let home = named(&[0x68, 0xD800]);
        let (d801, fffd) = (named(&[0x68, 0xD801]), named(&[0x68, 0xFFFD]));
        for x in [&home, &d801, &fffd] {
            std::fs::create_dir_all(x.join("code")).expect("mkdir");
        }
        let t = |cwd: &Path| text::repair(&abbreviate(cwd, Some(&home)));
        assert_eq!(t(&home), "~");
        assert_eq!(t(&home.join("code")), "~/code");
        assert_eq!(t(&named(&[0x48, 0xD800]).join("code")), "~/code", "case still folds");
        for sib in [&d801, &fffd] {
            let shown = t(&sib.join("code"));
            assert!(!shown.starts_with('~') && shown.ends_with("/h\u{fffd}/code"), "{shown}");
        }
        // One U+FFFD per surrogate, not one per byte of its WTF-8 form.
        let odd = home.join(std::ffi::OsString::from_wide(&[0x62, 0xDC00]));
        assert_eq!(t(&odd), "~/b\u{fffd}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn home_is_matched_case_sensitively_on_unix() {
        let home = Path::new("/home/alex");
        assert_eq!(abbreviate(Path::new("/home/Alex/code"), Some(home)), b"/home/Alex/code");
    }

    #[cfg(unix)]
    #[test]
    fn an_invalid_byte_in_a_path_survives_as_a_replacement_character() {
        use std::os::unix::ffi::OsStrExt;
        let cwd = Path::new(OsStr::from_bytes(b"/home/alex/b\xffd"));
        let home = Path::new("/home/alex");
        assert_eq!(text::repair(&abbreviate(cwd, Some(home))), "~/b\u{fffd}d");
    }
}
