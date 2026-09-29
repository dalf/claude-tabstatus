//! The Windows backend of [`crate::sys`].
//!
//! Every answer here is either the Windows equivalent or an honest "unknown" that
//! routes the caller onto its existing conservative path. Nothing pretends: a
//! question with no answer here says so in its type or in a `HAS_*` constant, and
//! a missing terminal paints nothing rather than guessing at one.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileTypeExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, ERROR_ACCESS_DENIED,
    ERROR_INVALID_PARAMETER, ERROR_SHARING_VIOLATION, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    STILL_ACTIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, FileRenameInfoEx, FindClose, FindFirstFileW, GetDriveTypeW,
    GetFileInformationByHandleEx, GetFileType, GetVolumeInformationByHandleW, LockFileEx,
    MoveFileExW, SetFileInformationByHandle, DELETE, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_TYPE_CHAR, LOCKFILE_EXCLUSIVE_LOCK, WIN32_FIND_DATAW,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, FreeConsole, GetConsoleScreenBufferInfo, SetConsoleCtrlHandler,
    SetConsoleTitleW, CONSOLE_SCREEN_BUFFER_INFO,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, IsWow64Process, OpenProcess,
    PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows_sys::Win32::System::IO::{CancelSynchronousIo, DeviceIoControl, OVERLAPPED};

use super::FileId;

/// [`lock_exclusive`] locks, and [`file_id_of`] and [`file_id_at`] can prove the
/// lock is on the file a path names: a `LockFileEx` byte, and `FILE_ID_INFO` read
/// from a handle. ([`file_id`], from `Metadata` alone, still cannot.)
pub const HAS_RECORD_LOCK: bool = true;

/// There are no mode bits: [`mode`] never answers and [`set_mode`] applies
/// nothing, so a report must not print a mode it did not read.
pub const HAS_MODES: bool = false;

/// [`session_tty`] never resolves: there is no pty. The session's tab is a console
/// here, reached by [`set_session_title`].
pub const HAS_SESSION_TTY: bool = false;

/// [`set_session_title`] can title the console Claude Code runs in.
pub const HAS_SESSION_CONSOLE: bool = true;

/// Bytes back into an `OsStr` - the inverse of [`OsStr::as_encoded_bytes`].
///
/// An `OsStr` here is WTF-8 and std offers no safe way back from arbitrary bytes,
/// so valid UTF-8 borrows and anything else is converted lossily, with U+FFFD for
/// each invalid sequence. What that costs depends on where the bytes came from:
/// bytes sliced out of an `OsStr` at ASCII boundaries are lossy only when the
/// path holds an unpaired surrogate, but bytes read from a FILE (a `.git` gitfile,
/// a marker's JSON) are lossy for any invalid UTF-8. Either way the result
/// displays rather than failing. Nothing relies on the round trip for safety: the
/// marker's paths were written through `json::quote`, so are UTF-8 already, and
/// `tree::in_tree` confines whatever they spell to the tree.
pub fn os_str_from_bytes(b: &[u8]) -> Cow<'_, OsStr> {
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(OsStr::new(s)),
        Err(_) => Cow::Owned(OsString::from(String::from_utf8_lossy(b).into_owned())),
    }
}

