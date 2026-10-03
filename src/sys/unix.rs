//! The Unix backend of [`crate::sys`]. One body for every Unix, except for the
//! handful of questions whose ANSWER differs: a process's start time and its
//! liveness, the session's pty, the record's origin key, and the variable naming
//! the state directory. Those are `cfg`-selected in place - Linux reads `/proc`,
//! macOS calls `libc` - and everything else on this page is shared. WHY THEY SIT
//! NEXT TO EACH OTHER rather than in a file of their own is the last section here,
//! because it has been asked and it deserves an answer and not an assertion.
//!
//! ONLY THE SYSTEM CALL IS `cfg`-SELECTED. Every decision either side makes - the
//! errno-to-liveness mapping, the packing of a start time into the one number a
//! record stores - is a pure function compiled on both and exercised by the Linux
//! test run. Native arm64 macOS CI additionally exercises the actual calls and
//! PTY delivery; cross-checks alone do not establish runtime correctness. The
//! `cfg` bodies are a call and a `?`; their decisions stay testable on Linux.
//!
//! THE LINE IS `libc`, NOT THE PLATFORM. That is the rule, and it is the one to follow
//! when adding anything here. An item that names a `libc` symbol is a CALL, and it
//! carries its own `#[cfg(target_os = "macos")]`: `libc` is a
//! `[target.'cfg(target_os = "macos")'.dependencies]` entry, so on a Linux build the
//! crate does not exist and naming it is a hard error. An item that names no `libc`
//! symbol is a DECISION, and it is compiled on both under
//! `#[cfg(any(target_os = "macos", test))]`. `EPERM` and `ESRCH` are written out by
//! hand for exactly that reason - spelling them `libc::EPERM` would move
//! `liveness_from_kill` to the wrong side of the line - and the macOS-only assert
//! beside them is what checks the two numbers against the real ones on a build that
//! has them.
//!
//! WHY THIS IS ONE FILE AND NOT THREE. The mix reads like two files wedged into one,
//! and the answer is still no. Four reasons, each measured rather than assumed.
//!
//! THE COUNT IS SMALLER THAN IT LOOKS. Of the 28 `#[cfg]` sites, 14 are the mix: SEVEN
//! questions with two answers - [`ORIGIN_KEY`], [`RUNTIME_DIR_VAR`], [`NO_STATE_DIR`],
//! [`kernel_hostname_file`], [`process_start_time`], [`process_alive`] and `fd1_path`.
//! The first four are a constant or a one-expression function and now sit in one
//! 47-line run, two of them under a SINGLE doc comment that explains both answers at
//! once - which a split would have to duplicate or cut in half. The other three are a
//! system call each. Of the remaining 14 sites, seven are the `any(macos, test)`
//! decisions, which are one body compiled on both and so the opposite of a mix; one is
//! a Linux-only parse; two are macOS-only declarations with no counterpart anywhere
//! (`mod abi` and the errno assert); four are the test module and its three Linux-only
//! tests. Seven forks in a thousand lines, in two clusters, is not the file the
//! attribute count describes.
//!
//! THE ADJACENCY IS THE PROOF, and it is the real argument. Linux
//! [`process_start_time`] and macOS [`process_start_time`] are 48 lines apart, and
//! `bsdinfo_start` - the pure function both of them are stripped down to - is 107
//! lines below that, so that the two kernels being asked the SAME question is
//! something a reviewer checks by scrolling. `proc_spells_a_pid_canonically_...`
//! exists only to show the two answer one string identically. Across three files each
//! of those becomes a claim in a comment instead of something the eye can check.
//!
//! THE PROPERTY WORTH PROTECTING IS ALREADY COMPILER-ENFORCED, and a split would
//! WEAKEN it. `mod tests` below carries no `target_os` gate and names every
//! dual-compiled item, so mistagging one is a build failure and not a silent skip:
//! retag `bsdinfo_start` as macOS-only and the Linux test run answers
//! `error[E0425]: cannot find function bsdinfo_start`, five times. Under a
//! `#[cfg(target_os = "macos")] mod macos;` the same mistake - a decision written into
//! the macOS half with its own test beside it - compiles clean on Linux and executes
//! nothing. The guard is the un-gated test module, and moving the decisions away from
//! it is what would remove the guard.
//!
//! AND std's OWN SHAPE AGREES. `sys/fs/unix.rs` is longer than this page with more
//! `target_os` forks in it, and std interleaves them in place; where std does give an
//! OS a file of its own - `sys/random/apple.rs` - it is a subsystem with a separate
//! implementation, not a handful of divergent answers. std nests, but its
//! `sys/pal/unix/` children are subsystems and not operating systems. By that
//! criterion this file is a `fs/unix.rs`.
//!
//! WHAT A THIRD UNIX WOULD COST, which is the test any layout here has to pass.
//! FreeBSD widens `any(target_os = "macos", test)` by one term and adds one arm to
//! three questions. Under a per-OS split it would ALSO have to move
//! `liveness_from_kill` and `pid_to_ask_about` back out of a file named for macOS,
//! because they are facts about `kill(2)` that every BSD shares and not macOS facts at
//! all. The seam that looks tidiest today is the one that would have to be undone.
//!
//! THE ONE THING THAT WAS GENUINELY FILE-SHAPED has been named rather than moved:
//! `mod abi`, the two `proc_info.h` declarations and the seven asserts that pin their
//! layout. Those have no Linux counterpart, so they had nothing to be adjacent to, and
//! one `#[cfg]` on the module replaced ten on its items. If they ever do leave this
//! file, that module is already the boundary and nothing else has to move with them.
//!
//! THE ACCEPTANCE GATE for any change to this file, and the cheapest one there is: the
//! leaf names of `cargo test -- --list` must come back unchanged. A decision that
//! stops being compiled still reports success, because a test that does not exist
//! cannot fail. The `unix` guard is load-bearing the same way, and it comes free from
//! `#[cfg(unix)] mod unix;` in [`super`] rather than from anything written here:
//! `cfg(test)` is true on Windows too, and `parse_pid` reads `OsStrExt::as_bytes`.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, FileType, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::FileId;

/// [`lock_exclusive`] locks, and [`file_id_of`] and [`file_id_at`] can prove the
/// lock is on the file a path names: `flock`, and device and inode.
pub const HAS_RECORD_LOCK: bool = true;

/// [`mode`] always answers, and [`set_mode`] applies what it is given.
pub const HAS_MODES: bool = true;

/// [`session_tty`] can resolve a pty on every Unix here. Linux reads the symlink
/// `/proc/<pid>/fd/1`; macOS asks `proc_pidfdinfo` for the same fd's vnode path.
/// Only that lookup differs - see [`fd1_path`] - and the guard downstream of it is
/// one body. The constant and the function have to agree, because `doctor` reads the
/// constant to say whether `session-start` and `session-end` have a route at all.
///
/// THREE CALLERS, NOT ONE, which is worth saying where the constant is written
/// because flipping it on macOS moved two of them without touching their files.
/// `manage.rs`'s `doctor` prints the `session terminal` row from it;
/// `mux::Channel::Direct::carries_raw` reads it to decide whether the session's own
/// tab is a BYTE route, and `mux::route` therefore now sends Konsole's OSC 50
/// arming down that channel on macOS as it already did on Linux. That is the
/// intended meaning - the two facts are one fact, "the session's own tab is a pty
/// we can write to" - and it is inert unless [`crate::surface::Surface::Konsole`]
/// was detected anyway. It is still a behaviour change in a file this declaration
/// does not name, so it is named here.
pub const HAS_SESSION_TTY: bool = true;

/// [`set_session_title`] has no console to title: a Unix terminal takes its title
/// as bytes on the pty, through [`session_tty`].
pub const HAS_SESSION_CONSOLE: bool = false;

