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
use crate::text;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
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
fn same_dir_as_cwd(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (a, b) = match (std::fs::metadata(path), std::fs::metadata(".")) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return false,
    };
    a.dev() == b.dev() && a.ino() == b.ino()
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
            let mut out = text::repair(name.as_bytes());
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

/// The path's last component, as a tab label wants it: exactly ONE trailing slash
/// is tolerated and nothing else is normalised.
///
/// Close to `Path::file_name` but deliberately not it: `file_name` normalises
/// EVERY trailing slash away, so a `GIT_DIR` a user spelled `/a///.git` by hand
/// would still yield `a` where this yields nothing.
fn last_component(path: &Path) -> &OsStr {
    let b = path.as_os_str().as_bytes();
    let b = b.strip_suffix(b"/").unwrap_or(b);
    let b = match b.iter().rposition(|&c| c == b'/') {
        Some(i) => &b[i + 1..],
        None => b,
    };
    OsStr::from_bytes(b)
}

/// [`last_component`], unless it is nothing a tab can be named after. `/`, `.`,
/// `..` and nothing-at-all are all `None`, which is `place`'s signal to fall back
/// to the working directory.
fn label(path: &Path) -> Option<&OsStr> {
    let name = last_component(path);
    match name.as_bytes() {
        b"" | b"." | b".." => None,
        _ => Some(name),
    }
}

/// `repo.git` -> `repo`, so the working directory of a bare checkout names the
/// tab after the repository rather than after its `.git`.
fn strip_git_suffix(name: &OsStr) -> &OsStr {
    let b = name.as_bytes();
    OsStr::from_bytes(b.strip_suffix(b".git").unwrap_or(b))
}

/// `~` for HOME itself, `~/x/y` beneath it, and anything outside HOME left
/// absolute.
///
/// The prefix test insists on the `/` that follows HOME, so `/home/alex` cannot
/// claim `/home/alex2`, and it is taken on path BYTES rather than through
/// `Path::starts_with`, which would silently normalise a doubled slash that a
/// `$PWD` kept verbatim can still hold.
fn abbreviate(logical: &Path, home: Option<&Path>) -> Vec<u8> {
    let l = logical.as_os_str().as_bytes();
    let home = match home {
        Some(h) => h.as_os_str().as_bytes(),
        None => return l.to_vec(),
    };
    if l == home {
        return b"~".to_vec();
    }
    if let Some(rest) = l.strip_prefix(home) {
        if rest.starts_with(b"/") {
            let mut out = b"~".to_vec();
            out.extend_from_slice(rest);
            return out;
        }
    }
    l.to_vec()
}

/// The host name, in order: `CCTAB_HOST` overrides outright, `/proc` is the
/// fork-free path, `$HOSTNAME` is next (bash sets it, dash and ash do not), and
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
        return Some(h.as_bytes().to_vec());
    }
    // A one-line system file, read the same way git's own metadata is.
    git::first_line(Path::new("/proc/sys/kernel/hostname"))
        .filter(|h| !h.is_empty())
        .or_else(|| {
            cfg.hostname_env
                .as_ref()
                .map(|h| h.as_bytes().to_vec())
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
    // nothing but newlines is no name.
    let mut v = out.stdout;
    while v.last() == Some(&b'\n') {
        v.pop();
    }
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_or_relative_pwd_is_replaced_by_getcwd() {
        let real = Path::new("/real/cwd");
        let p = |pwd: &str| logical_pwd(Some(OsStr::new(pwd)), Some(real));
        // None of these names the cwd by device and inode.
        assert_eq!(p("relative/dir"), real);
        assert_eq!(p("."), real);
        assert_eq!(p(""), real);
        assert_eq!(p("/no/such/directory/anywhere"), real);
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
        let got = logical_pwd(Some(OsStr::new("/gone/away")), None);
        assert_eq!(got, PathBuf::from("/gone/away"));
    }

    #[test]
    fn the_label_is_the_last_component_with_one_trailing_slash_tolerated() {
        let l = |s: &str| last_component(Path::new(s)).to_str().expect("ascii").to_owned();
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
        assert_eq!(label(Path::new("/a/repo")), Some(OsStr::new("repo")));
        // `.git` is stripped only on the FALLBACK, never on the label itself.
        assert_eq!(label(Path::new("/a/r.git")), Some(OsStr::new("r.git")));
    }

    #[test]
    fn the_fallback_label_is_taken_verbatim_dots_included() {
        // What `place` does when `label(top)` is None: the working directory's
        // last component, with `.git` off and NO second filtering pass. A $PWD of
        // `/x/.` therefore labels the tab `.`, which the reference implementation
        // does too - it is reproduced here rather than fixed.
        fn fallback(s: &str) -> &OsStr {
            strip_git_suffix(last_component(Path::new(s)))
        }
        assert_eq!(fallback("/x/repo"), OsStr::new("repo"));
        assert_eq!(fallback("/x/repo.git"), OsStr::new("repo"));
        assert_eq!(fallback("/x/."), OsStr::new("."));
        assert_eq!(fallback("/x/.."), OsStr::new(".."));
        assert_eq!(fallback("/"), OsStr::new(""));
    }

    #[test]
    fn the_home_abbreviation_replaces_only_a_whole_component_prefix() {
        let a = |cwd: &str, home: Option<&str>| {
            String::from_utf8(abbreviate(Path::new(cwd), home.map(Path::new))).expect("ascii")
        };
        assert_eq!(a("/home/alex", Some("/home/alex")), "~");
        assert_eq!(a("/home/alex/code", Some("/home/alex")), "~/code");
        assert_eq!(a("/home/alex/", Some("/home/alex")), "~/");
        // The sibling a naive string prefix would swallow.
        assert_eq!(a("/home/alex2/code", Some("/home/alex")), "/home/alex2/code");
        assert_eq!(a("/etc", Some("/home/alex")), "/etc");
        assert_eq!(a("/home/alex", None), "/home/alex");
    }

    #[test]
    fn an_invalid_byte_in_a_path_survives_as_a_replacement_character() {
        let cwd = Path::new(OsStr::from_bytes(b"/home/alex/b\xffd"));
        let home = Path::new("/home/alex");
        assert_eq!(text::repair(&abbreviate(cwd, Some(home))), "~/b\u{fffd}d");
    }
}
