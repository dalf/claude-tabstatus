//! The Unix backend of [`crate::sys`]. Linux-specific where it reads `/proc`;
//! elsewhere those functions find nothing and answer "unknown".

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

/// The file the kernel publishes its host name in, read without a fork.
pub fn kernel_hostname_file() -> Option<&'static Path> {
    Some(Path::new("/proc/sys/kernel/hostname"))
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
pub const ORIGIN_KEY: &str = "p";

/// The variable naming the per-user directory the state directory defaults under.
pub const RUNTIME_DIR_VAR: &str = "XDG_RUNTIME_DIR";

/// Why there is no state directory, when neither variable is set.
pub const NO_STATE_DIR: &str = "no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR";

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
/// boot. `None` when the process is gone - or on a Unix with no `/proc`.
///
/// Parsed after the LAST `") "`, never by splitting the whole line on spaces.
/// Field 2 is the executable name in parentheses and may itself contain both -
/// measured on this machine, `/proc/1259713/stat` holds `(npm exec chrome...)`, so
/// the naive split reads the wrong field for exactly the processes a `claude`
/// session spawns. The tail begins at field 3, so field 22 is its 20th word.
pub fn process_start_time(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    let w = raw.rsplit_once(") ")?.1.split(' ').nth(19)?;
    if w.is_empty() || w.len() > 20 || !w.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    w.parse().ok()
}

/// Whether `pid` names a running process: its `/proc` entry exists. On a Unix
/// with no `/proc` this answers `Some(false)`, as the check always has there.
pub fn process_alive(pid: u32) -> Option<bool> {
    Some(Path::new(&format!("/proc/{}", pid)).exists())
}

/// Whether the process that recorded `(pid, start)` is still that process:
/// `Some(true)` it is, `Some(false)` provably not - gone, or the pid given to
/// another - and `None` when that cannot be told. The same two `/proc` reads, in
/// the same order, that the reaper has always made.
pub fn same_process(pid: u32, start: u64) -> Option<bool> {
    if process_start_time(pid) == Some(start) {
        Some(true)
    } else {
        process_alive(pid).map(|_| false)
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
