//! Section 1 of the script: where this session is, as a tab wants to read it.
//!
//!     streaming-browser@master       inside a git repo
//!     ~/code/bug_fedora              not a repo: the home-relative path
//!
//! Fork-free, and deliberately not `git rev-parse`: that call is correct and
//! costs 15-40ms, against a whole-hook budget of a fraction of a millisecond on
//! an edge that fires once per tool call. So the two parts of git's repository
//! discovery that actually show up in a tab are reimplemented here, and nothing
//! else about a repository is consulted.

use crate::sh::{
    self, after_first_slash, basename, char_count, dirname, env_raw, env_set, env_str,
    pat_first_chars, pat_strip_trailing_cntrl, pat_trim_step, read_first_line, rstrip_slash,
    utf8_repair,
};

/// The two forms of the current directory. They differ, and the difference is
/// load-bearing:
///
///   * `phys` has every symlink resolved and is what the repository walk uses.
///     A cwd reached through a symlink has no `.git` on its LOGICAL ancestors,
///     so walking the logical path loses the repo and the branch entirely.
///   * `logical` is `$PWD` as inherited, and is what the `~` abbreviation uses,
///     so that a distro whose /home is a symlink to /var/home keeps its `~`.
pub struct Cwd {
    pub logical: Vec<u8>,
    pub phys: Vec<u8>,
    pub home: Vec<u8>,
}

pub fn cwd() -> Cwd {
    let real = std::env::current_dir()
        .ok()
        .map(|p| sh::from_os(p.into_os_string()));
    let inherited = env_raw("PWD");

    // The shell script read `$PWD`, but `$PWD` inside a shell is NOT simply the
    // inherited variable: bash re-validates it at startup (set_pwd() in
    // variables.c) and replaces it with getcwd() unless it is absolute AND
    // names the same directory as `.` by device and inode. Measured: PWD=/etc,
    // PWD=relative, PWD=. and PWD=/no/such/dir are all replaced, while
    // PWD=/a/b/../b and a PWD that reaches the cwd through a symlink are kept
    // VERBATIM - bash does not canonicalize a PWD that passes the check.
    //
    // The port is invoked directly, with no shell to do that, so it has to do
    // it itself. Skipping it is not a cosmetic difference: a stale PWD would
    // then decide the tab, where the shell version corrected it. The golden
    // corpus does NOT catch this - all six of its PWD cases sit inside a repo,
    // where the walk uses the physical path and the answer is the same either
    // way - a differential fuzz against the shell did.
    let mut pwd = match &inherited {
        Some(p) if p.starts_with(b"/") && same_dir_as_cwd(p) => p.clone(),
        // getcwd() failing leaves bash with whatever PWD already held.
        _ => real
            .clone()
            .or_else(|| inherited.clone())
            .unwrap_or_default(),
    };
    // [ -n "${PWD-}" ] || PWD=$(pwd 2>/dev/null)
    if pwd.is_empty() {
        pwd = real.clone().unwrap_or_default();
    }
    // A relative or empty PWD would make every path operation below meaningless.
    if !pwd.starts_with(b"/") {
        pwd = b"/".to_vec();
    }
    let logical = pwd;
    // `CDPATH= cd -P . 2>/dev/null` rewrites $PWD to getcwd(); a failure (an
    // unreadable or deleted cwd) leaves the logical path in place.
    let phys = match &real {
        Some(b) if b.starts_with(b"/") => b.clone(),
        _ => logical.clone(),
    };
    // ${HOME%/}: a HOME carrying a trailing slash would otherwise miss the
    // comparisons below and render the home directory as its full path.
    let home = rstrip_slash(&env_str("HOME")).to_vec();
    Cwd { logical, phys, home }
}