/// The owned form of [`os_str_from_bytes`], without a copy when it is UTF-8.
pub fn os_string_from_vec(v: Vec<u8>) -> OsString {
    match String::from_utf8(v) {
        Ok(s) => OsString::from(s),
        Err(e) => OsString::from(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Where the home directory is when `HOME` is unset, which native Windows shells
/// leave it: `%USERPROFILE%`, when set and non-empty.
pub fn home_fallback() -> Option<OsString> {
    std::env::var_os("USERPROFILE").filter(|v| !v.is_empty())
}

/// Whether `b` belongs to a line ending in a command's output: `hostname.exe` and
/// its kin end lines with CRLF, so `\r` does as well as `\n`.
pub fn is_line_end(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// No kernel file publishes the host name. A rooted path like `/proc/...` would
/// resolve against the current drive, where any local user may create it.
pub fn kernel_hostname_file() -> Option<&'static Path> {
    None
}

/// File identity from `Metadata` alone. std exposes the volume serial and file
/// index only behind an unstable feature, so this answers `None`; callers treat
/// that as "cannot prove same file". With a handle, [`file_id_of`] answers.
pub fn file_id(_m: &Metadata) -> Option<FileId> {
    None
}

/// The identity of the file an open handle names: `FILE_ID_INFO`, the volume serial
/// and the 128-bit file id. Asking needs no data access, so a byte-range lock -
/// anyone's - does not stand in the way.
pub fn file_id_of(f: &File) -> Option<FileId> {
    // SAFETY: FILE_ID_INFO is plain data, valid all-zero; the pointer and size
    // describe it exactly, and the call writes nothing else and keeps nothing.
    let mut info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            f.as_raw_handle() as HANDLE,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    (ok != 0).then(|| (info.VolumeSerialNumber, u128::from_le_bytes(info.FileId.Identifier)))
}

/// The identity of the entry `path` names now, a link as itself: a handle opened
/// for attributes only, not following a reparse point, sharing everything. A link
/// or junction therefore answers its OWN identity, never its target's, which is
/// what makes a comparison with a handle opened through it fail closed.
pub fn file_id_at(path: &Path, _lstat: &Metadata) -> Option<FileId> {
    let f = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    file_id_of(&f)
}

/// The one byte the record lock covers: far past any record's data (8 KiB).
///
/// `LockFileEx` is MANDATORY: every other handle - this process's own included -
/// gets `ERROR_LOCK_VIOLATION` reading or writing a byte it covers. std's
/// `File::lock` covers them all, so an unlocked reader (the reaper, `doctor`) and
/// even the holder's own read of its record by path would be refused, and a refused
/// read is an absent record. Nothing reads or writes this byte, so the lock excludes
/// other lockers and nobody else - an advisory lock in effect, like `flock`. SQLite's
/// Windows locking takes bytes outside its data for the same reason.
const LOCK_BYTE: u64 = 1 << 62;

/// Block until `f` holds the exclusive record lock: `LockFileEx` on [`LOCK_BYTE`],
/// released when the handle is closed or the process exits (measured: a waiter
/// gets it about 1ms after its holder is killed).
///
/// `f` must be a synchronous handle, which every `File` std opens is: the call
/// then returns only once the lock is held or refused.
pub fn lock_exclusive(f: &File) -> io::Result<()> {
    // SAFETY: OVERLAPPED is plain data, valid all-zero, and here only carries the
    // offset. On a synchronous handle the call completes before it returns, so it
    // keeps no pointer to `at`.
    let mut at: OVERLAPPED = unsafe { std::mem::zeroed() };
    at.Anonymous.Anonymous.Offset = LOCK_BYTE as u32;
    at.Anonymous.Anonymous.OffsetHigh = (LOCK_BYTE >> 32) as u32;
    let ok = unsafe {
        LockFileEx(f.as_raw_handle() as HANDLE, LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &mut at)
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `FILE_RENAME_FLAG_REPLACE_IF_EXISTS` and `FILE_RENAME_FLAG_POSIX_SEMANTICS`
/// (winbase.h), and `FILE_SUPPORTS_POSIX_UNLINK_RENAME` (winnt.h): spelled here
/// rather than pulled in through two more windows-sys feature modules.
const FILE_RENAME_FLAG_REPLACE_IF_EXISTS: u32 = 0x1;
const FILE_RENAME_FLAG_POSIX_SEMANTICS: u32 = 0x2;
const FILE_SUPPORTS_POSIX_UNLINK_RENAME: u32 = 0x400;

/// Replace `to` with `from` in one step, while `to` is held open - and locked - by
/// its writer and possibly by others: a POSIX-semantics rename.
///
/// That is the only kind that can. `MoveFileExW` refuses (os error 5) whenever
/// `to` is open at all, and the writer always holds it; a POSIX rename moves the
/// name at once and leaves every open handle on the old, now nameless file. std's
/// `rename` reaches the same call only as a fallback after that doomed
/// `MoveFileExW` (measured 4.2ms against 2.0ms), and reports the first error when
/// the fallback fails too - so it is called directly here.
///
/// What still refuses it is a handle opened WITHOUT `FILE_SHARE_DELETE` on either
/// file - a scanner, an indexer, an editor - which the call reports as a sharing
/// violation. That is retried for up to half a second, all of it under the
/// caller's lock; anything else is an answer and returns at once.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    let to = rename_target(to)?;
    let mut tries = 0;
    loop {
        match rename_posix(from, &to) {
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) && tries < 25 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            res => return res,
        }
    }
}

/// `to` spelled the way the rename call must be given it: absolute, and verbatim.
///
/// ABSOLUTE, because a relative name is resolved against the process's current
/// directory, not against `from`'s (measured: the temp file moved into the cwd, the
/// call returned success, and the record was untouched). VERBATIM (`\\?\`), because
/// the name is converted to an NT path with the MAX_PATH cap that std escapes by
/// the same prefix: a plain record path of 260 characters or more fails every write
/// with os error 206 while every other operation on it succeeds (measured: 284
/// characters fail plain and succeed verbatim). `absolute` has already folded `.`,
/// `..` and `/`, which a verbatim path would take literally, so the prefix changes
/// nothing else. A path that is verbatim already is used as it is.
fn rename_target(to: &Path) -> io::Result<Vec<u16>> {
    let abs = std::path::absolute(to)?;
    let w = wide(&abs);
    let kind = match abs.components().next() {
        Some(Component::Prefix(p)) => Some(p.kind()),
        _ => None,
    };
    let (lead, rest): (&str, &[u16]) = match kind {
        Some(Prefix::Disk(_)) => (r"\\?\", &w),
        // `\\server\share\x` is `\\?\UNC\server\share\x`: one leading `\` goes.
        Some(Prefix::UNC(..)) => (r"\\?\UNC", w.get(1..).unwrap_or_default()),
        // `\\?\...` and `\\.\...` already bypass the conversion.
        _ => ("", &w),
    };
    let mut out: Vec<u16> = lead.encode_utf16().collect();
    out.extend_from_slice(rest);
    Ok(out)
}

/// `SetFileInformationByHandle(FileRenameInfoEx)`, replacing and POSIX, on a handle
/// to `from` opened for DELETE only. `name` is [`rename_target`]'s spelling.
fn rename_posix(from: &Path, name: &[u16]) -> io::Result<()> {
    let src = OpenOptions::new()
        .access_mode(DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(from)?;
    let at = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    // The header, the name, and its NUL - the struct is variable-length.
    let size = at + (name.len() + 1) * 2;
    let len = u32::try_from(name.len() * 2)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `buf` is zeroed, 8-aligned (FILE_RENAME_INFO's alignment) and at least
    // `size` bytes, so the header fields are in bounds and the name's `name.len()`
    // units plus the zeroed NUL fit after `FileName`. The pointer derives from the
    // whole buffer, and `buf` outlives the call, which reads `size` bytes of it and
    // keeps nothing; `RootDirectory` stays null, so `name` is taken as a full path.
    let ok = unsafe {
        (*info).Anonymous.Flags = FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS;
        (*info).FileNameLength = len;
        let dst = std::ptr::addr_of_mut!((*info).FileName).cast::<u16>();
        std::ptr::copy_nonoverlapping(name.as_ptr(), dst, name.len());
        SetFileInformationByHandle(src.as_raw_handle() as HANDLE, FileRenameInfoEx, info.cast(), size as u32)
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Whether [`replace_file`] can replace a file in `dir` that its writer holds open
/// and locked: the volume says it has POSIX rename semantics. NTFS does, since
/// Windows 10 1709 (measured: `C:`); FAT, exFAT, the 9P share WSL exports
/// (measured) and some SMB servers do not - and there every write would fail,
/// because the writer itself holds the record open, so the layer stays off.
pub fn replaces_open_files(dir: &Path) -> bool {
    let Ok(d) = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
    else {
        return false;
    };
    let mut flags = 0u32;
    // SAFETY: `d` is an open handle. Every buffer is null with a zero size except
    // `flags`, a live u32; the call keeps no pointer.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            d.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    ok != 0 && flags & FILE_SUPPORTS_POSIX_UNLINK_RENAME != 0
}

/// Whether a word of the session-id grammar names a DEVICE rather than a file in a
/// directory: the DOS device names, any case. `<dir>\NUL` opens the null device
/// (measured), and Windows 10 treats the others the same way.
pub fn reserved_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    matches!(n.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (n.len() == 4
            && (n.starts_with("COM") || n.starts_with("LPT"))
            && n.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
}

/// The key a record's origin is written under: not Unix's `p`, because the
/// numbers mean something else here - a Windows pid and creation time read by a
/// Linux build sharing `CCTAB_STATE_DIR` (WSL) would be "gone" and reaped while
/// live, and the reverse here. Each side reads the other's key as an unknown
/// field: no origin, the mtime rule.
pub const ORIGIN_KEY: &str = "q";

/// The variable naming the per-user directory the state directory defaults under:
/// `%LOCALAPPDATA%` - private to the user, local rather than roaming, and not
/// emptied by Storage Sense the way `%TEMP%` is.
pub const RUNTIME_DIR_VAR: &str = "LOCALAPPDATA";

/// Why there is no state directory, when neither variable is set.
pub const NO_STATE_DIR: &str = "no CCTAB_STATE_DIR and no LOCALAPPDATA";

/// Windows has no mode bits; the read-only attribute is not one.
pub fn mode(_m: &Metadata) -> Option<u32> {
    None
}

/// Nothing to set: there is no mode to apply. The file keeps whatever ACL it has -
/// for a file this crate just created, the one inherited from its directory,
/// whether or not that directory is private. `Ok` means only "nothing failed".
pub fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// The mode is ignored for the same reason as [`set_mode`]: the created file
/// takes its directory's inherited ACL.
pub fn with_mode(opts: &mut OpenOptions, _mode: u32) -> &mut OpenOptions {
    opts
}

/// Unknown for a regular file: executability is the extension's business on
/// Windows, not a bit, and `CreateProcess` decides when it is asked. Anything
/// that is not a regular file certainly is not an executable.
pub fn is_executable(m: &Metadata) -> Option<bool> {
    if m.is_file() {
        None
    } else {
        Some(false)
    }
}

/// `create_dir_all`, with nothing to make it private: the directories take the
/// ACL inherited from where they are created. Under the user's profile that is
/// private; a `CCTAB_STATE_DIR` pointed elsewhere gets whatever that place grants.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// What [`link_dir`] makes, in the word a report line uses for it.
pub const DIR_LINK: &str = "junction";

/// A running program's file cannot be deleted: the loader's image section holds it.
/// It CAN be renamed, which is what [`replace_running`] relies on.
pub const HAS_UNLINK_RUNNING: bool = false;

// The numbers below are the SDK's, from winioctl.h, winnt.h, fileapi.h and
// winbase.h; they are spelled here rather than pulled in through more windows-sys
// feature modules, and the junction tests prove them against the real kernel.
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;
/// `GetDriveTypeW`'s answer for a drive letter mapped to a network share (winbase.h).
const DRIVE_REMOTE: u32 = 4;
/// The tag, the data length, the reserved word, and the four offset/length words.
const MOUNT_POINT_HEADER: usize = 16;

/// `REPARSE_DATA_BUFFER` in its `MountPointReparseBuffer` shape - the one a
/// junction is - sized to the largest buffer the kernel accepts.
#[repr(C)]
struct MountPointReparseBuffer {
    reparse_tag: u32,
    /// Bytes after the first eight: the four words below plus the path buffer.
    reparse_data_length: u16,
    reserved: u16,
    substitute_name_offset: u16,
    substitute_name_length: u16,
    print_name_offset: u16,
    print_name_length: u16,
    path_buffer: [u16; (MAXIMUM_REPARSE_DATA_BUFFER_SIZE - MOUNT_POINT_HEADER) / 2],
}

/// A directory link at `link` naming `target`: a JUNCTION.
///
/// Not a symlink, because `CreateSymbolicLinkW` for a directory needs Developer Mode
/// or elevation and fails with os error 1314 without them - and a junction does the
/// same job for a local directory with no privilege at all. std reads one back as a
/// symlink (`symlink_metadata`, `read_link`), so nothing else here needs to know.
///
/// The target is stored absolute, because a junction cannot be relative: a relative
/// `target` is taken against `link`'s directory, which is what a symlink would have
/// meant, and `\\?\` is stripped. It must be on a local drive; a junction cannot
/// name a network share - by UNC path or through a drive letter mapped to one - and
/// asking for one is refused before anything is created.
///
/// A junction is two steps - a directory made, then a reparse point set on it - and a
/// kill between them would leave a REAL empty directory at `link`, which every later
/// install and uninstall refuses as somebody else's. So both steps happen at a scratch
/// name beside `link`, `.<name>.cctab-new.<pid>`, and the finished junction is renamed
/// onto `link` - a rename that never replaces, so `link` goes from absent to a whole
/// junction in one step or not at all. A failure removes the scratch junction; a kill
/// leaves it for [`sweep_replaced`].
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    let target = junction_target(target, link)?;
    let (buf, len) = mount_point(&wide(&target))?;
    let scratch = beside(link, "new");
    // A leftover under this very name (a reused pid): a junction or an empty
    // directory goes, anything with contents makes `create_dir` fail below.
    let _ = fs::remove_dir(&scratch);
    make_junction(&scratch, &buf, len)?;
    if let Err(e) = rename_no_replace(&scratch, link) {
        let _ = fs::remove_dir(&scratch);
        return Err(e);
    }
    Ok(())
}

/// Make the directory `at` and set `buf` on it; on failure remove it again.
fn make_junction(at: &Path, buf: &MountPointReparseBuffer, len: usize) -> io::Result<()> {
    fs::create_dir(at)?;
    let res = set_reparse_point(at, buf, len);
    if res.is_err() {
        let _ = fs::remove_dir(at);
    }
    res
}

/// `MoveFileExW` with no flags: renames `from` to `to` on the same volume and fails
/// if `to` exists, where std's `rename` would replace it.
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (wide_nul(from), wide_nul(to));
    // SAFETY: both pointers are NUL-terminated UTF-16 buffers that outlive the call,
    // which reads them and keeps neither.
    if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().collect()
}

fn wide_nul(p: &Path) -> Vec<u16> {
    let mut v = wide(p);
    v.push(0);
    v
}

/// `\\?\C:\x` as `C:\x`, and `\\?\UNC\srv\share\x` as `\\srv\share\x`; any other
/// path unchanged. A verbatim path is exempt from `..` folding and names a directory
/// the plain spelling also names, so nothing here keeps the prefix.
fn strip_verbatim(p: &Path) -> PathBuf {
    let w = wide(p);
    match p.components().next() {
        Some(Component::Prefix(q)) => match q.kind() {
            Prefix::VerbatimDisk(_) => PathBuf::from(OsString::from_wide(&w[4..])),
            Prefix::VerbatimUNC(..) => {
                let mut v: Vec<u16> = r"\\".encode_utf16().collect();
                v.extend_from_slice(&w[8..]);
                PathBuf::from(OsString::from_wide(&v))
            }
            _ => p.to_path_buf(),
        },
        _ => p.to_path_buf(),
    }
}

/// `target` as the drive-absolute path a junction stores.
fn junction_target(target: &Path, link: &Path) -> io::Result<PathBuf> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    // `\\?\C:\x` becomes `C:\x` BEFORE normalising: a verbatim path is exempt from
    // `..` folding, and the kernel does not fold it inside a reparse point either.
    let abs = std::path::absolute(strip_verbatim(&joined))?;
    let local = match abs.components().next() {
        // A junction to a drive letter mapped to a share is created without complaint -
        // NTFS does not look at the name - and then never resolves.
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) => drive_type(d) != DRIVE_REMOTE,
            _ => false,
        },
        _ => false,
    };
    if local {
        Ok(abs)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a junction can only name a directory on a local drive, and {} is not one",
                abs.display()
            ),
        ))
    }
}

/// What kind of drive the letter `d` is.
fn drive_type(d: u8) -> u32 {
    let root: Vec<u16> = format!("{}:\\", d as char).encode_utf16().chain([0]).collect();
    // SAFETY: `root` is a NUL-terminated UTF-16 buffer that outlives the call.
    unsafe { GetDriveTypeW(root.as_ptr()) }
}

/// The reparse buffer for a junction to `target`, and how many bytes of it count.
/// The substitute name is the NT path `\??\<target>`, which is what the kernel
/// follows; the print name is `<target>` itself, which is what tools show.
fn mount_point(target: &[u16]) -> io::Result<(Box<MountPointReparseBuffer>, usize)> {
    let mut subst: Vec<u16> = r"\??\".encode_utf16().collect();
    subst.extend_from_slice(target);
    let units = subst.len() + 1 + target.len() + 1;
    let mut buf = Box::new(MountPointReparseBuffer {
        reparse_tag: IO_REPARSE_TAG_MOUNT_POINT,
        reparse_data_length: 0,
        reserved: 0,
        substitute_name_offset: 0,
        substitute_name_length: 0,
        print_name_offset: 0,
        print_name_length: 0,
        path_buffer: [0; (MAXIMUM_REPARSE_DATA_BUFFER_SIZE - MOUNT_POINT_HEADER) / 2],
    });
    if units > buf.path_buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path is too long for a junction to name",
        ));
    }
    // Both names NUL-terminated, the print name straight after the substitute name's
    // NUL. Lengths are in BYTES and exclude the NUL. Every value below fits in a u16:
    // `units` is bounded by the buffer, which is 8184 units.
    buf.path_buffer[..subst.len()].copy_from_slice(&subst);
    let print_at = subst.len() + 1;
    buf.path_buffer[print_at..print_at + target.len()].copy_from_slice(target);
    buf.substitute_name_length = (subst.len() * 2) as u16;
    buf.print_name_offset = (print_at * 2) as u16;
    buf.print_name_length = (target.len() * 2) as u16;
    buf.reparse_data_length = (8 + units * 2) as u16;
    Ok((buf, MOUNT_POINT_HEADER + units * 2))
}