/// Bytes back into an `OsStr` - the inverse of [`OsStr::as_encoded_bytes`]. On
/// Unix any byte string is an `OsStr`, so this borrows and never alters a byte.
pub fn os_str_from_bytes(b: &[u8]) -> Cow<'_, OsStr> {
    Cow::Borrowed(OsStr::from_bytes(b))
}

/// A path's encoded bytes as a title shows them: on Unix exactly as they are, and
/// `text::repair` makes each invalid byte one U+FFFD.
pub fn display_bytes(b: &[u8]) -> Cow<'_, [u8]> {
    Cow::Borrowed(b)
}

/// The owned form of [`os_str_from_bytes`], without a copy.
pub fn os_string_from_vec(v: Vec<u8>) -> OsString {
    OsString::from_vec(v)
}

/// Where the home directory is when `HOME` is unset: nowhere else on Unix.
pub fn home_fallback() -> Option<OsString> {
    None
}

/// Whether `b` belongs to a line ending in a command's output: only `\n`.
pub fn is_line_end(b: u8) -> bool {
    b == b'\n'
}

/// Whether a resolved gitdir, `GIT_DIR` or `HEAD` path may be handed to the
/// filesystem call that probes it. Always yes on Unix: a path is just a path, there
/// is no network share a bare `stat` can reach and so no credential to leak, and the
/// repository walk is meant to follow a `.git` that lives anywhere a symlink points.
/// `base` - the trusted directory the path was derived from - is unused here, and no
/// system call is made, so the hot path keeps exactly the shape it had before this
/// guard existed. The Windows backend is where the check has teeth.
pub fn gitpath_allowed(_path: &Path, _base: &Path) -> bool {
    true
}

/// The identity of the file `m` describes. Always known on Unix.
pub fn file_id(m: &Metadata) -> Option<FileId> {
    Some((m.dev(), u128::from(m.ino())))
}

/// The identity of the file an open handle names: its `fstat`.
pub fn file_id_of(f: &File) -> Option<FileId> {
    file_id(&f.metadata().ok()?)
}

/// The identity of the entry `path` names now, a link as itself. That is in the
/// `lstat` the caller already holds, so nothing is read again.
pub fn file_id_at(_path: &Path, lstat: &Metadata) -> Option<FileId> {
    file_id(lstat)
}

/// Block until `f` holds the exclusive record lock: `flock(LOCK_EX)`, advisory,
/// released when the file is closed or the process exits.
pub fn lock_exclusive(f: &File) -> io::Result<()> {
    f.lock()
}

/// Replace `to` with `from` in one step: `rename(2)`, which moves a name and leaves
/// every open handle - the writer's own locked one included - on the old inode.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Whether [`replace_file`] can replace a file in `dir` that its writer holds open
/// and locked: always - `rename(2)` does not ask who has the file open.
pub fn replaces_open_files(_dir: &Path) -> bool {
    true
}

/// Whether a word of the session-id grammar names a device rather than a file in a
/// directory: never, here.
pub fn reserved_name(_name: &str) -> bool {
    false
}

/// The key a record's origin is written under. It differs per platform because the
/// numbers do: a pid and a start time from one OS say nothing about a process on
/// another, and each side reads the other's key as an unknown field - no origin.
///
/// Three keys because there are three ENCODINGS, not three operating systems:
/// `p` is clock ticks since boot, `q` a 100ns FILETIME, and `r` microseconds since
/// the epoch (see [`process_start_time`]). A number under a key this build does not
/// know is not a start time it can compare, and `Record::parse` skips it like any
/// unknown field - absent, never different.
#[cfg(not(target_os = "macos"))]
pub const ORIGIN_KEY: &str = "p";
#[cfg(target_os = "macos")]
pub const ORIGIN_KEY: &str = "r";

/// The variable naming the per-user directory the state directory defaults under.
#[cfg(not(target_os = "macos"))]
pub const RUNTIME_DIR_VAR: &str = "XDG_RUNTIME_DIR";

/// `XDG_RUNTIME_DIR` is a freedesktop variable and macOS does not set one, so
/// reading it there switches the whole state layer - wait ownership included - off
/// on a platform that has everywhere to put a record. `TMPDIR` is what launchd
/// sets per user, to `/var/folders/<hash>/T`: 0700, owned by this user, and emptied
/// by the OS, so its volatility matches `XDG_RUNTIME_DIR`'s and the reaper's
/// one-day mtime fallback keeps the justification it already has.
#[cfg(target_os = "macos")]
pub const RUNTIME_DIR_VAR: &str = "TMPDIR";

/// Why there is no state directory, when neither variable is set.
#[cfg(not(target_os = "macos"))]
pub const NO_STATE_DIR: &str = "no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR";
#[cfg(target_os = "macos")]
pub const NO_STATE_DIR: &str = "no CCTAB_STATE_DIR and no TMPDIR";

/// The file the kernel publishes its host name in, read without a fork.
#[cfg(not(target_os = "macos"))]
pub fn kernel_hostname_file() -> Option<&'static Path> {
    Some(Path::new("/proc/sys/kernel/hostname"))
}

/// No file publishes it here: macOS has no `/proc`, and `kern.hostname` is a
/// `sysctl` and not a path. `None` sends [`crate::location::hostname`] straight on
/// to `$HOSTNAME` and then to `hostname(1)`, which is where it arrived anyway -
/// one guaranteed-failing `open` later.
#[cfg(target_os = "macos")]
pub fn kernel_hostname_file() -> Option<&'static Path> {
    None
}

/// The permission bits, `0o7777`-masked.
pub fn mode(m: &Metadata) -> Option<u32> {
    Some(m.permissions().mode() & 0o7777)
}

/// Set the permission bits exactly - not narrowed by the umask, which is the point.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Ask `open(2)` for `mode` when the file is created. The umask still applies; a
/// caller that needs the exact bits follows with [`set_mode`].
pub fn with_mode(opts: &mut OpenOptions, mode: u32) -> &mut OpenOptions {
    opts.mode(mode)
}

/// What a rewrite carries over from the file it replaces BEYOND its mode - and on
/// Unix that is nothing: the mode is the protection, and the caller already keeps
/// it. Uninhabited, so [`security_of`] provably answers `None` and
/// [`create_secured`] is never reached: no syscall is added to any Unix write.
pub enum Security {}

/// Always `None`, without a syscall; see [`Security`].
pub fn security_of(_path: &Path) -> io::Result<Option<Security>> {
    Ok(None)
}

/// Unreachable: there is no [`Security`] to apply.
pub fn create_secured(_path: &Path, sec: &Security) -> io::Result<File> {
    match *sec {}
}

/// Whether any execute bit is set. Always an answer on Unix.
pub fn is_executable(m: &Metadata) -> Option<bool> {
    Some(m.permissions().mode() & 0o111 != 0)
}

/// `mkdir -p` with mode 0700 on what it creates.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

/// What [`link_dir`] makes, in the word a report line uses for it.
pub const DIR_LINK: &str = "symlink";

/// Whether the file of a running executable can be removed: `unlink(2)` takes the
/// name and every running process keeps its inode.
pub const HAS_UNLINK_RUNNING: bool = true;