/// Section 1a. Reads one candidate `.git` and returns its branch, or `None`
/// when the candidate is not a repository.
///
/// "Is a repository" means "HEAD parses". git also insists on objects/ and
/// refs/; one readable file is cheaper and rules out what actually turns up in
/// the wild, which is an empty `.git` directory left behind by another tool -
/// without which an empty /tmp/.git would make every path under /tmp render as
/// `tmp`.
fn branch_of(candidate: &[u8]) -> Option<Vec<u8>> {
    let mut gd: Vec<u8> = candidate.to_vec();

    // .git may be a FILE holding "gitdir: <path>" - a linked worktree or a
    // submodule - and that path may be relative to the directory holding it.
    // The joined path is left unnormalized; the kernel resolves it.
    if is_file(&gd) {
        let raw = read_first_line(&gd);
        // A gitdir: line holds a path, so PATH_MAX bounds anything legitimate.
        // Longer means some other file that happens to be called .git.
        if char_count(&raw) > 4096 {
            return None;
        }
        // One trailing control character: a CR from a Windows checkout.
        let line: &[u8] = pat_strip_trailing_cntrl(&raw);
        // 'gitdir: '?* - the ?* means the path must be non-empty.
        let rest = line.strip_prefix(b"gitdir: ".as_slice())?;
        if rest.is_empty() {
            return None;
        }
        gd = if rest.starts_with(b"/") {
            rest.to_vec()
        } else {
            // ${1%/*}/$_gd, relative to the directory holding the .git file.
            let mut j = dirname(candidate).to_vec();
            j.push(b'/');
            j.extend_from_slice(rest);
            j
        };
    }

    let mut head_path = rstrip_slash(&gd).to_vec();
    head_path.extend_from_slice(b"/HEAD");
    if !is_file(&head_path) {
        return None;
    }
    let mut head = read_first_line(&head_path);
    // A real HEAD's first line is a 41-byte object id or a ref name, and git's
    // own limit on a ref is well inside 255 bytes. This guard has to come
    // BEFORE the cuts below, because in the shell those cuts were quadratic and
    // a 100KB first line stalled the hook for 6.5 seconds.
    if char_count(&head) > 255 {
        return None;
    }
    // Trim the trailing line noise: a CR from a Windows checkout, and the
    // spaces or tabs git itself ignores after a ref name.
    while let Some(start) = pat_trim_step(&head) {
        head.truncate(start);
    }

    if let Some(r) = head.strip_prefix(b"ref: ".as_slice()) {
        if r.is_empty() {
            return None;
        }
        // A symref normally points into refs/heads/. When it does not, keep the
        // namespace but drop the uninformative `refs/`, so a HEAD left on
        // refs/remotes/origin/main reads `origin/main`.
        let b = if let Some(t) = r.strip_prefix(b"refs/heads/".as_slice()) {
            t
        } else if let Some(t) = r.strip_prefix(b"refs/remotes/".as_slice()) {
            t
        } else if let Some(t) = r.strip_prefix(b"refs/".as_slice()) {
            t
        } else {
            r
        };
        if b.is_empty() {
            return None;
        }
        return Some(b.to_vec());
    }
    // At least 7 characters and all hex: an object id, so HEAD is detached.
    if char_count(&head) >= 7 && !head.iter().any(|b| !b.is_ascii_hexdigit()) {
        return Some(pat_first_chars(&head, 7).to_vec());
    }
    None
}