/// Make the empty directory `dir` a junction by setting `buf` on it.
///
/// The handle is opened through std, which is `CreateFileW`: write access, and
/// `FILE_FLAG_BACKUP_SEMANTICS` because it is a directory and
/// `FILE_FLAG_OPEN_REPARSE_POINT` so the handle is the directory itself. Only the
/// `DeviceIoControl` call has no safe wrapper.
fn set_reparse_point(dir: &Path, buf: &MountPointReparseBuffer, len: usize) -> io::Result<()> {
    let handle = OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?;
    let mut returned = 0u32;
    // SAFETY: `handle` is an open file handle that outlives the call. The input
    // pointer and `len` describe `buf`, a live, initialised `#[repr(C)]` value at
    // least `len` bytes long (`mount_point` bounds `len` by its size), which the
    // kernel only reads. There is no output buffer and no OVERLAPPED, so the call is
    // synchronous and keeps no pointer past its return; `returned` is a live u32.
    let ok = unsafe {
        DeviceIoControl(
            handle.as_raw_handle() as HANDLE,
            FSCTL_SET_REPARSE_POINT,
            (buf as *const MountPointReparseBuffer).cast(),
            len as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Whether [`link_dir`] could make a link to `target` in `dir` THAT RESOLVES, asked
/// before the first write by actually making one - `.cctab-ltest.<pid>` - following
/// it, and removing it. A refusal here (a filesystem with no reparse points, a target
/// on a network share) then lands in the preflight rather than after settings.json
/// has been edited.
///
/// Creating a junction proves little on its own: NTFS stores whatever name it is
/// given. So the probe names the deepest part of `target` that exists already -
/// `target` itself is usually not written yet - and must be traversable, which is the
/// half a junction to somewhere the kernel will not follow fails.
pub fn probe_dir_link(target: &Path, dir: &Path) -> io::Result<()> {
    let target = junction_target(target, dir)?;
    let reach = target.ancestors().find(|a| a.is_dir()).unwrap_or(&target).to_path_buf();
    let (buf, len) = mount_point(&wide(&reach))?;
    let probe = dir.join(format!(".cctab-ltest.{}", std::process::id()));
    let _ = fs::remove_dir(&probe);
    make_junction(&probe, &buf, len)?;
    let followed = fs::metadata(&probe);
    let removed = fs::remove_dir(&probe).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("the probe {} was made and could not be removed: {}", probe.display(), e),
        )
    });
    if let Err(e) = followed {
        return Err(io::Error::new(
            e.kind(),
            format!("a junction to {} is made but does not resolve: {}", reach.display(), e),
        ));
    }
    removed
}

/// Remove a link made by [`link_dir`] - the link, never what it names.
///
/// `RemoveDirectoryW` (std's `remove_dir`) on a junction or a directory symlink
/// removes the reparse point and leaves the target and its contents alone; the
/// `DeleteFileW` that `remove_file` is refuses a directory link outright. Anything
/// that is not a link is refused here rather than removed: `remove_dir` would take
/// an empty real directory, and this is only ever meant for a link.
pub fn remove_dir_link(link: &Path) -> io::Result<()> {
    let ft = fs::symlink_metadata(link)?.file_type();
    if ft.is_symlink_dir() {
        fs::remove_dir(link)
    } else if ft.is_symlink_file() {
        fs::remove_file(link)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a link", link.display()),
        ))
    }
}

/// Move the directory link `from` onto `to`, replacing the link there.
///
/// std's `rename` first. `MoveFileExW` refuses to replace a directory, and a
/// junction is one, so std retries with `FileRenameInfoEx` and POSIX semantics -
/// and NTFS on Windows 10 1709 and later does replace an (empty, reparse-point)
/// directory that way, in one call. That is the ordinary path, measured here, and
/// it is atomic like `rename(2)`: `to` never stops naming a target.
///
/// Only where that is refused as well - an older Windows, a filesystem without POSIX
/// rename - does [`swap_aside`] run, and that one is NOT atomic.
pub fn replace_dir_link(from: &Path, to: &Path) -> io::Result<()> {
    let first = match fs::rename(from, to) {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    // Only a LINK is ever moved aside: anything else at `to` is not ours to move.
    if !fs::symlink_metadata(to).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(first);
    }
    swap_aside(from, to)
}

/// The fallback repoint, for when no rename can replace the link at `to`:
///
///   1. rename the old link at `to` aside, to `.<name>.cctab-old.<pid>`;
///   2. rename `from` onto `to` - on failure, rename the old link back;
///   3. remove the old link, as a link.
///
/// Between 1 and 2 - two renames in one directory, microseconds - `to` does not
/// exist, and a hook starting in that window finds no plugin binary. Outside it `to`
/// always names the old target or the new one. A failed step 3 leaves a link
/// [`sweep_replaced`] takes on a later run.
fn swap_aside(from: &Path, to: &Path) -> io::Result<()> {
    let aside = beside(to, "old");
    let _ = remove_dir_link(&aside);
    fs::rename(to, &aside)?;
    if let Err(e) = fs::rename(from, to) {
        let _ = fs::rename(&aside, to);
        return Err(e);
    }
    let _ = remove_dir_link(&aside);
    Ok(())
}

