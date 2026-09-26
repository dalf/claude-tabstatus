//! Repository discovery, reduced to the two things a tab shows: the top
//! directory and the branch.
//!
//! Fork-free, and deliberately not `git rev-parse`: that call is correct and costs
//! 15-40ms, against a whole-hook budget of a fraction of a millisecond on an edge
//! that fires once per tool call. Only the two parts of git's discovery that reach
//! the tab are reimplemented, and nothing else about a repository is consulted.
//!
//! This is the byte side of the program: a `.git` file's `gitdir:` line holds a
//! PATH, and handing the kernel a repaired copy of one would open a different file
//! or none at all. The branch is the single value that becomes text, and it crosses
//! the moment it is extracted, because nothing after that opens anything.

use crate::text;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub struct Repo {
    /// The working tree's top directory, or wherever `GIT_DIR` says the
    /// repository lives. Only its last component is ever shown.
    pub top: PathBuf,
    pub branch: String,
}

/// How far up the tree the walk goes. Far past any real checkout, so no
/// pathological path can spin here; each step is one stat and no fork.
const WALK_LIMIT: usize = 64;

/// An explicit `GIT_DIR` wins over the walk, as it does for git, and a `GIT_DIR`
/// that is not a repository is not second-guessed by walking anyway.
pub fn find_repo(logical: &Path, phys: &Path, git_dir: Option<&OsStr>) -> Option<Repo> {
    if let Some(gd) = git_dir {
        let abs = absolute(logical, gd);
        let branch = branch_of(&abs)?;
        return Some(Repo { top: repo_top(&abs), branch });
    }
    let mut dir = phys.to_path_buf();
    for _ in 0..WALK_LIMIT {
        let probe = dir.join(".git");
        if probe.exists() {
            if let Some(branch) = branch_of(&probe) {
                return Some(Repo { top: dir, branch });
            }
            // A candidate that does not check out must NOT stop the walk: an
            // empty `.git` left behind by another tool, or a stale gitdir
            // pointer, cannot be allowed to shadow the real repository above it.
        }
        if dir == Path::new("/") {
            break;
        }
        dir = parent_of(&dir);
    }
    None
}

/// A relative `GIT_DIR` resolves against the LOGICAL cwd, which is what a shell
/// would have done with it.
fn absolute(logical: &Path, git_dir: &OsStr) -> PathBuf {
    if git_dir.as_bytes().starts_with(b"/") {
        PathBuf::from(git_dir)
    } else {
        logical.join(git_dir)
    }
}

/// Name the repository after `GIT_DIR`'s own location rather than after `$PWD`:
/// `/w/repo/.git` -> `/w/repo`, and a bare `/srv/repo.git` -> `/srv/repo`.
///
/// A suffix strip on the path's bytes, because `Path` has no way to say "drop
/// four bytes from the last component".
fn repo_top(git_dir: &Path) -> PathBuf {
    let b = git_dir.as_os_str().as_bytes();
    let b = b.strip_suffix(b"/").unwrap_or(b);
    let b = b
        .strip_suffix(b"/.git")
        .or_else(|| b.strip_suffix(b".git"))
        .unwrap_or(b);
    path_of(b)
}

/// One step up the tree: everything before the LAST slash, with an empty result
/// mapped back to `/`.
///
/// Not `Path::parent`, which normalises a trailing slash away first and so
/// answers `/a` for `/a/b/` where this answers `/a/b`. The walk's input can be a
/// `$PWD` kept verbatim, which is the one way a trailing slash gets here.
fn parent_of(dir: &Path) -> PathBuf {
    let b = dir.as_os_str().as_bytes();
    let cut = match b.iter().rposition(|&c| c == b'/') {
        Some(i) => &b[..i],
        None => b,
    };
    if cut.is_empty() {
        PathBuf::from("/")
    } else {
        path_of(cut)
    }
}