/// bash's `same_file(path, ".")`: same device and inode, symlinks followed.
/// False when either stat fails, which is what rejects a stale or nonexistent
/// PWD.
fn same_dir_as_cwd(path: &[u8]) -> bool {
    use std::os::unix::fs::MetadataExt;
    let a = match std::fs::metadata(sh::as_path(path)) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let b = match std::fs::metadata(".") {
        Ok(m) => m,
        Err(_) => return false,
    };
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn is_file(path: &[u8]) -> bool {
    std::fs::metadata(sh::as_path(path))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn exists(path: &[u8]) -> bool {
    std::fs::metadata(sh::as_path(path)).is_ok()
}

struct Repo {
    top: Vec<u8>,
    branch: Vec<u8>,
}

/// Section 1b. An explicit GIT_DIR wins over the walk, as it does for git, and
/// a GIT_DIR that is not a repository is not second-guessed by walking anyway.
fn find_repo(c: &Cwd) -> Option<Repo> {
    if env_set("GIT_DIR") {
        let gd = env_str("GIT_DIR");
        let abs = if gd.starts_with(b"/") {
            gd
        } else {
            let mut j = c.logical.clone();
            j.push(b'/');
            j.extend_from_slice(&gd);
            j
        };
        let branch = branch_of(&abs)?;
        // Name the repo after GIT_DIR's own location rather than after $PWD:
        // /w/repo/.git -> repo, and a bare /srv/repo.git -> repo.
        let mut top = rstrip_slash(&abs).to_vec();
        if top.ends_with(b"/.git") {
            top.truncate(top.len() - 5);
        } else if top.ends_with(b".git") {
            top.truncate(top.len() - 4);
        }
        return Some(Repo { top, branch });
    }
    // Walk up looking for a .git that checks out. Bounded at 64 components, far
    // past any real tree, so no pathological path can spin here. Each step is
    // one stat and no fork.
    let mut dir = c.phys.clone();
    for _ in 0..64 {
        let mut probe = rstrip_slash(&dir).to_vec();
        probe.extend_from_slice(b"/.git");
        if exists(&probe) {
            if let Some(branch) = branch_of(&probe) {
                return Some(Repo { top: dir, branch });
            }
        }
        if &dir[..] == b"/" {
            break;
        }
        dir = dirname(&dir).to_vec();
        if dir.is_empty() {
            dir = b"/".to_vec();
        }
    }
    None
}

/// Section 1c. The composed location, and whether it came from a repository -
/// which decides at which END the length policy in section 1d cuts.
pub fn place(c: &Cwd) -> (Vec<u8>, bool) {
    if let Some(r) = find_repo(c) {
        let mut place = basename(rstrip_slash(&r.top)).to_vec();
        // A GIT_DIR carrying dot components leaves one as the basename -
        // `GIT_DIR=.` inside a bare repo is a real idiom - and a tab labelled
        // `.` says nothing. Name it after the working directory instead.
        if place.is_empty() || &place[..] == b"." || &place[..] == b".." {
            let mut p = basename(rstrip_slash(&c.logical)).to_vec();
            if p.ends_with(b".git") {
                p.truncate(p.len() - 4);
            }
            place = p;
        }
        // A repo checked out at / has no basename to show.
        if place.is_empty() {
            place = b"/".to_vec();
        }
        place.push(b'@');
        place.extend_from_slice(&r.branch);
        // The display boundary: from here on this is text, not a path to open,
        // so an invalid byte becomes U+FFFD. Everything above used the raw bytes
        // the kernel gave us, which is what the .git walk needs. Fixes README
        // limitation 2 - a name that is legal on Linux and illegal in JSON.
        (utf8_repair(place), true)
    } else {
        // Not a repo: the home-relative path. ~ for HOME itself, ~/x/y beneath
        // it, and anything outside HOME stays absolute.
        let mut place = c.logical.clone();
        if !c.home.is_empty() {
            if c.logical == c.home {
                place = b"~".to_vec();
            } else {
                let mut pref = c.home.clone();
                pref.push(b'/');
                if c.logical.starts_with(&pref[..]) {
                    place = b"~".to_vec();
                    place.extend_from_slice(&c.logical[c.home.len()..]);
                }
            }
        }
        (utf8_repair(place), false)
    }
}

/// Used by the length policy in render.rs; kept next to the path logic it
/// belongs to.
pub fn peel_leading_component(place: &[u8]) -> Option<Vec<u8>> {
    let rest = after_first_slash(place);
    if rest == place || rest.is_empty() {
        None
    } else {
        Some(rest.to_vec())
    }
}

/// Section 1e's name resolution, in the script's order: CCTAB_HOST overrides
/// outright, /proc is the fork-free path, $HOSTNAME is next (bash sets it, dash
/// and ash do not), and `hostname` is the last resort and the only fork in the
/// whole block - reached only where /proc is absent.
pub fn hostname() -> Vec<u8> {
    utf8_repair(hostname_raw())
}

fn hostname_raw() -> Vec<u8> {
    let mut h = env_str("CCTAB_HOST");
    if h.is_empty() {
        h = read_first_line(b"/proc/sys/kernel/hostname");
    }
    if h.is_empty() {
        h = env_raw("HOSTNAME").unwrap_or_default();
    }
    if h.is_empty() {
        h = hostname_command();
    }
    h
}

fn hostname_command() -> Vec<u8> {
    use std::process::{Command, Stdio};
    match Command::new("hostname")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => {
            // Command substitution strips every trailing newline.
            let mut v = o.stdout;
            while v.last() == Some(&b'\n') {
                v.pop();
            }
            v
        }
        _ => Vec::new(),
    }
}