/// Move the file `from` onto `to`, which may be an executable something is running.
///
/// A running program's file cannot be replaced or deleted, but it CAN be renamed. So
/// the plain rename is tried first - atomic, and all it takes when nothing is running
/// `to` - and only when that is refused as in use:
///
///   1. rename `to` aside, to `.<name>.cctab-old.<pid>` - allowed while it runs, and
///      every running process carries on from the renamed file;
///   2. rename `from` onto `to` - on failure, rename the old file back;
///   3. try to delete the old file; while something still runs it that fails, and it
///      is left for [`sweep_replaced`] on a later run.
///
/// Between 1 and 2 `to` does not exist, so a hook starting in that window finds no
/// binary. It is two renames in one directory; outside it `to` is always the whole
/// old file or the whole new one.
///
/// The answer is the aside copy when step 3 could not delete it, so the caller can
/// say that it is there and what will remove it.
pub fn replace_running(from: &Path, to: &Path) -> io::Result<Option<PathBuf>> {
    let first = match fs::rename(from, to) {
        Ok(()) => return Ok(None),
        Err(e) => e,
    };
    let in_use = matches!(
        first.raw_os_error().map(|c| c as u32),
        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
    );
    if !in_use || !fs::symlink_metadata(to).is_ok_and(|m| m.is_file()) {
        return Err(first);
    }
    let aside = beside(to, "old");
    // A leftover under this very name (a reused pid) goes first, if it can; if it is
    // itself still running, the rename below fails and nothing has changed.
    let _ = fs::remove_file(&aside);
    patiently(|| fs::rename(to, &aside))?;
    if let Err(e) = patiently(|| fs::rename(from, to)) {
        let _ = patiently(|| fs::rename(&aside, to));
        return Err(e);
    }
    Ok(fs::remove_file(&aside).is_err().then_some(aside))
}

/// `op`, retried for up to about two seconds while it fails with a SHARING violation.
///
/// A rename of a running program's file is allowed; what refuses it for a moment is a
/// scanner - Defender opens a freshly written or freshly executed `.exe` without
/// `FILE_SHARE_DELETE` and lets go a few milliseconds later. Measured here: without
/// the retry, this sequence run straight after a copy-and-exec failed that way (os
/// error 32) in five runs of eight, and in none of twelve with it. Any other error is
/// an answer and returns at once.
fn patiently(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match op() {
            Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) && tries < 40 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            res => return res,
        }
    }
}

/// Remove what [`link_dir`], [`replace_dir_link`], [`replace_running`] and
/// [`probe_dir_link`] left in `dir` - every `.<name>.cctab-old.<pid>`,
/// `.<name>.cctab-new.<pid>` and `.cctab-ltest.<pid>` that can go - and answer what
/// was removed, so the report can name it.
///
/// The pid is not asked about - it names the run that set the file aside, not whoever
/// still uses it - because the OS answers the only question that matters: deleting a
/// file some process is still running fails, so that one is left for the next run.
/// A link is removed as a link, so what it names is never touched, and a real
/// directory only with `remove_dir`, which takes it only when it is empty - the
/// shape a kill between a junction's two steps leaves.
pub fn sweep_replaced(dir: &Path) -> Vec<PathBuf> {
    let mut gone = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return gone };
    for e in rd.flatten() {
        let raw = e.file_name();
        if !raw.to_str().is_some_and(is_retired) {
            continue;
        }
        let p = e.path();
        let removed = match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => remove_dir_link(&p).is_ok(),
            Ok(m) if m.is_file() => fs::remove_file(&p).is_ok(),
            Ok(m) if m.is_dir() => fs::remove_dir(&p).is_ok(),
            _ => false,
        };
        if removed {
            gone.push(p);
        }
    }
    gone.sort();
    gone
}

/// `.<name>.cctab-<tag>.<pid>` beside `p`.
fn beside(p: &Path, tag: &str) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(p.file_name().unwrap_or_default());
    name.push(format!(".cctab-{}.{}", tag, std::process::id()));
    p.with_file_name(name)
}

/// A name [`beside`] made - a dotfile ending `.cctab-old.<digits>` or
/// `.cctab-new.<digits>` - or the probe junction of a [`probe_dir_link`] that was
/// killed before it removed it.
fn is_retired(name: &str) -> bool {
    let digits = |pid: &str| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
    let tagged = |tag: &str| name.rsplit_once(tag).is_some_and(|(_, p)| digits(p));
    name.strip_prefix(".cctab-ltest.").is_some_and(digits)
        || (name.starts_with('.') && (tagged(".cctab-old.") || tagged(".cctab-new.")))
}

/// The spelling of `p` that is compared, printed and recorded.
///
/// NTFS answers to many spellings of one directory - any letter case, an 8.3 short
/// name like `ALEXAN~1`, a `\\?\` prefix - and a path compared by its bytes then says
/// "different directory" about the same one: a tree under `<config>\SKILLS` walks past
/// the refusal that keeps it out of `skills`, and a re-install through `\\?\` reads
/// its own live tree as an orphan to delete. So: `\\?\` stripped, `.` and `..` folded,
/// the drive letter upper-cased, and every component that EXISTS spelled the way the
/// directory lists it - `FindFirstFileW` answers with the stored name, long form and
/// true case, without following a link. What does not exist yet keeps the caller's
/// spelling, and [`same_path`] compares it without regard to case.
pub fn normalize(p: &Path) -> PathBuf {
    let plain = strip_verbatim(p);
    let abs = std::path::absolute(&plain).unwrap_or(plain);
    let mut out = PathBuf::new();
    let mut listed = true;
    for c in abs.components() {
        match c {
            Component::Prefix(q) => match q.kind() {
                Prefix::Disk(d) => out.push(format!("{}:", d.to_ascii_uppercase() as char)),
                _ => out.push(c),
            },
            Component::Normal(n) if listed => match stored_name(&out.join(n)) {
                Some(real) => out.push(real),
                None => {
                    listed = false;
                    out.push(n)
                }
            },
            other => out.push(other),
        }
    }
    out
}

/// The name the directory stores for the last component of `p`, or `None` when it
/// cannot be listed (absent, or no permission to list its parent).
fn stored_name(p: &Path) -> Option<OsString> {
    let name = p.file_name()?;
    // `FindFirstFileW` reads these as wildcards; no real file name holds one, and a
    // pattern must never pick some other entry's spelling.
    if name.encode_wide().any(|u| matches!(u, 0x2A | 0x3F | 0x3C | 0x3E | 0x22)) {
        return None;
    }
    let w = wide_nul(p);
    // SAFETY: WIN32_FIND_DATAW is plain data, valid all-zero; `w` is NUL-terminated
    // and outlives the call, and `data` is a live, writable value of the right type.
    // A handle other than INVALID_HANDLE_VALUE is closed exactly once.
    let data = unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileW(w.as_ptr(), &mut data);
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        FindClose(h);
        data
    };
    let n = data.cFileName.iter().position(|&u| u == 0).unwrap_or(data.cFileName.len());
    (n > 0).then(|| OsString::from_wide(&data.cFileName[..n]))
}

/// A path as NTFS compares it: its components, each upper-cased one character at a
/// time (the simple mapping, as the volume's upcase table does).
fn folded(p: &Path) -> Vec<String> {
    let up = |c: char| {
        let mut u = c.to_uppercase();
        match (u.next(), u.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    };
    p.components().map(|c| c.as_os_str().to_string_lossy().chars().map(up).collect()).collect()
}

/// Whether `a` and `b` name the same directory by their spelling: equal once both
/// are [`normalize`]d and case is set aside. Links are NOT followed - a junction and
/// the directory it names are two different paths, as they are to `install`.
pub fn same_path(a: &Path, b: &Path) -> bool {
    a == b || folded(&normalize(a)) == folded(&normalize(b))
}

/// Whether `p` is `base` or lies beneath it, WHEREVER the two really are: the part of
/// each that exists is resolved through `fs::canonicalize` - links, short names and
/// case included - and the rest is compared without regard to case. It answers the
/// question the refusal asks: would a directory written at `p` land inside `base`?
pub fn is_within(p: &Path, base: &Path) -> bool {
    let (p, base) = (folded(&resolved(p)), folded(&resolved(base)));
    p.len() >= base.len() && p[..base.len()] == base[..]
}

/// `p` with its deepest existing ancestor replaced by where that really is.
fn resolved(p: &Path) -> PathBuf {
    let p = normalize(p);
    let mut tail: Vec<&OsStr> = Vec::new();
    let mut at = p.as_path();
    loop {
        if let Ok(real) = fs::canonicalize(at) {
            let mut out = strip_verbatim(&real);
            out.extend(tail.iter().rev());
            return out;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                at = parent;
            }
            _ => return p.clone(),
        }
    }
}

/// The command a report hands an operator to remove the directory `p` outright: a
/// PowerShell one, the path single-quoted - a profile path with a space in it is the
/// ordinary case here, and `rm -rf` is a parameter error in PowerShell.
pub fn remove_dir_command(p: &Path) -> String {
    format!(
        "Remove-Item -Recurse -Force -LiteralPath '{}'",
        p.display().to_string().replace('\'', "''")
    )
}