/// A directory link at `link` naming `target`: a symlink.
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Whether [`link_dir`] could make a link to `target` in `dir`, asked before the
/// first write. Nothing to ask here, so nothing is touched: `symlink(2)` needs only
/// write permission on `dir`, and the caller has already probed that.
pub fn probe_dir_link(_target: &Path, _dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Remove a link made by [`link_dir`] - the link, never what it names: `unlink(2)`.
pub fn remove_dir_link(link: &Path) -> io::Result<()> {
    fs::remove_file(link)
}

/// Move the directory link `from` onto `to`, replacing the link there. `rename(2)`
/// over a symlink is atomic, so `to` resolves to the old target or the new one at
/// every instant and never to nothing.
pub fn replace_dir_link(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// Move the file `from` onto `to`, which may be an executable something is running
/// right now: `rename(2)`, which leaves every running process on its own inode and
/// is atomic for the name. Nothing is ever set aside, so the answer is `None`.
pub fn replace_running(from: &Path, to: &Path) -> io::Result<Option<PathBuf>> {
    fs::rename(from, to).map(|()| None)
}

/// Remove what [`replace_dir_link`] and [`replace_running`] left behind in `dir`,
/// and say what that was. `rename(2)` leaves nothing, so there is nothing to find.
pub fn sweep_replaced(_dir: &Path) -> Vec<PathBuf> {
    Vec::new()
}

/// The spelling of `p` that is compared, printed and recorded: `p` itself. A Unix
/// path has one spelling per name - no short names, no verbatim prefix - and case
/// is the filesystem's business, not this program's.
pub fn normalize(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// Whether `a` and `b` are the same path: component-wise equality.
pub fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

/// Whether `p` is `base` or lies beneath it: a component-prefix test, lexical.
pub fn is_within(p: &Path, base: &Path) -> bool {
    p == base || p.starts_with(base)
}

/// What follows `home` in `path`, when `path` is `home` or lies beneath it: empty for
/// `home` itself, else the rest from its `/` on. Exact bytes, case and all - a doubled
/// slash in a `$PWD` kept verbatim stays doubled, which `Path::starts_with` would not
/// allow - and the `/` after `home` is required, so `/home/alex` cannot claim
/// `/home/alex2`.
pub fn strip_home_prefix(path: &Path, home: &Path) -> Option<OsString> {
    let rest = path.as_os_str().as_bytes().strip_prefix(home.as_os_str().as_bytes())?;
    (rest.is_empty() || rest[0] == b'/').then(|| OsStr::from_bytes(rest).to_owned())
}

/// The command a report hands an operator to remove the directory `p` outright,
/// pasteable into any POSIX shell: the path as it is when every byte is one no shell
/// treats specially, single-quoted otherwise. Unquoted, a tree under `~/my tree`
/// printed `rm -rf /home/me/my tree/claude-tabstatus`, which removes `/home/me/my`.
pub fn remove_dir_command(p: &Path) -> String {
    let s = p.display().to_string();
    let plain = !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+,:@%".contains(&b));
    if plain {
        format!("rm -rf {}", s)
    } else {
        format!("rm -rf '{}'", s.replace('\'', r"'\''"))
    }
}

/// Whether `name` is one [`replace_running`] or [`replace_dir_link`] set aside.
/// Neither sets anything aside here, so nothing is.
pub fn is_set_aside(_name: &str) -> bool {
    false
}

/// Field 22 of `/proc/<pid>/stat`: the process start time, in clock ticks since
/// boot. `None` when the process is gone. The read is the only part of this that
/// is a system call; [`start_time_from_stat`] is the parse.
#[cfg(not(target_os = "macos"))]
pub fn process_start_time(pid: u32) -> Option<u64> {
    start_time_from_stat(&fs::read(format!("/proc/{}/stat", pid)).ok()?)
}

/// That parse, as a pure function of the bytes the file holds - the same rule the
/// rest of this page follows, and here it is what makes the non-UTF-8 case below
/// reachable from a test at all.
///
/// Read after the LAST `") "`, never by splitting the whole line on spaces. Field 2
/// is the executable name in parentheses and may itself contain both - measured on
/// this machine, `/proc/1259713/stat` holds `(npm exec chrome...)`, so the naive
/// split reads the wrong field for exactly the processes a `claude` session spawns.
/// The tail begins at field 3, so field 22 is its 20th word.
///
/// BYTES, and no longer `read_to_string`: field 2 is `comm`, the first 15 bytes of
/// the executable's name, which the kernel copies out unvalidated. `read_to_string`
/// answers `Err` for a name that is not UTF-8, so a LIVE and fully readable process
/// reported no start time - measured here by running a copy of `sleep` renamed with
/// a `0xff` byte in it, whose `/proc/<pid>/stat` fails to decode while every field
/// this reads is plain ASCII. `from_utf8_lossy` replaces bytes only INSIDE the
/// parenthesised name, which the split below skips past, so the fields it reads are
/// unchanged and a valid-UTF-8 `stat` parses byte for byte as it did.
#[cfg(not(target_os = "macos"))]
fn start_time_from_stat(raw: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(raw);
    let w = text.rsplit_once(") ")?.1.split(' ').nth(19)?;
    if w.is_empty() || w.len() > 20 || !w.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    w.parse().ok()
}

/// Whether `pid` names a running process: its `/proc` entry exists.
#[cfg(not(target_os = "macos"))]
pub fn process_alive(pid: u32) -> Option<bool> {
    Some(Path::new(&format!("/proc/{}", pid)).exists())
}

/// The macOS start time: `proc_pidinfo`'s `PROC_PIDTBSDINFO`, whose
/// `pbi_start_tvsec`/`pbi_start_tvusec` pair [`bsdinfo_start`] packs into the one
/// number a record stores. `None` when the pid is gone, is not a pid this platform
/// can name, or the kernel filled less than the whole struct.
///
/// `libc` declares both the call and `proc_bsdinfo`, so nothing here is a
/// hand-written `#[repr(C)]` layout. The native process test checks the returned
/// start time against the child's creation interval, as well as its stable identity;
/// cross-checking a declaration alone cannot validate the kernel's answer.
#[cfg(target_os = "macos")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    let pid = pid_to_ask_about(pid)?;
    let want = std::mem::size_of::<libc::proc_bsdinfo>();
    let size = i32::try_from(want).ok()?;
    // SAFETY: `proc_bsdinfo` is plain integers and byte arrays, so all-zero is a
    // valid value of it; nothing is read out of the buffer except through
    // `bsdinfo_start`, which answers only when the kernel says it filled all of it.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer and `size` describe that same live buffer exactly, so the
    // call writes at most `size` bytes inside it; it keeps no pointer to it, and
    // `info` outlives the call.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    bsdinfo_start(filled, want, info.pbi_start_tvsec, info.pbi_start_tvusec)
}

/// Whether `pid` names a running process, asked with `kill(pid, 0)`: signal 0 is
/// the existence-and-permission question and DELIVERS NOTHING. [`liveness_from_kill`]
/// decides what the answer means.
#[cfg(target_os = "macos")]
pub fn process_alive(pid: u32) -> Option<bool> {
    let pid = pid_to_ask_about(pid)?;
    // SAFETY: two integers to a libc wrapper round a syscall; it reads and writes
    // no memory of ours, and signal 0 sends no signal to anything.
    let rc = unsafe { libc::kill(pid, 0) };
    // `last_os_error` is read unconditionally and is stale when `rc` is 0 - which
    // is the one case the mapping decides without looking at it.
    liveness_from_kill(rc, io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

/// `pid` as the `pid_t` these two ASK ABOUT, or `None` when it is not one - the
/// third decision on this page, and pure for the same reason the other two are.
///
/// `kill(2)` does not take a pid alone: it reads 0 as "every process in MY OWN
/// process group" and a negative number as "the group whose id is -pid". Both
/// SUCCEED, and `kill(0, 0)` succeeds always, because the caller is in its own
/// group - so a record naming pid 0 would be answered `Some(true)`, a live process
/// that does not exist, by the one function whose whole purpose is to answer only
/// what it can prove. `i32::try_from` rules out the negatives already, since a
/// `u32` above `i32::MAX` does not convert; 0 is what has to be named, and it is
/// named HERE rather than in either body so that a Linux test runs it.
///
/// `None`, not `Some(false)`: this is a question that cannot be asked, and the
/// invariant is that only evidence - `ESRCH` - claims a death. Linux answers
/// `Some(false)` for the same pids because `/proc/0` genuinely is not there, which
/// is evidence; macOS has none to offer and says so.
#[cfg(any(target_os = "macos", test))]
fn pid_to_ask_about(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok().filter(|p| *p > 0)
}

/// The two `errno` values the mapping below names. POSIX fixes both, and macOS and
/// Linux agree on them; naming them here rather than reaching for `libc::ESRCH` is
/// what lets a Linux `cargo test` run the decision. The macOS build checks them
/// against the platform's own constants at compile time - see below - so a
/// disagreement is a build failure on the Mac and never a wrong answer on one.
#[cfg(any(target_os = "macos", test))]
const EPERM: i32 = 1;
#[cfg(any(target_os = "macos", test))]
const ESRCH: i32 = 3;

// The numbers above against the platform's own. `cargo check` for the Mac evaluates
// this, so the one fact a Linux test cannot see is checked by the compiler instead.
#[cfg(target_os = "macos")]
const _: () = assert!(EPERM == libc::EPERM && ESRCH == libc::ESRCH);

/// What a `kill(pid, 0)` outcome says about the process, as a pure function of the
/// return value and `errno` - so the macOS body above is a call and not a branch,
/// and this file's only liveness DECISION is one a Linux test runs.
///
/// `EPERM` is `Some(true)`: a process this user may not signal is a process that
/// EXISTS, which is the whole of what the reaper asks. Anything else is `None`,
/// "cannot tell", which [`same_process`] passes on and the reaper reads as "keep
/// the record". `Some(false)` is a CLAIM OF DEATH and is made for `ESRCH` alone -
/// this function's reason for existing is that the `/proc` body above used to make
/// that claim for every live process on macOS.
#[cfg(any(target_os = "macos", test))]
fn liveness_from_kill(rc: i32, errno: i32) -> Option<bool> {
    match (rc, errno) {
        (0, _) => Some(true),
        (_, ESRCH) => Some(false),
        (_, EPERM) => Some(true),
        _ => None,
    }
}

/// The start time a macOS record stores: MICROSECONDS since the epoch, which is
/// `proc_bsdinfo`'s seconds-and-microseconds pair as one number. A third encoding -
/// hence a third [`ORIGIN_KEY`] - and one `u64` holds it until the year 586524.
///
/// `filled` is what `proc_pidinfo` RETURNED, which is the byte count it wrote and
/// not a status: a dead pid fills 0, and a short fill would leave the fields this
/// reads holding the zeros the buffer was created with - a start time of 0, which
/// compares equal to the next short fill and would make a dead session's pid look
/// like the same process forever. Only an exactly-full struct is an answer.
///
/// Saturating, not wrapping: the multiply cannot overflow for any real start time,
/// and `panic = "abort"` means the debug-build check would be a crash in a hook
/// rather than a test failure.
#[cfg(any(target_os = "macos", test))]
fn bsdinfo_start(filled: i32, want: usize, tvsec: u64, tvusec: u64) -> Option<u64> {
    (usize::try_from(filled) == Ok(want))
        .then(|| tvsec.saturating_mul(1_000_000).saturating_add(tvusec))
}

/// Whether the process that recorded `(pid, start)` is still that process:
/// `Some(true)` it is, `Some(false)` provably not - gone, or the pid given to
/// another - and `None` when that cannot be told. The same two questions, in the
/// same order, that the reaper has always asked; only whom they are put to differs.
pub fn same_process(pid: u32, start: u64) -> Option<bool> {
    same_as_recorded(process_start_time(pid), start, || process_alive(pid))
}

/// That decision, as a pure function of the two answers, with the second asked only
/// when the first did not settle it - which is also what puts every combination,
/// including ones this machine cannot stage, in front of a test.
///
/// A start time settles it either way: it is the process under that pid NOW. Without
/// one, only a pid PROVEN not to exist says the record's process is gone. "Alive but
/// unreadable" is `None`, and the reaper keeps the record.
///
/// The old shape was "alive, therefore some OTHER process has this pid", and it was
/// safe only where that pair cannot arise. It arises immediately on macOS, where a
/// process this user may not inspect answers `EPERM`: alive, with no start time.
/// Reading that as a reused pid unlinks a LIVE session's record.
///
/// It is reachable on Linux too, which is worth saying plainly rather than claiming
/// `/proc` makes it impossible. A `hidepid` mount is one way. The other was found
/// here by measurement: `/proc/<pid>/stat` embeds `comm` verbatim, so a process
/// whose executable name is not UTF-8 decoded as `Err` and reported no start time
/// while being perfectly readable and alive. [`start_time_from_stat`] closes that
/// one at the source by parsing bytes, but the pair itself is not hypothetical and
/// this function is what makes it harmless either way.
///
/// Windows' backend has always had this shape - `Probe::Unknown` -> `None` - and
/// this is the Unix side agreeing with it.
fn same_as_recorded(
    now: Option<u64>,
    recorded: u64,
    alive: impl FnOnce() -> Option<bool>,
) -> Option<bool> {
    match now {
        Some(t) => Some(t == recorded),
        None => alive().and_then(|a| (!a).then_some(false)),
    }
}

/// Resolve the session's pty from `$CLAUDE_PID`. Hook subprocesses are detached,
/// with fd 0 on /dev/null and `exec 3>/dev/tty` failing, so /dev/tty is no use
/// here; `CLAUDE_PID` is exported into every hook subprocess.
///
/// THE HEADLESS GUARD: unless fd 1 of that pid resolves to a writable character
/// device under /dev/pts or /dev/tty, do nothing rather than retitle an unrelated
/// terminal - which covers a redirected `claude -p` and every platform where fd 1
/// cannot be resolved at all. `None` is therefore both "no tab to paint" and "fd 1
/// would not resolve", deliberately the same answer: painting on a guess is the one
/// outcome that retitles somebody else's terminal.
///
/// ONE body for every Unix. Only [`fd1_path`] is `cfg`-selected, because only the
/// lookup differs; the three checks below are the contract and they are the same
/// three whichever kernel answered.
pub fn session_tty(claude_pid: &OsStr) -> Option<File> {
    let target = fd1_path(claude_pid)?;
    if !is_tty_path(&target) || !is_char_device(&target.metadata().ok()?.file_type()) {
        return None;
    }
    // Asking whether it is writable and opening it are the same question; ask it
    // once.
    //
    // NO `O_NOCTTY`, and that is a decision rather than an omission, because this
    // opens somebody else's terminal by name and the flag is the obvious hardening.
    // What it would guard against is acquiring a controlling terminal by accident,
    // which takes a session leader that has none; the hook is a child of `claude`
    // and inherits its ctty, so it is never one, and an attempt to reproduce the
    // acquisition on Linux from a `setsid` process left `tty_nr` at 0. What it
    // would COST is the part that decides it: `libc` is a
    // `cfg(target_os = "macos")` dependency here, so spelling `O_NOCTTY` on Linux
    // means either widening that dependency to every Unix - which
    // `docs/backend-scouting.md` §5's gate governs and this is not the commit for -
    // or hand-writing a per-architecture octal constant, which is the exact kind of
    // guess the macOS declarations below go to such lengths not to make.
    OpenOptions::new().write(true).open(&target).ok()
}

/// What fd 1 of `claude_pid` names, where a `/proc` says so: the symlink
/// `/proc/<pid>/fd/1`, read as a path. The pid is pasted in as the bytes it arrived
/// as and never parsed - a name that is not a live pid's fd is a `read_link` error,
/// which is the same `None` a rejected parse would have produced. This is also the
/// line `tests/oracle/tabstatus.sh` implements as `readlink "/proc/$CLAUDE_PID/fd/1"`,
/// and the 312-case replay compares the two, so it stays a paste.
///
/// Measured while writing the macOS side, and left alone deliberately: because the
/// bytes are pasted, a `$CLAUDE_PID` of `../../dev` makes `/proc/../../dev/fd/1`,
/// and `/dev/fd` IS a symlink to `/proc/self/fd` - so that one name resolves, to
/// THIS hook's own fd 1. It buys nothing: the guard downstream still demands a
/// writable pty, so the worst it reaches is the terminal the hook is already
/// running in, and `$CLAUDE_PID` comes from the process that spawned the hook.
/// macOS cannot reach even that, for the unrelated reason that a system call takes
/// a number and `parse_pid` is what produces one - which is a `cfg` away from here,
/// so this sentence names it rather than linking it.
#[cfg(not(target_os = "macos"))]
fn fd1_path(claude_pid: &OsStr) -> Option<PathBuf> {
    let mut link = OsString::from("/proc/");
    link.push(claude_pid);
    link.push("/fd/1");
    fs::read_link(Path::new(&link)).ok()
}

/// The same question where there is no `/proc`: `proc_pidfdinfo` with the
/// `PROC_PIDFDVNODEPATHINFO` flavour, whose `vip_path` is the path fd 1's vnode
/// hangs at. [`fd1_path_from_vnode`] turns the answer into a path, and everything
/// after that is the shared guard above.
///
/// THE HEADLESS GUARD SURVIVES, which is the whole reason this route is usable:
/// the kernel serves this flavour only for a vnode, so a PIPE OR SOCKET on fd 1 is
/// `EBADF` and never a path, and an fd 1 redirected to a file yields that FILE's
/// path, which [`is_tty_path`] rejects. Nothing here has to recognise a headless
/// session; it falls out of what the call will and will not answer.
///
/// `e_tdev` from `proc_bsdinfo` - which [`process_start_time`] already reads, and
/// which needs no new declaration at all - is deliberately NOT used: it is the
/// CONTROLLING terminal, which a redirected `claude -p > file` still has, so it
/// would name a terminal that is not this session's output. That is exactly the
/// substitution the guard exists to refuse.
#[cfg(target_os = "macos")]
fn fd1_path(claude_pid: &OsStr) -> Option<PathBuf> {
    let pid = pid_to_ask_about(parse_pid(claude_pid)?)?;
    let want = std::mem::size_of::<abi::VnodeFdInfoWithPath>();
    let size = i32::try_from(want).ok()?;
    // SAFETY: every field of this struct, transitively, is an integer or an array
    // of integers, so all-zero is a valid value of it. Nothing is read out of it
    // except `pvip.vip_path`, and only after the byte count says the kernel filled
    // the whole of it.
    let mut info: abi::VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer and `size` describe that same live buffer exactly, so the
    // call writes at most `size` bytes inside it; it keeps no pointer to it, and
    // `info` outlives the call. The kernel refuses a `size` below the flavour's own
    // before it copies anything - see the declarations below - so the one way to
    // get this wrong is an error return.
    let nb = unsafe {
        libc::proc_pidfdinfo(
            pid,
            1,
            abi::PROC_PIDFDVNODEPATHINFO,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    // `vip_path` is `[[c_char; 32]; 32]` because libc spells a 1024-byte array that
    // way; flatten it into the bytes a path is made of. `as u8` is the identity on
    // whichever sign the platform gives `c_char` and copies no other byte.
    let raw: Vec<u8> = info.pvip.vip_path.iter().flatten().map(|&c| c as u8).collect();
    fd1_path_from_vnode(nb, want, &raw)
}

/// The kernel's answer read as a path, as a pure function of the byte count it
/// returned and the bytes it wrote - so the macOS body above is a call and a `?`,
/// and every DECISION in it is one a Linux `cargo test` runs.
///
/// `nb` is a byte count, not a status: `nb <= 0` is the error return, and it does
/// not convert. BOTH halves of that matter, because `libproc`'s userland wrapper
/// and the system call under it do not agree on how a failure looks - one reports
/// it as -1 and the other can surface 0. Requiring EXACTLY `want` refuses
/// every value that is not a full struct, by construction, so the distinction has
/// no way to matter here and no claim about it is relied on. `ENOENT` means the
/// vnode was REVOKED - an fd whose terminal went away - and it is the same `None`
/// as any other failure, because both mean there is no tab to paint.
///
/// A count that is not exactly `want` is a hard error and
/// not a short read to tolerate: the fields this reads would be holding the zeros
/// the buffer was created with, and a zero-length path is not a refusal this can
/// tell apart from a real one. The taxonomy is lsof's technique - `nb <= 0`, the
/// revoked vnode, a short count as an error - and none of its code.
///
/// It is also what makes a wrong `PROC_PIDFDVNODEPATHINFO` harmless: another
/// flavour fills its own smaller struct and returns ITS size, which is not `want`.
///
/// Then the NUL. `vip_path` is a C string in a fixed array, so the path ends at the
/// first zero byte and the rest is whatever was there. lsof forces a terminator
/// into the last byte before calling `strlen`; a Rust slice cannot be walked off
/// the end in the first place, so what is left is the policy, and the policy here
/// is stricter: an array with NO zero in it is refused rather than truncated to its
/// last byte. `MAXPATHLEN` counts the terminator, so a real path always has one,
/// and a truncated path names a DIFFERENT file - which is the one thing the guard
/// downstream cannot catch, since /dev/pts/12 truncated to /dev/pts/1 is also a
/// writable character device, belonging to somebody else's terminal.
///
/// An empty path is `None` for the same reason it is not a path.
///
/// The bytes become an `OsString` unaltered: a path is bytes on Unix, and a
/// filesystem that holds a name which is not UTF-8 still holds a name.
#[cfg(any(target_os = "macos", test))]
fn fd1_path_from_vnode(nb: i32, want: usize, raw: &[u8]) -> Option<PathBuf> {
    if usize::try_from(nb) != Ok(want) {
        return None;
    }
    let end = raw.iter().position(|b| *b == 0)?;
    let name = raw.get(..end)?;
    if name.is_empty() {
        return None;
    }
    Some(PathBuf::from(os_string_from_vec(name.to_vec())))
}

/// The macOS ABI: the two `proc_info.h` declarations `libc` does not carry, and
/// the compile-time guards that pin their layout. ONE `cfg` for the whole
/// module, because nothing in it has a Linux counterpart to sit beside.
#[cfg(target_os = "macos")]
mod abi {
    /// The flavour that answers with a vnode's path. `libc` 0.2.189 declares neither
    /// this nor the struct below - measured against the crate source, not assumed - so
    /// both are written out here; see the declarations for what makes that sound.
    pub(super) const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

    /// `struct proc_fileinfo`, and below it `struct vnode_fdinfowithpath`: the two
    /// halves of what `proc_pidfdinfo`'s `PROC_PIDFDVNODEPATHINFO` copies out.
    ///
    /// WHAT THIS CONFORMS TO. The interface is `xnu`'s `bsd/sys/proc_info.h` - the field
    /// order every caller of that flavour must match to interoperate at all. Nothing is
    /// copied from it: no text, no comments, no transcription. Apple's source is APSL
    /// 2.0 and this program is GPL-3.0-or-later, so an ABI is the only thing that may
    /// cross, and an ABI is an interface rather than an expression of one.
    ///
    /// LAYOUT CHECKS ARE NOT RUNTIME VALIDATION. The `const _` block below fails
    /// `cargo check` on both Apple ABIs unless the required sizes and offsets match
    /// the header. Native arm64 CI separately links and exercises this call through
    /// disposable PTYs and redirected stdout; see docs/architecture.md for execution
    /// status and remaining limits. The nested types are libc's OWN
    /// (`vinfo_stat`, `vnode_info`, `vnode_info_path`), and libc checks those against
    /// Apple's real SDK on its own CI; what is added here is five scalars and two
    /// fields, all of which the asserts pin.
    ///
    /// AND A WRONG SIZE WOULD NOT BE CORRUPTION ANYWAY. The kernel compares the
    /// `buffersize` it was handed against this flavour's own size and returns `ENOMEM`
    /// before it copies a byte, then copies out exactly that many. The direction is
    /// kernel to user into a buffer we sized ourselves, so a mistake is an error
    /// return that [`fd1_path_from_vnode`](super::fd1_path_from_vnode) reads as
    /// `None` - never a write past the end of anything.
    ///
    /// WHO MAY ASK. The gate is the same-user check, not an entitlement: this asks
    /// about `$CLAUDE_PID`, which is this user's own `claude`, and it works under SIP
    /// with no privilege of any kind.
    ///
    /// The fields are named for the ABI and read through `pvip` alone; the rest are
    /// here to occupy the bytes the kernel writes, which is what the asserts check.
    #[allow(dead_code)]
    #[repr(C)]
    pub(super) struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: libc::off_t,
        fi_type: i32,
        fi_guardflags: u32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    pub(super) struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pub(super) pvip: libc::vnode_info_path,
    }

    // The layout, checked by the compiler that will build for the Mac. These numbers
    // are the header's, and a build that disagrees with any one of them does not
    // produce a binary - which is the whole of what stands in for running this
    // anywhere. They are not vacuous: adding one spurious `u32` to `ProcFileInfo` here
    // fails THREE of them - `ProcFileInfo`'s size, `VnodeFdInfoWithPath`'s size and
    // `pvip`'s offset - identically on both Apple targets. Three failed asserts, and
    // four lines beginning `error`, because cargo appends its own summary line. The
    // number that means something is the three, and this comment said four until the
    // control was re-run and counted.
    //
    // `vip_path`'s offset is asserted too, because the path is read by flattening that
    // array: it must begin where the header puts it and run to the end of the struct,
    // which is 1176 - 152 = 1024 bytes, `MAXPATHLEN`.
    //
    // WHAT THESE CANNOT CATCH, exactly rather than roughly, because a limit described
    // loosely is worse than one described plainly. `ProcFileInfo` is four 32-bit fields
    // and one `off_t`, and `off_t`'s 8-byte alignment pins it to offset 8 - so ANY
    // permutation of `fi_openflags`, `fi_status`, `fi_type` and `fi_guardflags` leaves
    // every size and offset here unchanged and every assert green. Measured rather than
    // argued: `fi_type` and `fi_guardflags` transposed produced zero diagnostics on
    // both Apple targets. That is survivable here and only here, because this reads
    // NOTHING out of `proc_fileinfo` - a swapped pair inside it has no consequence at
    // all. What it reads is `pvip`, whose offset is asserted, and inside it `vip_path`,
    // whose offset is asserted, in a struct that is libc's own and is checked against
    // Apple's real SDK on libc's CI.
    //
    // One struct up, that hazard is not hypothetical: `darwin-libproc-sys` 0.2.0
    // declares `vnode_info` as `vi_stat, vi_type, vi_fsid, vi_pad` where the header has
    // `vi_stat, vi_type, vi_pad, vi_fsid`. Same 152 bytes, different offset for
    // `vi_fsid` - invisible to a size assert. It is one reason libc's declaration is
    // the one used here and no `libproc` wrapper crate is.
    //
    // One item each, and not one block: a const block stops at its first failure, and
    // what an operator on a Mac wants from a broken build is every number that moved.
    const _: () = assert!(size_of::<ProcFileInfo>() == 24);
    const _: () = assert!(size_of::<libc::vinfo_stat>() == 136);
    const _: () = assert!(size_of::<libc::vnode_info>() == 152);
    const _: () = assert!(size_of::<libc::vnode_info_path>() == 1176);
    const _: () = assert!(size_of::<VnodeFdInfoWithPath>() == 1200);
    const _: () = assert!(std::mem::offset_of!(VnodeFdInfoWithPath, pvip) == 24);
    const _: () = assert!(std::mem::offset_of!(libc::vnode_info_path, vip_path) == 152);
}

/// `$CLAUDE_PID` as a pid: 1-10 ASCII digits, canonical, and nothing else. The
/// `/proc` body needs no such thing - a bad name is a failed `read_link` - but a
/// system call takes a number, so this is where the bytes stop being bytes.
/// [`pid_to_ask_about`] then rules out the values that are not a `pid_t` at all.
///
/// NOT the same function as the Windows backend's `parse_pid`, and this comment
/// used to claim it was. Three digit rules are shared - non-empty, at most ten, all
/// ASCII digits - and two things differ. Windows ends in `.filter(|&p| p != 0)`;
/// here 0 is passed on and [`pid_to_ask_about`] is what refuses it, which the test
/// below pins for the pair rather than for either half. And leading zeros are
/// refused here, which Windows accepts.
///
/// THE LEADING ZERO IS NOT FUSSINESS, it is the one place these two Unixes could
/// have disagreed about the same string. Linux never parses at all: `/proc/007`
/// does not exist, because `/proc` spells its pids canonically, so `007` is a
/// failed `read_link` and no tab. Accepting it here would have made `007` resolve
/// pid 7's fd 1 on macOS and nothing on Linux - a split in a function whose whole
/// design is that only the system call differs. Unreachable from a real
/// `$CLAUDE_PID`, which Claude Code writes canonically; refused anyway, because
/// "unreachable" is a claim about today's caller and this is a claim about the
/// function.
#[cfg(any(target_os = "macos", test))]
fn parse_pid(raw: &OsStr) -> Option<u32> {
    let b = raw.as_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b.first() == Some(&b'0') {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

/// The console route to the session's title, which Unix does not have: `Ok(false)`,
/// "not painted", with no system call. Nothing calls it here - see
/// [`HAS_SESSION_CONSOLE`] - and it exists so both backends offer the same API.
pub fn set_session_title(_claude_pid: &OsStr, _title: &str) -> io::Result<bool> {
    Ok(false)
}

/// Write to a pty NAMED BY tmux - an attached client's terminal - under the same
/// guard [`session_tty`] applies to fd 1: under /dev/pts or /dev/tty, a character
/// device, and writable. `Ok(false)` is a refused/unresolvable destination;
/// `Ok(true)` is a completed write, not terminal acknowledgement. An open/write
/// error is returned; a write error may follow a partial write. The client may
/// have detached between the listing and the write.
pub fn write_tty(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if !is_tty_path(path) {
        return Ok(false);
    }
    if !path.metadata().is_ok_and(|m| is_char_device(&m.file_type())) {
        return Ok(false);
    }
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.write_all(bytes).map(|()| true)
}

/// A byte prefix, not `Path::starts_with`: /dev/ttyS0 is a single component, so
/// component matching would reject the serial consoles this is meant to allow.
fn is_tty_path(p: &Path) -> bool {
    let b = p.as_os_str().as_bytes();
    b.starts_with(b"/dev/pts/") || b.starts_with(b"/dev/tty")
}

fn is_char_device(ft: &FileType) -> bool {
    ft.is_char_device()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_process_identity_and_headless_guard_follow_a_real_child() {
        use std::process::{Command, Stdio};
        use std::time::{SystemTime, UNIX_EPOCH};

        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        // cat waits for EOF on our pipe. Its stdout is /dev/null, so this test
        // cannot target the developer's terminal even when cargo is interactive.
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start a disposable process");
        let pid = child.id();
        let start = process_start_time(pid);
        let again = process_start_time(pid);
        let alive = process_alive(pid);
        let same = start.and_then(|s| same_process(pid, s));
        let different = start.and_then(|s| same_process(pid, s + 1));
        let tty = session_tty(OsStr::new(&pid.to_string()));
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros();
        // Finish the child before assertions, including on a failing OS query.
        drop(child.stdin.take());
        assert!(child.wait().expect("reap the child").success());

        let start = start.expect("the kernel supplies a real process start time");
        assert!(start > 0);
        assert_eq!(again, Some(start));
        assert_eq!(alive, Some(true));
        assert_eq!(same, Some(true));
        assert_eq!(different, Some(false), "a changed start time is a different identity");
        assert!(tty.is_none(), "a character device alone is not a terminal");
        if cfg!(target_os = "macos") {
            assert!((before..=after).contains(&u128::from(start)), "epoch microseconds: {start}");
        }
        assert_eq!(process_start_time(pid), None);
        assert_eq!(process_alive(pid), Some(false));
        assert_eq!(same_process(pid, start), Some(false));
    }

    /// The reaper's liveness question, decided here so that the macOS body is a
    /// call and not a branch, and tested on Linux as well as native macOS.
    ///
    /// `EPERM` is the one that matters: a process this user may not signal exists,
    /// and calling it dead would unlink a live session's record. `Some(false)` is a
    /// claim, and `ESRCH` is the only evidence for it.
    #[test]
    fn a_signal_probe_claims_death_only_for_esrch() {
        assert_eq!(liveness_from_kill(0, ESRCH), Some(true), "a stale errno is not read");
        assert_eq!(liveness_from_kill(-1, ESRCH), Some(false));
        assert_eq!(liveness_from_kill(-1, EPERM), Some(true), "may not signal, but exists");
        // Everything else is "cannot tell", which the caller reads as "keep the
        // record" - never the confident wrong answer /proc gave here before.
        for errno in [0, 4, 9, 14, 22, 1000] {
            assert_eq!(liveness_from_kill(-1, errno), None, "errno {errno}");
        }
    }

    /// The pid a liveness question may be ASKED about. `kill(2)` answers a
    /// different question for 0 - "may I signal my own process group", which always
    /// succeeds - so letting 0 through would have `process_alive` report a live
    /// process for a pid nothing has: the exact mirror of the confident wrong
    /// `Some(false)` this whole change exists to remove.
    #[test]
    fn a_liveness_question_is_asked_only_about_a_pid_kill_reads_as_a_pid() {
        assert_eq!(pid_to_ask_about(0), None, "kill(0, 0) asks about MY process group");
        assert_eq!(pid_to_ask_about(1), Some(1));
        assert_eq!(pid_to_ask_about(99_999), Some(99_999));
        let top = u32::try_from(i32::MAX).expect("i32::MAX is a u32");
        assert_eq!(pid_to_ask_about(top), Some(i32::MAX));
        // Above `pid_t` nothing converts, so no `u32` can reach `kill` as the
        // negative number that would name a process GROUP.
        assert_eq!(pid_to_ask_about(top + 1), None);
        assert_eq!(pid_to_ask_about(u32::MAX), None);
    }

    /// `proc_pidinfo` returns the byte count it filled, so a partial fill is a
    /// buffer still holding its own zeros - and a start time of 0 would compare
    /// equal to the next one, which is a dead pid reading as the same process.
    #[test]
    fn a_start_time_is_read_only_from_a_completely_filled_struct() {
        // Any size stands in for `size_of::<proc_bsdinfo>()`: what is under test is
        // the comparison, which is all the macOS body delegates.
        let want: usize = 136;
        let full = |sec, usec| bsdinfo_start(136, want, sec, usec);
        assert_eq!(full(1_790_380_620, 123_456), Some(1_790_380_620_123_456));
        assert_eq!(full(0, 0), Some(0), "the epoch itself is still an answer");
        assert_eq!(bsdinfo_start(0, want, 1, 2), None, "a dead pid fills nothing");
        assert_eq!(bsdinfo_start(135, want, 1, 2), None, "a short fill");
        assert_eq!(bsdinfo_start(137, want, 1, 2), None, "more than we asked for");
        assert_eq!(bsdinfo_start(-1, want, 1, 2), None, "an error is not a length");
        // Never wraps: `panic = "abort"` makes an overflow check a crash in a hook.
        assert_eq!(full(u64::MAX, u64::MAX), Some(u64::MAX));
        // And what it produces is a number a record can carry back: `state::digits`
        // parses at most twenty ASCII digits, which is every `u64`.
        assert!(u64::MAX.to_string().len() <= 20);
    }

    /// Three encodings, three keys. A reader meeting a key it does not know skips
    /// it like any unknown field, so another platform's origin is ABSENT here and
    /// never a pid of ours - which is what makes a state directory shared between
    /// two of them safe.
    #[test]
    fn each_start_time_encoding_has_its_own_origin_key() {
        let mine = if cfg!(target_os = "macos") { "r" } else { "p" };
        assert_eq!(ORIGIN_KEY, mine);
        assert_ne!(ORIGIN_KEY, "q", "the Windows FILETIME key");
    }

    /// The reaper's other decision, including the awkward pair: no start time and a
    /// process that is ALIVE. Reading that as "a different process has the pid" is
    /// what would unlink a live session's record on macOS, where a process this user
    /// may not inspect answers exactly that way - and on Linux under `hidepid`.
    #[test]
    fn a_missing_start_time_is_a_different_process_only_for_a_pid_proven_gone() {
        let unasked = || unreachable!("a start time settles it without a second call");
        assert_eq!(same_as_recorded(Some(7), 7, unasked), Some(true));
        assert_eq!(same_as_recorded(Some(8), 7, unasked), Some(false), "the pid was reused");
        assert_eq!(same_as_recorded(None, 7, || Some(false)), Some(false), "gone");
        assert_eq!(same_as_recorded(None, 7, || Some(true)), None, "alive, unreadable");
        assert_eq!(same_as_recorded(None, 7, || None), None, "nothing could be asked");
    }

    /// The `/proc` parse, on the bytes the kernel actually writes. The last two
    /// cases are why it takes bytes: `comm` is copied out unvalidated, so a live
    /// process whose executable name is not UTF-8 used to report NO start time,
    /// which is the "alive, but no start time" pair [`same_as_recorded`] is careful
    /// about - reached on an ordinary unprivileged process, with no `hidepid`.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_start_time_is_the_word_after_the_last_paren_whatever_the_name_holds() {
        let line = |comm: &[u8]| {
            let mut v = b"4242 (".to_vec();
            v.extend_from_slice(comm);
            v.extend_from_slice(b") S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 ");
            v.extend_from_slice(b"907861 19 20\n");
            v
        };
        assert_eq!(start_time_from_stat(&line(b"sleep")), Some(907_861));
        // The name that broke the naive split: it holds both a space and a `)`.
        assert_eq!(start_time_from_stat(&line(b"npm exec chrome)")), Some(907_861));
        // The name that broke `read_to_string`: one byte that is not UTF-8.
        assert_eq!(start_time_from_stat(&line(b"sl\xffeep")), Some(907_861));
        assert_eq!(start_time_from_stat(&line(b"\xff\xfe\xfd")), Some(907_861));
        // And the refusals, unchanged: nothing, no paren, and a field that is not
        // twenty digits of ASCII.
        assert_eq!(start_time_from_stat(b""), None);
        assert_eq!(start_time_from_stat(b"4242 sleep S 1 2 3"), None);
        assert_eq!(start_time_from_stat(&line(b"x")[..30]), None);
    }

    /// `doctor` prints [`NO_STATE_DIR`] when there is nowhere to write, and the
    /// operator's next move is to set the variable it names. The two are one fact.
    #[test]
    fn the_refusal_names_the_state_directory_variable() {
        assert!(NO_STATE_DIR.ends_with(RUNTIME_DIR_VAR), "{NO_STATE_DIR}");
    }

    /// The macOS fd-1 lookup's whole decision, run where this project can run
    /// anything. `proc_pidfdinfo` answers with a BYTE COUNT and a C string inside a
    /// fixed array, so every way either can be wrong is staged here - including the
    /// two the kernel is not supposed to produce, because "not supposed to" is not
    /// a guarantee the caller gets to rely on.
    #[test]
    fn an_fd_path_needs_the_whole_struct_and_ends_at_the_first_nul() {
        const WANT: usize = 1200;
        // `vip_path` as the kernel leaves it: the path, a terminator, and then
        // whatever the array held - here, deliberately, another path.
        let arr = |s: &[u8]| {
            let mut v = s.to_vec();
            v.resize(1024, 0);
            v.splice(600..612, *b"/dev/ttys999");
            v
        };
        let read = |nb: i32, s: &[u8]| fd1_path_from_vnode(nb, WANT, &arr(s));
        assert_eq!(read(1200, b"/dev/ttys004"), Some(PathBuf::from("/dev/ttys004")));
        // A byte count, not a status. BOTH spellings of failure are staged, and on
        // purpose: `libproc`'s wrapper and the call under it do not agree on whether
        // a failure arrives as -1 or as 0, and nothing here can run either to find
        // out. Requiring exactly `want` makes the question moot, and these two lines
        // are what says so. Either way it is the same `None` as a revoked vnode's
        // `ENOENT`, because all three mean no tab.
        assert_eq!(read(-1, b"/dev/ttys004"), None, "an error is not a length");
        assert_eq!(read(0, b"/dev/ttys004"), None, "nothing was filled");
        // A short count would leave the path holding the zeros the buffer was made
        // with, which is an empty path this cannot tell from a real refusal. And a
        // longer one is a struct that is not the one asked for - which is also what
        // a wrong flavour constant would return.
        assert_eq!(read(1199, b"/dev/ttys004"), None, "a short fill");
        assert_eq!(read(1201, b"/dev/ttys004"), None, "not the struct we asked for");
        // No terminator anywhere: refused, never truncated. /dev/ttys004 cut to
        // /dev/ttys00 is another writable character device, so nothing downstream
        // would catch it.
        assert_eq!(fd1_path_from_vnode(1200, WANT, &[b'/'; 1024]), None, "unterminated");
        assert_eq!(fd1_path_from_vnode(1200, WANT, &[0u8; 1024]), None, "empty");
        assert_eq!(fd1_path_from_vnode(1200, WANT, &[]), None, "no path at all");
        // A path is bytes here, and a name that is not UTF-8 is still a name: the
        // bytes come back as they went in, undecoded.
        let odd = read(1200, b"/dev/tty\xff\xfe");
        assert_eq!(odd.as_deref().map(|p| p.as_os_str().as_bytes()), Some(&b"/dev/tty\xff\xfe"[..]));
        // What the shared guard does with the answers: the pty passes the prefix
        // test, the redirected `claude -p > out.txt` does not - which is the whole
        // reason this flavour is safe to ask for. A pipe or socket on fd 1 never
        // reaches here at all; the kernel answers EBADF for a non-vnode.
        assert!(is_tty_path(&read(1200, b"/dev/ttys004").expect("a pty path")));
        assert!(is_tty_path(&read(1200, b"/dev/tty").expect("a tty path")));
        assert!(!is_tty_path(&read(1200, b"/Users/me/out.txt").expect("a file path")));
        assert!(!is_tty_path(&read(1200, b"/dev/null").expect("a device path")));
    }

    /// `$CLAUDE_PID` as a number, which only the macOS body needs - `/proc` takes
    /// the bytes and lets `read_link` refuse them. Ten digits is `u32`'s width; the
    /// values that are not a `pid_t` are [`pid_to_ask_about`]'s business, and 0 is
    /// rejected there, so this accepts it and the caller does not.
    #[test]
    fn a_claude_pid_is_decimal_digits_and_nothing_else() {
        assert_eq!(parse_pid(OsStr::new("4242")), Some(4242));
        assert_eq!(parse_pid(OsStr::new("0")), Some(0), "pid_to_ask_about refuses it");
        assert_eq!(parse_pid(OsStr::new("")), None);
        assert_eq!(parse_pid(OsStr::new(" 42")), None);
        assert_eq!(parse_pid(OsStr::new("42\n")), None);
        assert_eq!(parse_pid(OsStr::new("-42")), None);
        assert_eq!(parse_pid(OsStr::new("4294967295")), Some(u32::MAX));
        assert_eq!(parse_pid(OsStr::new("4294967296")), None, "past a u32");
        assert_eq!(parse_pid(OsStr::new("00000000004")), None, "eleven digits");
        // A LEADING ZERO IS NOT A PID HERE, and the reason is the other kernel:
        // `/proc/007` does not exist, so Linux answers `None` for these and macOS
        // would otherwise have resolved pid 7's fd 1. The two Unixes give one
        // answer for one string, which is the whole premise of this file.
        assert_eq!(parse_pid(OsStr::new("007")), None, "Linux has no /proc/007");
        assert_eq!(parse_pid(OsStr::new("0042")), None);
        assert_eq!(parse_pid(OsStr::new("00")), None, "not even zero twice");
        // Not a path, not a number, and never pasted into a system call.
        assert_eq!(parse_pid(OsStr::new("../../etc/passwd")), None);
        assert_eq!(parse_pid(os_str_from_bytes(b"4\xff2").as_ref()), None);
        // And what the two of them together let through is exactly a pid.
        let ask = |s: &str| parse_pid(OsStr::new(s)).and_then(pid_to_ask_about);
        assert_eq!(ask("4242"), Some(4242));
        assert_eq!(ask("0"), None);
        assert_eq!(ask("4294967295"), None);
    }

    /// The leading-zero rule is only worth anything if Linux really does refuse
    /// what it is imitating, so ask Linux rather than assert what it would say.
    /// `/proc/<pid>` is canonical decimal: this process's own pid resolves and the
    /// same number with a zero in front does not, which is exactly the pair
    /// [`parse_pid`] now answers the same way on both kernels.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn proc_spells_a_pid_canonically_which_is_why_a_leading_zero_is_refused() {
        let me = std::process::id();
        assert!(fd1_path(&OsString::from(me.to_string())).is_some(), "own fd 1");
        assert_eq!(fd1_path(&OsString::from(format!("0{me}"))), None, "/proc/0{me}");
        assert_eq!(parse_pid(OsStr::new(&format!("0{me}"))), None, "and so does this");
    }

    /// The headless guard over a REAL fd 1, on the Unix that can stage one: this
    /// process's own. Under `cargo test` fd 1 is a captured pipe, which `/proc`
    /// spells `pipe:[...]` - not a path at all, and the same shape a redirected
    /// `claude -p > out.txt` produces. Whatever it is, the guard paints only a pty.
    ///
    /// The other half - fd 1 IS a pty and the guard opens it - is what every tmux
    /// case in `tests/run.sh` exercises against real terminals, which is where it
    /// belongs; a unit test cannot count on having one.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_guard_refuses_a_real_fd_1_that_is_not_a_terminal() {
        let me = OsString::from(std::process::id().to_string());
        let target = fd1_path(&me).expect("/proc names this process's own fd 1");
        if !is_tty_path(&target) {
            assert!(session_tty(&me).is_none(), "fd 1 is {}", target.display());
        }
        // And a name that is not a pid resolves to nothing without ever being
        // parsed - `read_link` refuses it, which is the whole of the Linux route's
        // validation and is what the oracle does too.
        assert_eq!(fd1_path(OsStr::new("not-a-pid")), None);
        assert_eq!(fd1_path(OsStr::new("")), None);
        assert_eq!(fd1_path(OsStr::new("0")), None, "a pid nothing has");
    }
}
