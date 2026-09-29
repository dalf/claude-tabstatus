//! The Unix backend of [`crate::sys`]. One body for every Unix, except for the
//! handful of questions whose ANSWER differs: a process's start time and its
//! liveness, the session's pty, the record's origin key, and the variable naming
//! the state directory. Those are `cfg`-selected in place - Linux reads `/proc`,
//! macOS calls `libc` - and everything else on this page is shared, which is why
//! the divergent items sit next to each other rather than in a file of their own.
//!
//! ONLY THE SYSTEM CALL IS `cfg`-SELECTED. Every decision either side makes - the
//! errno-to-liveness mapping, the packing of a start time into the one number a
//! record stores - is a pure function compiled on both and exercised by the Linux
//! test run, because there is no Mac in this project's CI to run a macOS branch on
//! and an untested branch is how a wrong answer ships. The `cfg` bodies are a call
//! and a `?`; they contain no test of their own.

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

/// [`session_tty`] can resolve a pty - through `/proc`, so on Linux.
#[cfg(not(target_os = "macos"))]
pub const HAS_SESSION_TTY: bool = true;

/// [`session_tty`] never resolves here: fd 1 of another process is not a path
/// macOS will hand over through anything `libc` declares - see there. The constant
/// and the function have to agree, because `doctor` reads the constant to say
/// whether `session-start` and `session-end` have a route at all, and a `true`
/// would promise a pty nothing can open.
#[cfg(target_os = "macos")]
pub const HAS_SESSION_TTY: bool = false;

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
/// hand-written `#[repr(C)]` layout - which matters because no machine in this
/// project can LINK a macOS binary, only type-check one, and a guessed layout
/// would be memory corruption that no test here could catch.
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
/// terminal - which covers a redirected `claude -p` and every platform with no
/// /proc. `None` is therefore both "no tab to paint" and "fd 1 would not resolve",
/// deliberately the same answer: painting on a guess is the one outcome that
/// retitles somebody else's terminal.
#[cfg(not(target_os = "macos"))]
pub fn session_tty(claude_pid: &OsStr) -> Option<File> {
    let mut link = OsString::from("/proc/");
    link.push(claude_pid);
    link.push("/fd/1");
    let target = fs::read_link(Path::new(&link)).ok()?;
    if !is_tty_path(&target) || !is_char_device(&target.metadata().ok()?.file_type()) {
        return None;
    }
    // Asking whether it is writable and opening it are the same question; ask it
    // once.
    OpenOptions::new().write(true).open(&target).ok()
}

/// DELIBERATELY UNANSWERED on macOS, and the honest `None` the guard above already
/// defines: "fd 1 would not resolve", which paints nothing rather than guessing at
/// a terminal.
///
/// What it would take: `proc_pidfdinfo(pid, 1, PROC_PIDFDVNODEPATHINFO, ...)` into
/// a `vnode_fdinfowithpath`, whose `vip_path` is the path fd 1 names. `libc` 0.2.189
/// declares `proc_pidfdinfo` and `proc_fdinfo` but NEITHER `PROC_PIDFDVNODEPATHINFO`
/// nor `struct vnode_fdinfowithpath` - measured, not assumed - so both would have to
/// be hand-written `#[repr(C)]`, and no machine in this project can link a macOS
/// binary to check a layout that a wrong guess turns into memory corruption on
/// somebody's Mac. It is not guessed here.
///
/// `proc_bsdinfo.e_tdev`, which IS declared, is the CONTROLLING TERMINAL and not
/// this: a redirected `claude -p > file` keeps its controlling terminal, so taking
/// that route would paint a terminal that is not this session's output - exactly the
/// substitution the headless guard exists to refuse.
///
/// The consequence is bounded, because it is only these two edges: the hot
/// working/waiting/idle paint travels the hook protocol's `terminalSequence` and
/// needs no pty at all. `session-start`'s arming and `session-end`'s clearing are
/// what is missing, and [`HAS_SESSION_TTY`] is `false` so that `doctor` says so.
#[cfg(target_os = "macos")]
pub fn session_tty(_claude_pid: &OsStr) -> Option<File> {
    None
}

/// The console route to the session's title, which Unix does not have: `Ok(false)`,
/// "not painted", with no system call. Nothing calls it here - see
/// [`HAS_SESSION_CONSOLE`] - and it exists so both backends offer the same API.
pub fn set_session_title(_claude_pid: &OsStr, _title: &str) -> io::Result<bool> {
    Ok(false)
}

/// Write to a pty NAMED BY tmux - an attached client's terminal - under the same
/// guard [`session_tty`] applies to fd 1: under /dev/pts or /dev/tty, a character
/// device, and writable. A failure is nothing to report: the client may have
/// detached between the listing and the write.
pub fn write_tty(path: &Path, bytes: &[u8]) {
    if !is_tty_path(path) {
        return;
    }
    if !path.metadata().is_ok_and(|m| is_char_device(&m.file_type())) {
        return;
    }
    if let Ok(mut f) = OpenOptions::new().write(true).open(path) {
        let _ = f.write_all(bytes);
    }
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

    /// The reaper's liveness question, decided here so that the macOS body is a
    /// call and not a branch - and run on Linux, which is the only place this
    /// project can run anything.
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
}