/// The creation time of the RUNNING process `pid`, in 100ns units since 1601, or
/// `None` when it has exited or cannot be asked.
///
/// Immutable for the life of the process - a clock stepped later does not move it
/// - and a pid is not handed out again while any handle to its process is open, so
/// `(pid, creation)` names one process, as `(pid, start ticks)` does on Linux.
pub fn process_start_time(pid: u32) -> Option<u64> {
    match probe(pid) {
        Probe::Running(created) => Some(created),
        Probe::Gone | Probe::Unknown => None,
    }
}

/// Whether `pid` names a running process. `Some(false)` only on proof - no such pid,
/// or its process has exited - and `None` for a process this user may not ask about
/// (the System process, another user's), which callers keep.
pub fn process_alive(pid: u32) -> Option<bool> {
    match probe(pid) {
        Probe::Running(_) => Some(true),
        Probe::Gone => Some(false),
        Probe::Unknown => None,
    }
}

/// Whether the process that recorded `(pid, start)` is still that process, from ONE
/// handle, so the answer cannot mix two processes: `Some(true)` it is running with
/// that creation time, `Some(false)` provably not, `None` it cannot be asked.
pub fn same_process(pid: u32, start: u64) -> Option<bool> {
    match probe(pid) {
        Probe::Running(created) => Some(created == start),
        Probe::Gone => Some(false),
        Probe::Unknown => None,
    }
}

/// What one look at a pid finds.
enum Probe {
    /// Running, with this creation time.
    Running(u64),
    /// Proven not running: no such pid (`ERROR_INVALID_PARAMETER`), or its process
    /// has exited though someone still holds a handle to it.
    Gone,
    /// Could not be asked - access denied, or a call failed. Never read as gone.
    Unknown,
}

/// `OpenProcess` for `PROCESS_QUERY_LIMITED_INFORMATION` alone - what a same-user
/// process always grants, and all the two calls after it need.
///
/// Exit is read from the exit code rather than by waiting, which would need
/// `SYNCHRONIZE` as well; the price is that a process that exited with code 259
/// (`STILL_ACTIVE`) reads as running, which only keeps a record. The pid's low two
/// bits are ignored by Windows (measured: `OpenProcess(pid + 1)` opens `pid`), and
/// that is harmless: a record's pid came through this same call.
fn probe(pid: u32) -> Probe {
    // SAFETY: no pointers; a null handle is the failure, read at once.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        // SAFETY: no arguments; reads this thread's last error, set by the call above.
        return match unsafe { GetLastError() } {
            ERROR_INVALID_PARAMETER => Probe::Gone,
            _ => Probe::Unknown,
        };
    }
    probe_open(&Process(h))
}

/// [`probe`], on a handle already open for at least `PROCESS_QUERY_LIMITED_INFORMATION`
/// - so a caller asking something else of the same process asks it of the same one.
fn probe_open(process: &Process) -> Probe {
    let mut code = 0u32;
    // SAFETY: an open process handle and a live u32.
    if unsafe { GetExitCodeProcess(process.0, &mut code) } == 0 {
        return Probe::Unknown;
    }
    if code != STILL_ACTIVE as u32 {
        return Probe::Gone;
    }
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: an open process handle and four live FILETIMEs.
    let ok = unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) };
    match (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime) {
        created if ok != 0 && created != 0 => Probe::Running(created),
        _ => Probe::Unknown,
    }
}

/// An open process handle, closed on drop.
struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// No pty to resolve: Windows has no device a title can be written to as bytes.
/// session-start and session-end title the console instead, through
/// [`set_session_title`]; tmux, which does not exist natively here, finds nothing.
pub fn session_tty(_claude_pid: &OsStr) -> Option<File> {
    None
}

/// How far up its parent chain a hook looks for `$CLAUDE_PID`. Measured under Git
/// Bash: tabstatus.exe, `usr\bin\bash.exe`, Git's `bin\bash.exe` launcher, claude.exe -
/// three hops.
const ANCESTOR_HOPS: usize = 8;

/// How long [`set_session_title`] waits on Claude's console. Detaching, attaching and
/// titling take about 1ms; but AttachConsole waits on the console server, which a
/// terminal that is not draining its output holds for as long as it stalls
/// (measured: 3.6s of a 4s stall, under the inbox conhost and OpenConsole alike),
/// and SessionEnd's whole hook budget is 1s.
const CONSOLE_BUDGET: Duration = Duration::from_millis(250);

/// How long an overrun [`set_session_title`] then spends cancelling the console call
/// it left in flight, see [`abandon`]. Cancelling one takes about 5ms (measured).
const CANCEL_BUDGET: Duration = Duration::from_millis(50);

/// Set the title of the console Claude Code runs in - the session's tab, under
/// Windows Terminal - for the two edges `terminalSequence` cannot carry. `Ok(true)`
/// painted, `Ok(false)` deliberately did not, and `Err` is the paint itself failing
/// or overrunning [`CONSOLE_BUDGET`].
///
/// A title set through the console API is console state, and a pseudo console
/// forwards every change to its terminal as an OSC 0 - measured through Windows
/// Terminal 1.24's own OpenConsole.exe and the ConPTY package's 1.25
/// (`ESC ] 0 ; title ESC \`), and the inbox conhost (`... BEL`), the empty title
/// included. It is the channel Claude Code's own `process.title` uses; it needs
/// neither the buffer's VT mode nor its code page, which bytes written into Claude's
/// output would; and it composes with the `terminalSequence` titles Claude writes,
/// which update the same state.
///
/// A hook is NOT in that console: Claude Code spawns it with `windowsHide`, which is
/// CREATE_NO_WINDOW, so it runs in a hidden console of its own that reaches no tab
/// (measured). So this leaves that console, attaches to Claude's for one call, and
/// leaves again - which works as well for a hook that inherited Claude's console, or
/// that has none.
///
/// THE HEADLESS GUARD, the Windows form of Unix's "fd 1 is a tty", is three proofs,
/// and any "no" paints nothing:
///   1. `claude_pid` is a LIVE ANCESTOR of this process: the parent chain is walked
///      up from here, each hop running and created no later than its child, so a
///      stale or recycled pid, a sibling, and every unrelated process are refused.
///   2. That process's standard output - read out of its PEB, the one place Windows
///      records it, current as of any `SetStdHandle` - is a character device. That
///      refuses `claude -p > file` and `claude -p | jq` before any console is touched.
///   3. Once attached, that same handle is a screen buffer of the console attached,
///      which refuses NUL, a character device too. Asked any earlier, a console call
///      on it answers for the CALLER's console (measured).
///
/// What it cannot prove: that a terminal shows the console - a hidden one is harmless
/// to title - or that the process is claude.exe rather than whatever spawned this
/// hook and draws on that console, which is what the README's hand fix relies on.
///
/// Bounded: the console part runs on a worker thread given [`CONSOLE_BUDGET`]. One
/// that overruns it is abandoned and its console call cancelled ([`abandon`]), so
/// the hook can exit: process exit waits for a console call in flight, and a stalled
/// terminal holds that call for as long as it stalls. Nothing of Claude's console
/// changes but its title, and this process's standard handles - the pipes the hook
/// protocol runs over - are untouched.
pub fn set_session_title(claude_pid: &OsStr, title: &str) -> io::Result<bool> {
    let Some(pid) = parse_pid(claude_pid) else { return Ok(false) };
    if !is_live_ancestor(pid) {
        return Ok(false);
    }
    let Some(stdout) = char_stdout_of(pid) else { return Ok(false) };
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    on_console_worker(move |abandoned| title_console(pid, &stdout, &title, abandoned))
}

/// Run `work` on a worker thread for at most [`CONSOLE_BUDGET`], then [`abandon`] it:
/// its answer, or `TimedOut`. `work` is handed the flag an abandon raises, to check
/// before each console call it makes.
fn on_console_worker<F>(work: F) -> io::Result<bool>
where
    F: FnOnce(&AtomicBool) -> io::Result<bool> + Send + 'static,
{
    let abandoned = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&abandoned);
    let (tx, rx) = mpsc::channel();
    let worker = thread::Builder::new().spawn(move || {
        let _ = tx.send(work(&flag));
    })?;
    match rx.recv_timeout(CONSOLE_BUDGET) {
        Ok(painted) => painted,
        Err(_) => {
            abandon(&worker, &abandoned);
            Err(io::ErrorKind::TimedOut.into())
        }
    }
}