/// Reads one candidate `.git` and returns its branch, or `None` when the
/// candidate is not a repository.
///
/// "Is a repository" means "HEAD parses". git also insists on `objects/` and
/// `refs/`; one readable file is cheaper and rules out what actually turns up in
/// the wild, which is an empty `.git` directory left behind by another tool -
/// without which an empty `/tmp/.git` would make every path under `/tmp` render
/// as `tmp`.
fn branch_of(candidate: &Path) -> Option<String> {
    let git_dir = resolve_gitdir(candidate)?;
    let head_path = git_dir.join("HEAD");
    if !head_path.is_file() {
        return None;
    }
    // From here on HEAD's content is display text and never a path, so it can
    // cross to `String` at once. The crossing preserves the character count -
    // one U+FFFD per invalid byte - which is what the guard below counts.
    let head = text::repair(&first_line(&head_path)?);
    // A real HEAD's first line is a 41-byte object id or a ref name, and git's
    // own limit on a ref is well inside 255 characters. The guard comes BEFORE
    // the cuts below so that a pathological file is rejected rather than parsed.
    if head.chars().count() > 255 {
        return None;
    }
    // Trailing line noise: a CR from a Windows checkout, and the spaces or tabs
    // git itself ignores after a ref name.
    let head = head.trim_end_matches(|c| text::is_cntrl(c) || text::is_blank(c));

    if let Some(r) = head.strip_prefix("ref: ") {
        if r.is_empty() {
            return None;
        }
        // A symref normally points into refs/heads/. When it does not, keep the
        // namespace but drop the uninformative `refs/`, so a HEAD left on
        // refs/remotes/origin/main reads `origin/main`.
        let b = r
            .strip_prefix("refs/heads/")
            .or_else(|| r.strip_prefix("refs/remotes/"))
            .or_else(|| r.strip_prefix("refs/"))
            .unwrap_or(r);
        if b.is_empty() {
            return None;
        }
        return Some(b.to_owned());
    }
    // At least 7 characters and all hex: an object id, so HEAD is detached. All
    // hex also means all ASCII, which is what makes the byte slice below a
    // character slice.
    if head.chars().count() >= 7 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..7].to_owned());
    }
    None
}

/// The directory holding `HEAD`.
///
/// `.git` may be a FILE holding `gitdir: <path>` - a linked worktree or a
/// submodule - and that path may be relative to the directory holding it. The
/// joined path is left unnormalized; the kernel resolves it.
fn resolve_gitdir(candidate: &Path) -> Option<PathBuf> {
    if !candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    let raw = first_line(candidate)?;
    // A `gitdir:` line holds a path, so PATH_MAX bounds anything legitimate.
    // Longer means some other file that happens to be called .git.
    let text = text::repair(&raw);
    if text.chars().count() > 4096 {
        return None;
    }
    // One trailing control character: a CR from a Windows checkout. It comes off
    // the RAW bytes rather than off the repaired copy, because what follows the
    // prefix is a path to open. A control character is valid UTF-8 and so
    // occupies the same bytes in both, which is what makes the cut transferable;
    // an INVALID trailing byte repairs to U+FFFD, which is not a control
    // character, so it is left alone in both.
    let mut end = raw.len();
    if let Some(c) = text.chars().next_back() {
        if text::is_cntrl(c) {
            end -= c.len_utf8();
        }
    }
    // The path after the prefix must be non-empty.
    let rest = raw[..end].strip_prefix(b"gitdir: ".as_slice())?;
    if rest.is_empty() {
        return None;
    }
    Some(if rest.starts_with(b"/") {
        path_of(rest)
    } else {
        parent_of(candidate).join(OsStr::from_bytes(rest))
    })
}

/// The first line of a small file: no escape processing, NUL bytes dropped, and
/// `None` for a file that will not open or read. 1 MiB is far past any HEAD, any
/// gitfile or `/proc/sys/kernel/hostname`, and it keeps a pathological file from
/// being slurped whole; the callers reject anything over 4096 characters anyway.
pub fn first_line(path: &Path) -> Option<Vec<u8>> {
    let f = File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(1 << 20).read_to_end(&mut buf).ok()?;
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    buf.truncate(end);
    if buf.contains(&0) {
        buf.retain(|&b| b != 0);
    }
    Some(buf)
}

fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A throwaway directory under the test process's own temp dir. No crate for
    /// it, and no cleanup: the harness's temp dir is the cleanup.
    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!(
                "tabstatus-git-{}-{}-{:?}",
                std::process::id(),
                tag,
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).expect("temp dir");
            Dir(p)
        }
        /// A `.git` DIRECTORY whose HEAD holds `head`.
        fn repo(&self, head: &[u8]) -> PathBuf {
            let gd = self.0.join(".git");
            fs::create_dir_all(&gd).expect("mkdir .git");
            fs::write(gd.join("HEAD"), head).expect("write HEAD");
            gd
        }
        fn file(&self, name: &str, body: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            fs::write(&p, body).expect("write");
            p
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_symref_into_refs_heads_is_the_branch() {
        let d = Dir::new("symref");
        assert_eq!(branch_of(&d.repo(b"ref: refs/heads/master\n")).as_deref(), Some("master"));
        assert_eq!(
            branch_of(&d.repo(b"ref: refs/heads/feat/a-b\n")).as_deref(),
            Some("feat/a-b")
        );
    }

    #[test]
    fn a_symref_elsewhere_keeps_its_namespace_minus_refs() {
        let d = Dir::new("ns");
        assert_eq!(
            branch_of(&d.repo(b"ref: refs/remotes/origin/main\n")).as_deref(),
            Some("origin/main")
        );
        assert_eq!(branch_of(&d.repo(b"ref: refs/tags/v1\n")).as_deref(), Some("tags/v1"));
        assert_eq!(branch_of(&d.repo(b"ref: dangling\n")).as_deref(), Some("dangling"));
    }

    #[test]
    fn a_detached_head_is_a_seven_character_prefix() {
        let d = Dir::new("detached");
        assert_eq!(
            branch_of(&d.repo(b"0123456789abcdef0123456789abcdef01234567\n")).as_deref(),
            Some("0123456")
        );
        // sha256 and uppercase are still all hex.
        assert_eq!(branch_of(&d.repo(&[b'a'; 64])).as_deref(), Some("aaaaaaa"));
        assert_eq!(branch_of(&d.repo(b"DEADBEEFCAFE\n")).as_deref(), Some("DEADBEE"));
        // Six is not enough, and a non-hex character is not an object id.
        assert_eq!(branch_of(&d.repo(b"012345\n")), None);
        assert_eq!(branch_of(&d.repo(b"0123456z\n")), None);
    }

    #[test]
    fn trailing_noise_comes_off() {
        let d = Dir::new("noise");
        assert_eq!(branch_of(&d.repo(b"ref: refs/heads/m\r\n")).as_deref(), Some("m"));
        assert_eq!(branch_of(&d.repo(b"ref: refs/heads/m  \t \n")).as_deref(), Some("m"));
        assert_eq!(branch_of(&d.repo(b"ref: refs/heads/m")).as_deref(), Some("m"));
    }

    #[test]
    fn an_unborn_or_empty_head_is_not_a_repository() {
        let d = Dir::new("unborn");
        assert_eq!(branch_of(&d.repo(b"")), None);
        assert_eq!(branch_of(&d.repo(b"\n")), None);
        assert_eq!(branch_of(&d.repo(b"ref: \n")), None);
        assert_eq!(branch_of(&d.repo(b"ref: refs/heads/\n")), None);
        assert_eq!(branch_of(&d.repo(b"garbage\n")), None);
    }

    #[test]
    fn an_absurdly_long_head_line_is_refused_before_it_is_parsed() {
        let d = Dir::new("long");
        let mut head = b"ref: refs/heads/".to_vec();
        head.resize(300, b'x');
        assert_eq!(branch_of(&d.repo(&head)), None);
        // 255 characters exactly is still read.
        let mut ok = b"ref: refs/heads/".to_vec();
        ok.resize(255, b'y');
        assert!(branch_of(&d.repo(&ok)).is_some());
    }

    #[test]
    fn an_invalid_byte_in_a_branch_becomes_one_replacement_character() {
        let d = Dir::new("badbyte");
        assert_eq!(
            branch_of(&d.repo(b"ref: refs/heads/m\xffn\n")).as_deref(),
            Some("m\u{fffd}n")
        );
    }

    #[test]
    fn an_empty_git_directory_is_not_a_repository() {
        let d = Dir::new("emptydir");
        let gd = d.0.join(".git");
        fs::create_dir_all(&gd).expect("mkdir");
        assert_eq!(branch_of(&gd), None);
    }

    #[test]
    fn a_gitdir_pointer_is_followed_absolute_and_relative() {
        let d = Dir::new("pointer");
        let real = d.repo(b"ref: refs/heads/wt\n");
        // Absolute.
        let ptr = d.file("dotgit-abs", format!("gitdir: {}\n", real.display()).as_bytes());
        assert_eq!(branch_of(&ptr).as_deref(), Some("wt"));
        // Relative to the directory holding the pointer, and a CR survives from
        // a Windows checkout.
        let ptr = d.file("dotgit-rel", b"gitdir: .git\r\n");
        assert_eq!(branch_of(&ptr).as_deref(), Some("wt"));
        // No newline at all.
        let ptr = d.file("dotgit-bare", b"gitdir: .git");
        assert_eq!(branch_of(&ptr).as_deref(), Some("wt"));
    }

    #[test]
    fn a_garbage_or_stale_gitdir_pointer_is_not_a_repository() {
        let d = Dir::new("stale");
        assert_eq!(branch_of(&d.file("a", b"gitdir: /no/such/place\n")), None);
        assert_eq!(branch_of(&d.file("b", b"gitdir: \n")), None);
        assert_eq!(branch_of(&d.file("c", b"not a gitdir line\n")), None);
        assert_eq!(branch_of(&d.file("d", b"")), None);
        // Longer than PATH_MAX is some other file that happens to be called .git.
        let mut long = b"gitdir: /".to_vec();
        long.resize(5000, b'z');
        assert_eq!(branch_of(&d.file("e", &long)), None);
    }

    #[test]
    fn the_walk_finds_the_innermost_repository_and_steps_over_junk() {
        let d = Dir::new("walk");
        d.repo(b"ref: refs/heads/outer\n");
        let deep = d.0.join("a/b/c");
        fs::create_dir_all(&deep).expect("mkdir");
        let found = find_repo(&deep, &deep, None).expect("outer repo");
        assert_eq!(found.branch, "outer");
        assert_eq!(found.top, d.0);

        // An empty `.git` halfway up must not shadow the repository above it.
        fs::create_dir_all(d.0.join("a/b/.git")).expect("mkdir junk");
        let found = find_repo(&deep, &deep, None).expect("outer repo still");
        assert_eq!(found.branch, "outer");
        assert_eq!(found.top, d.0);

        // A real inner repository does win.
        fs::write(d.0.join("a/b/.git/HEAD"), b"ref: refs/heads/inner\n").expect("write");
        let found = find_repo(&deep, &deep, None).expect("inner repo");
        assert_eq!(found.branch, "inner");
        assert_eq!(found.top, d.0.join("a/b"));
    }

    #[test]
    fn the_walk_is_bounded() {
        let d = Dir::new("bound");
        d.repo(b"ref: refs/heads/top\n");
        // The walk probes the starting directory itself, so a repository
        // WALK_LIMIT - 1 components above the cwd is the last one reachable.
        let mut at = d.0.clone();
        for _ in 0..WALK_LIMIT - 1 {
            at = at.join("d");
        }
        fs::create_dir_all(&at).expect("mkdir");
        assert!(find_repo(&at, &at, None).is_some(), "the last step still probes");
        let past = at.join("d");
        fs::create_dir_all(&past).expect("mkdir");
        assert!(find_repo(&past, &past, None).is_none(), "one step past the bound");
    }

    #[test]
    fn git_dir_names_the_repository_after_itself() {
        assert_eq!(repo_top(Path::new("/w/repo/.git")), PathBuf::from("/w/repo"));
        assert_eq!(repo_top(Path::new("/w/repo/.git/")), PathBuf::from("/w/repo"));
        assert_eq!(repo_top(Path::new("/srv/proj.git")), PathBuf::from("/srv/proj"));
        // No `.git` to drop: a dot component is left for `place` to reject.
        assert_eq!(repo_top(Path::new("/w/repo/.git/.")), PathBuf::from("/w/repo/.git/."));
        assert_eq!(repo_top(Path::new("/.git")), PathBuf::from(""));
    }

    #[test]
    fn a_relative_git_dir_resolves_against_the_logical_cwd() {
        let l = Path::new("/w/repo");
        assert_eq!(absolute(l, OsStr::new(".git")), PathBuf::from("/w/repo/.git"));
        assert_eq!(absolute(l, OsStr::new(".git/.")), PathBuf::from("/w/repo/.git/."));
        assert_eq!(absolute(l, OsStr::new("/abs/.git")), PathBuf::from("/abs/.git"));
    }

    #[test]
    fn one_step_up_cuts_at_the_last_slash() {
        assert_eq!(parent_of(Path::new("/a/b")), PathBuf::from("/a"));
        assert_eq!(parent_of(Path::new("/a")), PathBuf::from("/"));
        assert_eq!(parent_of(Path::new("/")), PathBuf::from("/"));
        // Where `Path::parent` would have said `/a`.
        assert_eq!(parent_of(Path::new("/a/b/")), PathBuf::from("/a/b"));
        // No slash at all: unchanged, and the walk's bound is what stops it.
        assert_eq!(parent_of(Path::new("rel")), PathBuf::from("rel"));
    }

    #[test]
    fn first_line_stops_at_the_newline_and_drops_nul_bytes() {
        let d = Dir::new("firstline");
        assert_eq!(first_line(&d.file("a", b"one\ntwo\n")), Some(b"one".to_vec()));
        assert_eq!(first_line(&d.file("b", b"no newline")), Some(b"no newline".to_vec()));
        assert_eq!(first_line(&d.file("c", b"a\0b\n")), Some(b"ab".to_vec()));
        assert_eq!(first_line(&d.file("d", b"")), Some(Vec::new()));
        assert_eq!(first_line(&d.0.join("no-such-file")), None);
    }
}