/// Stop an overrun worker from holding up process exit, within [`CANCEL_BUDGET`].
///
/// A thread inside a console call when the process exits keeps it from exiting until
/// that call returns - 3.7s of a 4s stall, measured, when the stall began after the
/// attach (one that began before it leaves the worker in AttachConsole, which does
/// not). `CancelSynchronousIo` ends such a call at once (measured, ~5ms). It is
/// repeated because the worker may be between two calls when first asked; the flag
/// keeps it from starting another, and its `Detach` from calling FreeConsole, which
/// the exit does anyway. Should a call not yield in time the hook still exits, late.
fn abandon(worker: &JoinHandle<()>, abandoned: &AtomicBool) {
    abandoned.store(true, Ordering::SeqCst);
    let start = Instant::now();
    while !worker.is_finished() && start.elapsed() < CANCEL_BUDGET {
        // SAFETY: a thread handle std keeps open for the JoinHandle's lifetime; this
        // cancels only that thread's synchronous I/O, and failing does nothing.
        unsafe { CancelSynchronousIo(worker.as_raw_handle()) };
        thread::sleep(Duration::from_millis(1));
    }
}

/// The part of [`set_session_title`] that runs attached to Claude's console, on the
/// worker [`on_console_worker`] runs. Once `abandoned`, it makes no further call.
fn title_console(
    pid: u32,
    stdout: &OwnedHandle,
    title: &[u16],
    abandoned: &AtomicBool,
) -> io::Result<bool> {
    let live = || !abandoned.load(Ordering::SeqCst);
    // SAFETY: no pointers; each call changes this process's own console state only.
    unsafe {
        // Ctrl+C typed into Claude's console while this is attached reaches this
        // process too, and ends it with 0xC000013A (measured). The ignore FLAG, not a
        // handler: AttachConsole discards every handler registered before it, even
        // one registered with no console at all, and keeps the flag (measured).
        SetConsoleCtrlHandler(None, 1);
        // AttachConsole refuses a process that has a console (error 5), and a hook
        // has one. Our standard handles are pipes, which this leaves as they are.
        FreeConsole();
        if !live() || AttachConsole(pid) == 0 {
            // Abandoned already, or no console at all - a detached or GUI parent -
            // so no tab.
            return Ok(false);
        }
    }
    let _detach = Detach(abandoned);
    // SAFETY: a function of the documented signature, never removed. Registered after
    // the attach, the only place a handler takes effect, for Ctrl+Break, which the
    // flag does not cover. That leaves a Ctrl+Break unhandled from the moment the
    // console knows this process until this line - the tail of AttachConsole - and
    // one landing there ends the hook with 0xC000013A: measured under a Ctrl+Break
    // every 200us, about one hook in two; a key would have to land in that sliver.
    // Claude and its console are untouched; the title is simply not set. No
    // in-process order closes it.
    unsafe { SetConsoleCtrlHandler(Some(swallow), 1) };
    // SAFETY: an all-zero CONSOLE_SCREEN_BUFFER_INFO is valid; filled or refused.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: a live handle this process owns, and a live out-parameter.
    if !live() || unsafe { GetConsoleScreenBufferInfo(stdout.as_raw_handle(), &mut info) } == 0 {
        return Ok(false);
    }
    // SAFETY: a NUL-terminated UTF-16 string that outlives the call.
    if !live() || unsafe { SetConsoleTitleW(title.as_ptr()) } == 0 {
        return if live() { Err(io::Error::last_os_error()) } else { Ok(false) };
    }
    Ok(true)
}

/// Every control event that arrives once it is registered, swallowed while attached
/// to Claude's console (see [`title_console`] for the gap before). A close still ends
/// the process once this returns, which is harmless here.
unsafe extern "system" fn swallow(_event: u32) -> i32 {
    1
}

/// Leave whatever console this process is attached to, on every return path - unless
/// the call was abandoned: a FreeConsole on a stalled console blocks like any other
/// call, and the exit that follows detaches anyway.
struct Detach<'a>(&'a AtomicBool);

impl Drop for Detach<'_> {
    fn drop(&mut self) {
        if !self.0.load(Ordering::SeqCst) {
            // SAFETY: no arguments; detaches this process only.
            unsafe { FreeConsole() };
        }
    }
}

/// `$CLAUDE_PID` as a pid: 1-10 ASCII digits, in range, not zero - else nothing.
fn parse_pid(raw: &OsStr) -> Option<u32> {
    let b = raw.as_encoded_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok().filter(|&p| p != 0)
}

/// Whether `pid` is a live ancestor of this process within [`ANCESTOR_HOPS`]. A hop
/// whose recorded parent is YOUNGER than it has outlived that parent, whose pid went
/// to a newer process: nothing above it is ours, and the walk stops there.
fn is_live_ancestor(pid: u32) -> bool {
    let Some((mut born, mut parent)) = hop(std::process::id()) else { return false };
    for _ in 0..ANCESTOR_HOPS {
        let Some((parent_born, grandparent)) = hop(parent) else { return false };
        if parent_born > born {
            return false;
        }
        if parent == pid {
            return true;
        }
        (born, parent) = (parent_born, grandparent);
    }
    false
}

/// A RUNNING process's creation time and the pid that created it, from one handle,
/// so the answer cannot mix two processes.
fn hop(pid: u32) -> Option<(u64, u32)> {
    let p = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let Probe::Running(created) = probe_open(&p) else { return None };
    Some((created, u32::try_from(basic_info(&p)?.parent).ok()?))
}

/// The standard output of `pid`, duplicated into this process, when it is a character
/// device - the Windows `/proc/<pid>/fd/1`. The PEB offsets are the 64-bit layout
/// (`PEB.ProcessParameters` at 0x20, and `StandardOutput` at 0x28 in that), stable
/// since NT; a WOW64 process keeps its current handles in its 32-bit copy, and a
/// 32-bit build cannot use these offsets, so both are refused.
fn char_stdout_of(pid: u32) -> Option<OwnedHandle> {
    if !cfg!(target_pointer_width = "64") {
        return None;
    }
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE;
    let p = open_process(pid, access)?;
    let mut wow = 0;
    // SAFETY: an open process handle and a live BOOL.
    if unsafe { IsWow64Process(p.0, &mut wow) } == 0 || wow != 0 {
        return None;
    }
    let params = read_word(&p, basic_info(&p)?.peb.checked_add(0x20)?)?;
    let handle = read_word(&p, params.checked_add(0x28)?)?;
    if handle == 0 {
        return None;
    }
    let mut dup: HANDLE = std::ptr::null_mut();
    // SAFETY: a process handle opened with PROCESS_DUP_HANDLE, a handle value that is
    // only ever interpreted in that process, and a live out-parameter.
    let ok = unsafe {
        DuplicateHandle(p.0, handle as HANDLE, GetCurrentProcess(), &mut dup, 0, 0, DUPLICATE_SAME_ACCESS)
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: a handle this process now owns, closed exactly once, by the drop.
    let dup = unsafe { OwnedHandle::from_raw_handle(dup) };
    // SAFETY: a live handle.
    (unsafe { GetFileType(dup.as_raw_handle()) } == FILE_TYPE_CHAR).then_some(dup)
}

fn open_process(pid: u32, access: u32) -> Option<Process> {
    // SAFETY: no pointers; a null handle is the failure.
    let h = unsafe { OpenProcess(access, 0, pid) };
    (!h.is_null()).then(|| Process(h))
}

/// `PROCESS_BASIC_INFORMATION` (winternl.h), spelled out so the PEB stays an address
/// and no windows-sys feature is needed for the pointer types.
#[repr(C)]
#[derive(Default)]
struct BasicInfo {
    exit_status: i32,
    peb: usize,
    affinity: usize,
    base_priority: i32,
    pid: usize,
    parent: usize,
}

fn basic_info(p: &Process) -> Option<BasicInfo> {
    let mut info = BasicInfo::default();
    let mut len = 0u32;
    // SAFETY: an open process handle; `info` is exactly the size passed.
    let status = unsafe {
        NtQueryInformationProcess(
            p.0,
            ProcessBasicInformation,
            (&mut info as *mut BasicInfo).cast(),
            std::mem::size_of::<BasicInfo>() as u32,
            &mut len,
        )
    };
    (status >= 0).then_some(info)
}

/// One pointer-sized word of another process's memory, or nothing.
fn read_word(p: &Process, at: usize) -> Option<usize> {
    let (mut word, mut got) = (0usize, 0usize);
    let size = std::mem::size_of::<usize>();
    // SAFETY: reads into a live usize exactly its size; a bad address fails the call.
    let ok = unsafe {
        ReadProcessMemory(p.0, at as *const _, (&mut word as *mut usize).cast(), size, &mut got)
    };
    (ok != 0 && got == size).then_some(word)
}

/// tmux client ptys do not exist on native Windows.
pub fn write_tty(_path: &Path, _bytes: &[u8]) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cctab-win-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("mkdir");
        d
    }

    fn abs(p: &Path) -> PathBuf {
        std::path::absolute(p).expect("absolute")
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The replace's target is absolute and verbatim in every spelling a state
    /// directory can take, `..` folded before the prefix makes it literal. (Pure
    /// string work: nothing here touches a disk or a network.)
    #[test]
    fn a_rename_target_is_absolute_and_verbatim() {
        let t = |p: &str| String::from_utf16(&rename_target(Path::new(p)).expect("target")).expect("utf16");
        assert_eq!(t(r"C:\state\x\..\s1"), r"\\?\C:\state\s1");
        assert_eq!(t("C:/state/s1"), r"\\?\C:\state\s1");
        assert_eq!(t(r"\\srv\share\state\s1"), r"\\?\UNC\srv\share\state\s1");
        assert_eq!(t(r"\\?\C:\state\s1"), r"\\?\C:\state\s1");
        assert_eq!(t(r"\\?\UNC\srv\share\s1"), r"\\?\UNC\srv\share\s1");
        let rel = t("s1");
        assert!(rel.starts_with(r"\\?\") && rel.ends_with(r"\s1") && rel.len() > 8, "{rel}");
    }

    /// What makes the junction usable as THE plugin link: std reads it as a directory
    /// link, and `read_link` hands back the plain drive path - no `\??\` or `\\?\` -
    /// so the `t == c.tree` comparisons in `manage` hold.
    #[test]
    fn a_junction_reads_back_as_a_plain_directory_link() {
        let d = scratch("junction");
        let target = d.join("tree");
        fs::create_dir_all(&target).expect("mkdir");
        let link = d.join("link");
        link_dir(&target, &link).expect("linked");
        let ft = fs::symlink_metadata(&link).expect("there").file_type();
        assert!(ft.is_symlink() && ft.is_symlink_dir(), "a directory link");
        let back = fs::read_link(&link).expect("read_link");
        assert_eq!(back, abs(&target));
        assert!(!back.to_string_lossy().starts_with(r"\\?\"), "{}", back.display());
        remove_dir_link(&link).expect("removed");
        assert!(target.is_dir());
        let _ = fs::remove_dir_all(&d);
    }

    /// A junction cannot be relative, so a relative target means what a symlink would
    /// have meant - relative to the link's own directory - and a verbatim `\\?\`
    /// target is stored plain.
    #[test]
    fn a_relative_or_verbatim_target_is_stored_as_a_plain_absolute_path() {
        let d = scratch("junction-rel");
        let target = d.join("tree");
        fs::create_dir_all(&target).expect("mkdir");
        let rel = d.join("rel");
        link_dir(Path::new("tree"), &rel).expect("relative");
        assert_eq!(fs::read_link(&rel).expect("read_link"), abs(&target));
        let mut verbatim = OsString::from(r"\\?\");
        verbatim.push(abs(&target));
        let vlink = d.join("verbatim");
        link_dir(Path::new(&verbatim), &vlink).expect("verbatim");
        assert_eq!(fs::read_link(&vlink).expect("read_link"), abs(&target));
        let _ = fs::remove_dir_all(&d);
    }

    /// A target a junction cannot name is refused BEFORE the directory is created, and
    /// the preflight probe carries that refusal - so it lands before any write.
    #[test]
    fn a_network_target_is_refused_and_leaves_nothing_behind() {
        let d = scratch("junction-unc");
        let link = d.join("link");
        let unc = Path::new(r"\\server\share\tree");
        let e = link_dir(unc, &link).expect_err("refused");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{}", e);
        assert!(fs::symlink_metadata(&link).is_err(), "no directory left behind");
        assert!(probe_dir_link(unc, &d).is_err());
        assert!(names(&d).is_empty(), "{:?}", names(&d));
        let _ = fs::remove_dir_all(&d);
    }

    /// `$CLAUDE_PID` is a decimal pid or nothing: no sign, no space, not zero, in range.
    #[test]
    fn a_claude_pid_is_decimal_digits_in_range_and_not_zero() {
        for bad in ["", "0", "00", "abc", "-1", "+1", " 12", "12 ", "1e3", "4294967296", "12345678901"] {
            assert_eq!(parse_pid(OsStr::new(bad)), None, "{bad:?}");
        }
        assert_eq!(parse_pid(OsStr::new("4")), Some(4));
        assert_eq!(parse_pid(OsStr::new("4294967295")), Some(u32::MAX));
    }

    /// A child that stays alive, with the given standard output, until its stdin closes.
    fn held_child(stdout: Stdio) -> std::process::Child {
        Command::new(std::env::current_exe().expect("exe"))
            .args(["--exact", "sys::windows::tests::hold_the_image_until_stdin_closes"])
            .args(["--ignored", "--test-threads=1"])
            .env("CCTAB_TEST_HOLD", "1")
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn")
    }

    /// Guard 1. The process that started this test is an ancestor; this process, a
    /// child it spawned - running, then exited - and a pid nothing has are not.
    /// Nothing here touches a console.
    #[test]
    fn only_a_running_ancestor_passes_the_walk() {
        let me = std::process::id();
        let (_, parent) = hop(me).expect("this process");
        assert!(is_live_ancestor(parent), "the process that started this test");
        assert!(!is_live_ancestor(me), "not its own ancestor");
        assert!(!is_live_ancestor(u32::MAX), "no such pid");
        let mut child = held_child(Stdio::null());
        let pid = child.id();
        assert!(!is_live_ancestor(pid), "a child is not an ancestor");
        drop(child.stdin.take());
        child.wait().expect("wait");
        assert!(!is_live_ancestor(pid), "nor once it has exited");
    }

    /// Guard 2 reads another process's CURRENT standard output out of its PEB: a file
    /// and a pipe are refused before any console is touched, and NUL - a character
    /// device - is not, which is why guard 3 exists.
    #[test]
    fn a_stdout_that_is_a_file_or_a_pipe_is_refused_and_nul_needs_the_third_proof() {
        let d = scratch("stdout");
        let file = fs::File::create(d.join("out")).expect("file");
        let nul = OpenOptions::new().write(true).open("NUL").expect("NUL");
        for (stdout, what, char_device) in [
            (Stdio::from(file), "a file", false),
            (Stdio::piped(), "a pipe", false),
            (Stdio::from(nul), "NUL", true),
        ] {
            let mut child = held_child(stdout);
            assert_eq!(char_stdout_of(child.id()).is_some(), char_device, "{what}");
            drop(child.stdin.take());
            child.wait().expect("wait");
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// The whole function, on everything a unit test may offer it: a non-pid, a pid
    /// nothing has, a child. Each is refused - `Ok(false)` - before any console call.
    /// It is NEVER called here with an ancestor: this test's ancestors include the
    /// terminal it was started from, which that would retitle. tests/conpty.rs paints,
    /// inside a pseudo console of its own.
    #[test]
    fn set_session_title_refuses_everything_but_an_ancestor() {
        for raw in ["", "0", "not-a-pid", "4294967295"] {
            assert!(!set_session_title(OsStr::new(raw), "x").expect("no error"), "{raw:?}");
        }
        let mut child = held_child(Stdio::null());
        let pid = child.id().to_string();
        assert!(!set_session_title(OsStr::new(&pid), "x").expect("no error"), "a child");
        drop(child.stdin.take());
        child.wait().expect("wait");
    }

    /// The worker's bound, without a console: a prompt answer comes back as it is, and
    /// one stuck in synchronous I/O - a read of a pipe nobody writes, standing in for a
    /// console call on a stalled terminal - is cancelled and has FINISHED by the time
    /// `TimedOut` returns, so it cannot hold up the hook's exit.
    #[test]
    fn an_overrun_console_worker_is_cancelled_before_it_times_out() {
        assert!(on_console_worker(|_| Ok(true)).expect("answered"));
        let (mut r, w) = std::io::pipe().expect("pipe");
        let (tx, rx) = mpsc::channel();
        let t = Instant::now();
        let got = on_console_worker(move |abandoned| {
            let read = r.read(&mut [0u8; 1]);
            let _ = tx.send(read.is_err());
            Ok(!abandoned.load(Ordering::SeqCst))
        });
        let took = t.elapsed();
        assert_eq!(got.expect_err("overran").kind(), io::ErrorKind::TimedOut);
        assert_eq!(rx.try_recv(), Ok(true), "the read was cancelled and the worker is done");
        assert!(took < CONSOLE_BUDGET + CANCEL_BUDGET + Duration::from_millis(100), "{took:?}");
        drop(w);
    }

    /// `remove_dir_link` is for links. On a real directory - even an empty one, which
    /// `remove_dir` would take - it refuses.
    #[test]
    fn removing_a_link_refuses_a_real_directory() {
        let d = scratch("junction-real");
        let real = d.join("real");
        fs::create_dir_all(&real).expect("mkdir");
        assert!(remove_dir_link(&real).is_err());
        assert!(real.is_dir());
        let _ = fs::remove_dir_all(&d);
    }

    /// Both repoint paths: the one rename std manages over a junction on NTFS (the
    /// ordinary case, and atomic), and the rename-aside fallback for a filesystem that
    /// refuses it, driven directly because no local volume here will refuse. Each must
    /// leave the new target live, no aside or temp link behind, and both targets whole.
    #[test]
    fn a_junction_is_repointed_by_one_rename_or_by_the_aside_fallback() {
        let d = scratch("junction-swap");
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        for t in [&a, &b, &c] {
            fs::create_dir_all(t).expect("mkdir");
            fs::write(t.join("f"), t.to_string_lossy().as_bytes()).expect("write");
        }
        let (live, tmp) = (d.join("live"), d.join(".live.cctab-tmp.1"));
        link_dir(&a, &live).expect("linked");

        link_dir(&b, &tmp).expect("temp");
        replace_dir_link(&tmp, &live).expect("replaced");
        assert_eq!(fs::read_link(&live).expect("read_link"), abs(&b));

        link_dir(&c, &tmp).expect("temp");
        swap_aside(&tmp, &live).expect("swapped aside");
        assert_eq!(fs::read_link(&live).expect("read_link"), abs(&c));

        for t in [&a, &b, &c] {
            assert_eq!(fs::read(t.join("f")).expect("intact"), t.to_string_lossy().as_bytes());
        }
        assert_eq!(names(&d), vec!["a", "b", "c", "live"]);
        let _ = fs::remove_dir_all(&d);
    }

    /// Not a test: the child process the running-binary test starts. It keeps its own
    /// image mapped until its stdin closes, which is exactly what a hook executing
    /// `bin\tabstatus.exe` does to that file.
    #[test]
    #[ignore = "a child process for another test"]
    fn hold_the_image_until_stdin_closes() {
        if std::env::var_os("CCTAB_TEST_HOLD").is_some() {
            let _ = io::stdin().read_to_end(&mut Vec::new());
        }
    }

    /// The re-install case: `bin\tabstatus.exe` is being executed right now. Windows
    /// will neither delete nor replace it, so `replace_running` renames it aside: the
    /// new bytes are in place, the running process carries on, the aside copy
    /// survives a sweep while it runs, and a sweep after it exits takes it.
    #[test]
    fn replacing_a_running_binary_sets_it_aside_and_a_later_sweep_takes_it() {
        let d = scratch("running");
        let victim = d.join("victim.exe");
        fs::copy(std::env::current_exe().expect("exe"), &victim).expect("copy");
        let mut child = Command::new(&victim)
            .args(["--exact", "sys::windows::tests::hold_the_image_until_stdin_closes"])
            .args(["--ignored", "--test-threads=1"])
            .env("CCTAB_TEST_HOLD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        // The premise, proved rather than assumed: the file is in use, so neither the
        // delete nor the one-rename replace that Unix does is allowed.
        assert!(fs::remove_file(&victim).is_err(), "a running image cannot be deleted");
        let blocked = d.join(".blocked.cctab-tmp.1");
        fs::write(&blocked, b"x").expect("write");
        assert!(fs::rename(&blocked, &victim).is_err(), "a running image cannot be replaced");
        fs::remove_file(&blocked).expect("rm");

        let new = d.join(".victim.exe.cctab-tmp.1");
        fs::write(&new, b"the new build").expect("write");
        let left = replace_running(&new, &victim).expect("replaced");
        assert_eq!(fs::read(&victim).expect("read"), b"the new build");
        assert!(!new.exists(), "moved, not copied");
        assert!(child.try_wait().expect("try_wait").is_none(), "the running process carries on");
        let aside = beside(&victim, "old");
        assert!(aside.is_file(), "the running image was set aside, not lost");
        assert_eq!(left.as_deref(), Some(aside.as_path()), "and the caller is told where");

        assert!(sweep_replaced(&d).is_empty(), "nothing it could take");
        assert!(aside.is_file(), "a sweep must not take a file something still runs");

        drop(child.stdin.take());
        child.wait().expect("wait");
        // The image is released as the process is torn down; give the kernel a moment
        // rather than racing it.
        let mut swept = Vec::new();
        for _ in 0..50 {
            swept.extend(sweep_replaced(&d));
            if !aside.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(!aside.exists(), "once nothing runs it, the sweep takes it");
        assert_eq!(swept, vec![aside.clone()], "and says what it took");
        assert_eq!(names(&d), vec!["victim.exe"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn only_this_tools_aside_and_probe_names_are_swept() {
        assert!(is_retired(".tabstatus.exe.cctab-old.123"));
        assert!(is_retired(".claude-tabstatus.cctab-old.9"));
        assert!(is_retired(".cctab-ltest.42"));
        assert!(is_retired(".claude-tabstatus.cctab-new.7"));
        assert!(!is_retired("claude-tabstatus.cctab-new.7"));
        assert!(!is_retired(".claude-tabstatus.cctab-new.x"));
        assert!(!is_retired("tabstatus.exe.cctab-old.123"));
        assert!(!is_retired(".tabstatus.exe.cctab-old."));
        assert!(!is_retired(".tabstatus.exe.cctab-old.12a"));
        assert!(!is_retired(".cctab-ltest."));
        assert!(!is_retired(".tabstatus.exe.cctab-tmp.123"));
        assert!(!is_retired("tabstatus.exe"));
    }

    /// A junction appears at its name whole or not at all, so it never REPLACES
    /// anything: onto an existing directory it is refused, that directory is
    /// untouched, and no scratch junction is left beside it. A scratch left by a kill -
    /// the real, empty directory between a junction's two steps - is swept; a
    /// same-named directory with something in it is not.
    #[test]
    fn a_junction_never_replaces_what_is_there_and_a_killed_ones_scratch_is_swept() {
        let d = scratch("junction-noreplace");
        let target = d.join("tree");
        fs::create_dir_all(&target).expect("mkdir");
        let taken = d.join("taken");
        fs::create_dir_all(&taken).expect("mkdir");
        assert!(link_dir(&target, &taken).is_err(), "an existing name is refused");
        assert!(!fs::symlink_metadata(&taken).expect("there").file_type().is_symlink());
        assert_eq!(names(&d), vec!["taken", "tree"]);

        let killed = d.join(".live.cctab-new.1");
        fs::create_dir_all(&killed).expect("mkdir");
        let theirs = d.join(".other.cctab-new.2");
        fs::create_dir_all(&theirs).expect("mkdir");
        fs::write(theirs.join("keep"), b"x").expect("write");
        assert_eq!(sweep_replaced(&d), vec![killed.clone()]);
        assert!(!killed.exists() && theirs.join("keep").is_file());
        let _ = fs::remove_dir_all(&d);
    }

    /// The probe names what EXISTS of a target that is not written yet, follows it,
    /// and leaves nothing behind.
    #[test]
    fn the_probe_follows_a_junction_to_what_exists_of_an_unwritten_target() {
        let d = scratch("probe-deep");
        probe_dir_link(&d.join("not").join("yet").join("tree"), &d).expect("resolves");
        assert!(names(&d).is_empty(), "{:?}", names(&d));
        let _ = fs::remove_dir_all(&d);
    }

    /// One directory, every spelling NTFS answers to: letter case, `\\?\`, `..`, and an
    /// 8.3 short name where the volume makes them. All normalise to the stored
    /// spelling, and a component not written yet keeps the caller's.
    #[test]
    fn every_spelling_of_one_directory_normalises_to_the_stored_one() {
        let d = normalize(&scratch("spelling"));
        let stored = d.join("Stored Name");
        fs::create_dir_all(stored.join("LongDirectoryName")).expect("mkdir");
        let lower = PathBuf::from(stored.to_string_lossy().to_lowercase());
        assert_eq!(normalize(&lower), stored, "letter case");
        let mut verbatim = OsString::from(r"\\?\");
        verbatim.push(&stored);
        assert_eq!(normalize(Path::new(&verbatim)), stored, "\\\\?\\ prefix");
        assert_eq!(normalize(&stored.join("x").join("..")), stored, "`..` folded");
        assert_eq!(normalize(&lower.join("NotYet")), stored.join("NotYet"), "unwritten tail kept");
        let short = stored.join("LONGDI~1");
        if short.is_dir() {
            assert_eq!(normalize(&short), stored.join("LongDirectoryName"), "8.3 short name");
        }
        assert!(same_path(&lower.join("notyet"), &stored.join("NotYet")));
        assert!(!same_path(&stored, &stored.join("LongDirectoryName")));
        let _ = fs::remove_dir_all(&d);
    }

    /// The refusal that keeps the tree out of `<config>\skills` asks where a directory
    /// written at the path would LAND, so no spelling of `skills` - and no junction
    /// into it - walks past it, while a sibling that merely shares a prefix does.
    #[test]
    fn is_within_sees_through_case_verbatim_and_junctions() {
        let d = scratch("within");
        let skills = d.join("cfg").join("skills");
        fs::create_dir_all(&skills).expect("mkdir");
        let upper = PathBuf::from(skills.to_string_lossy().to_uppercase());
        assert!(is_within(&upper.join("claude-tabstatus"), &skills), "case, unwritten tail");
        assert!(is_within(&upper, &skills), "the directory itself");
        let mut verbatim = OsString::from(r"\\?\");
        verbatim.push(skills.join("foo"));
        assert!(is_within(Path::new(&verbatim), &skills), "\\\\?\\ prefix");
        let via = d.join("via");
        link_dir(&skills, &via).expect("junction");
        assert!(is_within(&via.join("foo"), &skills), "through a junction");
        assert!(!is_within(&d.join("cfg").join("skills2").join("x"), &skills), "a sibling");
        assert!(!is_within(&d.join("cfg"), &skills), "the parent");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_removal_hint_is_a_powershell_command_with_the_path_quoted() {
        assert_eq!(
            remove_dir_command(Path::new(r"C:\Users\A B\it's")),
            r"Remove-Item -Recurse -Force -LiteralPath 'C:\Users\A B\it''s'"
        );
    }
}
