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

use windows_sys::Wdk::System::SystemServices::RtlUpcaseUnicodeChar;
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, ERROR_ACCESS_DENIED,
    ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
    ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION, FILETIME, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, STILL_ACTIVE,
};
use windows_sys::Win32::Security::{
    AddAccessAllowedAce, GetKernelObjectSecurity, GetSecurityDescriptorControl, InitializeAcl,
    InitializeSecurityDescriptor, SetKernelObjectSecurity, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION,
    GROUP_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SE_DACL_AUTO_INHERITED, SE_DACL_AUTO_INHERIT_REQ,
    SE_DACL_PROTECTED,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdInfo, FileRenameInfoEx, CREATE_NEW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FindClose, FindFirstFileW, GetDriveTypeW,
    GetFileInformationByHandleEx, GetFileType, GetVolumeInformationByHandleW, LockFileEx,
    MoveFileExW, SetFileInformationByHandle, DELETE, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_RENAME_INFO, FILE_TYPE_CHAR, LOCKFILE_EXCLUSIVE_LOCK, READ_CONTROL, WIN32_FIND_DATAW,
    WRITE_DAC, WRITE_OWNER,
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
/// so valid UTF-8 borrows and anything else is decoded by [`from_wtf8`]: an unpaired
/// surrogate in WTF-8's own three-byte form comes back as itself, so bytes sliced
/// out of an `OsStr` at ASCII boundaries - a `$HOME` losing its trailing separator,
/// a gitdir - round-trip exactly, and every other invalid sequence is U+FFFD, which
/// is what bytes read from a FILE (a `.git` gitfile, a marker's JSON) can hold.
/// Either way the result displays rather than failing. Nothing relies on the round
/// trip for safety: the marker's paths were written through `json::quote`, so are
/// UTF-8 already, and `tree::in_tree` confines whatever they spell to the tree.
pub fn os_str_from_bytes(b: &[u8]) -> Cow<'_, OsStr> {
    match std::str::from_utf8(b) {
        Ok(s) => Cow::Borrowed(OsStr::new(s)),
        Err(_) => Cow::Owned(from_wtf8(b)),
    }
}

/// A path's encoded bytes as a title shows them: each unpaired surrogate - WTF-8's
/// `ED A0..BF 80..BF`, the only bytes here that are not UTF-8 - becomes ONE U+FFFD,
/// as `to_string_lossy` makes it, where `text::repair` would see three bad bytes and
/// paint (and count against the length cap) three.
pub fn display_bytes(b: &[u8]) -> Cow<'_, [u8]> {
    if std::str::from_utf8(b).is_ok() {
        return Cow::Borrowed(b);
    }
    let mut out = Vec::with_capacity(b.len());
    let mut rest = b;
    while let [c, tail @ ..] = rest {
        rest = match rest {
            [0xED, 0xA0..=0xBF, 0x80..=0xBF, after @ ..] => {
                out.extend_from_slice("\u{fffd}".as_bytes());
                after
            }
            _ => {
                out.push(*c);
                tail
            }
        };
    }
    Cow::Owned(out)
}

/// The owned form of [`os_str_from_bytes`], without a copy when it is UTF-8.
pub fn os_string_from_vec(v: Vec<u8>) -> OsString {
    match String::from_utf8(v) {
        Ok(s) => OsString::from(s),
        Err(e) => from_wtf8(e.as_bytes()),
    }
}

/// `b` decoded as WTF-8: UTF-8, plus `ED A0..BF 80..BF` for an unpaired surrogate
/// (`D800`-`DFFF`). Any other invalid sequence is one U+FFFD, as
/// `String::from_utf8_lossy` would make it.
fn from_wtf8(b: &[u8]) -> OsString {
    let mut w = Vec::with_capacity(b.len());
    let mut rest = b;
    while !rest.is_empty() {
        let (good, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, &[][..]),
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                (std::str::from_utf8(good).unwrap_or_default(), bad)
            }
        };
        w.extend(good.encode_utf16());
        rest = match bad {
            [] => bad,
            [0xED, b1 @ 0xA0..=0xBF, b2 @ 0x80..=0xBF, tail @ ..] => {
                w.push(0xD000 | (u16::from(b1 & 0x3F) << 6) | u16::from(b2 & 0x3F));
                tail
            }
            _ => {
                w.push(0xFFFD);
                let len = std::str::from_utf8(bad).err().and_then(|e| e.error_len());
                &bad[len.unwrap_or(bad.len())..]
            }
        };
    }
    OsString::from_wide(&w)
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

/// What a rewrite carries over from the file it replaces where there is no mode: its
/// access control list. A self-relative security descriptor holding the DACL - with
/// its protected and auto-inherited flags - and the owner and group, read by
/// [`security_of`] and given to the replacement by [`create_secured`]. Held in
/// `u64`s because the descriptor's fields are 4-aligned and a `Vec<u8>` promises 1.
pub struct Security {
    sd: Vec<u64>,
}

pub const HAS_SECURITY: bool = true;
pub const CAN_FORCE_ACL: bool = true;

/// Keep the existing Windows byte-copy policy; CopyFileExW copies no DACL.
pub fn copy_secured(from: &Path, to: &mut File, _sec: &Security) -> io::Result<()> {
    use std::io::Write;
    to.write_all(&fs::read(from)?)
}

/// Windows has no final chmod that could alter the applied DACL.
pub fn verify_security(_f: &File, _sec: &Security, _mode: u32) -> io::Result<()> {
    Ok(())
}

impl Security {
    /// For calls that only READ the descriptor, which is all [`set_security`] makes.
    fn ptr(&self) -> *mut core::ffi::c_void {
        self.sd.as_ptr().cast_mut().cast()
    }
}

/// `path`'s access control list, for the file that is about to replace it - or `None`
/// when there is no file there, and the new one takes its directory's inherited ACL
/// exactly as before.
///
/// WHY: a file written beside `path` and renamed over it brings its own security
/// descriptor - the one its directory gave it - so an ACL someone set on settings.json
/// itself (inheritance off, a group removed, a deny) was flattened by every install and
/// uninstall, and each backup of it, made by `CopyFileExW`, which copies no DACL, was
/// born with the directory's too. Unix keeps the mode; this is the same promise.
///
/// WHY A COPY, NOT `ReplaceFileW`, which keeps the target's ACL by itself. Measured
/// here with a reader holding the target open: under Node's share flags (read, write
/// and delete) the rename write_atomic does and `ReplaceFileW` both succeed; under
/// read-and-write or read only, both are refused (5 and 32). So neither is more robust
/// - but `ReplaceFileW` without a backup name documents a failure
/// (`ERROR_UNABLE_TO_MOVE_REPLACEMENT`) that leaves the target GONE and the new
/// file under its temp name, where the one rename leaves the old file or the new one;
/// and it copies the ACL only at the end, after the temp file was written with the
/// directory's. Copying the DACL first, onto an empty temp born private
/// ([`create_secured`]), keeps the rename and closes that window too.
///
/// An `Err` is a file that is there and whose ACL this user may not read; the caller
/// refuses rather than write a file it cannot make as private as the one it replaces.
/// Its kind is `Unsupported` when the FILESYSTEM keeps no Windows ACL at all - the
/// 9P share WSL exports answers the query with error 1 (measured) - where a file
/// written from Windows would not keep its permissions either (measured there: a
/// 0600 settings.json came back 0644); the caller says that rather than "cannot read".
/// FAT and exFAT never get here: the I/O manager answers for a filesystem with no
/// security of its own with a world-access descriptor, which costs nothing to carry.
pub fn security_of(path: &Path) -> io::Result<Option<Security>> {
    let f = match OpenOptions::new().access_mode(READ_CONTROL).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let info = DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION;
    let mut sd: Vec<u64> = Vec::new();
    loop {
        let len = u32::try_from(sd.len() * 8).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let mut need = 0u32;
        let buf = if sd.is_empty() { std::ptr::null_mut() } else { sd.as_mut_ptr().cast() };
        // SAFETY: `f` is an open handle. `buf` is null with a length of 0, or `sd`'s
        // live, writable `len` bytes; `need` is a live u32. The call keeps no pointer.
        let ok = unsafe { GetKernelObjectSecurity(f.as_raw_handle() as HANDLE, info, buf, len, &mut need) };
        if ok != 0 {
            break;
        }
        let e = io::Error::last_os_error();
        let code = e.raw_os_error();
        if code == Some(ERROR_INVALID_FUNCTION as i32) || code == Some(ERROR_NOT_SUPPORTED as i32) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, e));
        }
        if code != Some(ERROR_INSUFFICIENT_BUFFER as i32) || need <= len {
            return Err(e);
        }
        sd = vec![0u64; (need as usize).div_ceil(8)];
    }
    let (mut ctl, mut rev) = (0u16, 0u32);
    // SAFETY: `sd` holds the self-relative descriptor the call above wrote; the two
    // outputs are live locals.
    if unsafe { GetSecurityDescriptorControl(sd.as_mut_ptr().cast(), &mut ctl, &mut rev) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // An inheriting DACL is given back as one: without this request the kernel sets
    // its entries but drops the auto-inherited flag (measured: `D:AI(...)` came back
    // `D:(...)`), and the file would stop taking its directory's changes. With it, the
    // DACL is set exactly as given - its inherited entries included, which are the
    // ORIGINAL's, not recomputed from the directory: a file moved in from a directory
    // that gave it a deny keeps that deny (measured, and tested) - and keeps the flag.
    // A protected DACL needs nothing: that flag is kept.
    if ctl & SE_DACL_AUTO_INHERITED != 0 {
        // SAFETY: as above, through a pointer from the owned, mutable `sd`: the call
        // sets one control bit inside the buffer and keeps no pointer.
        let ok = unsafe {
            SetSecurityDescriptorControl(sd.as_mut_ptr().cast(), SE_DACL_AUTO_INHERIT_REQ, SE_DACL_AUTO_INHERIT_REQ)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Some(Security { sd }))
}

/// Create `path` - a new temp file, opened for writing as `create_new` does - with
/// `sec`'s DACL, before a byte is written to it.
///
/// It is BORN private ([`create_private`]): a protected DACL that grants its owner -
/// this process's user - and no one else, so between its creation and the DACL set
/// below nothing else can open it at all. Share mode 0 alone would not do: it bars a
/// second handle with data access, but one asking only for `READ_CONTROL`,
/// `WRITE_DAC` or `WRITE_OWNER` gets through it (measured), and access is checked at
/// open - a handle opened while the file had its directory's ACL would keep
/// `WRITE_DAC` after the DACL changed.
///
/// The DACL must apply, or the file is not made; the owner and group are carried
/// where this user may set them, and otherwise left as created - setting another
/// account's ownership takes a privilege, and the DACL is what decides who reads.
pub fn create_secured(path: &Path, sec: &Security) -> io::Result<File> {
    let f = create_private(path)?;
    set_security(&f, DACL_SECURITY_INFORMATION, sec)?;
    let _ = set_security(&f, OWNER_SECURITY_INFORMATION, sec);
    let _ = set_security(&f, GROUP_SECURITY_INFORMATION, sec);
    Ok(f)
}

/// `CreateFileW(CREATE_NEW)` of `path` - nothing there may be opened or replaced -
/// unshared, for writing and for setting its security, with a protected DACL whose
/// one entry is OWNER RIGHTS: full access for the file's owner, this process's user,
/// and nothing for anyone else - not even the implicit `READ_CONTROL` and `WRITE_DAC`
/// an owner has without it. Not an EMPTY DACL, which would do the same for others:
/// the handle that creates a file under one still writes, but is not counted for its
/// data in the share check, so once the real DACL is set another reader can open the
/// file beside it (measured). std's `OpenOptions` cannot pass a security descriptor,
/// hence the raw call.
fn create_private(path: &Path) -> io::Result<File> {
    // `SECURITY_DESCRIPTOR_REVISION`, from a feature this crate does not otherwise need.
    const SD_REVISION: u32 = 1;
    // OWNER RIGHTS, S-1-3-4: revision 1, one sub-authority, authority 3, then 4.
    // Spelled as bytes, in memory order, held in u32s for the alignment a SID needs.
    let mut owner_rights: [u32; 3] =
        [u32::from_ne_bytes([1, 1, 0, 0]), u32::from_ne_bytes([0, 0, 0, 3]), u32::from_ne_bytes([4, 0, 0, 0])];
    // The ACL header, one ACCESS_ALLOWED_ACE (8 bytes before its SID) and that SID.
    let mut acl = [0u32; 7];
    let mut sd = SECURITY_DESCRIPTOR::default();
    let sdp: *mut core::ffi::c_void = (&mut sd as *mut SECURITY_DESCRIPTOR).cast();
    let aclp: *mut ACL = acl.as_mut_ptr().cast();
    // SAFETY: `acl` (4-aligned, 28 bytes, the size passed), `owner_rights` (a valid
    // 12-byte SID) and `sd` are live, writable locals; the calls write only inside
    // them, and `sd` (absolute) keeps pointers to `acl` alone, which outlives the
    // CreateFileW below that only reads it. No call keeps a pointer past it.
    let ok = unsafe {
        InitializeAcl(aclp, std::mem::size_of_val(&acl) as u32, ACL_REVISION) != 0
            && AddAccessAllowedAce(aclp, ACL_REVISION, FILE_ALL_ACCESS, owner_rights.as_mut_ptr().cast()) != 0
            && InitializeSecurityDescriptor(sdp, SD_REVISION) != 0
            && SetSecurityDescriptorDacl(sdp, 1, aclp, 0) != 0
            && SetSecurityDescriptorControl(sdp, SE_DACL_PROTECTED, SE_DACL_PROTECTED) != 0
    };
    if !ok {
        return Err(io::Error::last_os_error());
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sdp,
        bInheritHandle: 0,
    };
    let name = wide_nul_long(path);
    let access = GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER;
    // SAFETY: `name` is NUL-terminated; `sa` and the descriptor and ACL it points at
    // are live locals the call only reads; no template handle.
    let h = unsafe { CreateFileW(name.as_ptr(), access, 0, &sa, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `h` is a valid file handle just opened and owned by nothing else.
    Ok(unsafe { File::from_raw_handle(h as _) })
}

fn set_security(f: &File, what: OBJECT_SECURITY_INFORMATION, sec: &Security) -> io::Result<()> {
    // SAFETY: `f` is an open handle with WRITE_DAC and WRITE_OWNER; `sec` holds a
    // valid self-relative descriptor, which the call only reads.
    if unsafe { SetKernelObjectSecurity(f.as_raw_handle() as HANDLE, what, sec.ptr()) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
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
    let (f, t) = (wide_nul_long(from), wide_nul_long(to));
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

/// [`wide_nul`] for a raw Win32 call that has to reach a LONG path. std adds the
/// `\\?\` prefix itself to every path of 248 units or more; a raw call gets nothing,
/// so past MAX_PATH it answers os error 3 - measured: `install --tree` under a long
/// config directory made its scratch junction through std and then could not rename
/// it into place, after settings.json had already been written. The same threshold
/// as std, so a path short enough for Win32 is passed exactly as it always was.
fn wide_nul_long(p: &Path) -> Vec<u16> {
    if wide(p).len() < 248 {
        return wide_nul(p);
    }
    match verbatim(p) {
        Some(v) => wide_nul(Path::new(&v)),
        None => wide_nul(p),
    }
}

/// `p` spelled `\\?\C:\...` or `\\?\UNC\server\share\...`: every separator a backslash
/// and `.` and `..` folded, which is the normalisation a verbatim path no longer gets
/// from Win32. `None` for anything else - relative, or verbatim or a device already.
fn verbatim(p: &Path) -> Option<OsString> {
    let mut comps = p.components();
    let mut out = match comps.next()? {
        Component::Prefix(pre) => match pre.kind() {
            Prefix::Disk(d) => OsString::from(format!(r"\\?\{}:", char::from(d))),
            Prefix::UNC(server, share) => {
                let mut s = OsString::from(r"\\?\UNC\");
                s.push(server);
                s.push(r"\");
                s.push(share);
                s
            }
            _ => return None,
        },
        _ => return None,
    };
    if comps.next() != Some(Component::RootDir) {
        return None;
    }
    let mut parts: Vec<&OsStr> = Vec::new();
    for c in comps {
        match c {
            Component::Normal(n) => parts.push(n),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            _ => return None,
        }
    }
    for n in parts {
        out.push(r"\");
        out.push(n);
    }
    Some(out)
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

/// Whether `name` is one [`replace_running`], [`replace_dir_link`] or [`link_dir`] set
/// aside or left half made: the names [`sweep_replaced`] removes.
pub fn is_set_aside(name: &str) -> bool {
    is_retired(name)
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

/// One name as NTFS compares it without regard to case: each UTF-16 unit through
/// the system's own upcase table, `RtlUpcaseUnicodeChar`.
///
/// Lossless: a name is UTF-16 units, not text, and the table maps every surrogate
/// unit to itself - so `<D800>`, `<D801>` and a real U+FFFD stay three different
/// names, as they are on disk, where a lossy decode made them one.
///
/// Unit to unit, as NTFS's own `$UpCase` table is: `é` folds to `É`; `ß`, a
/// character outside the BMP (`𐐨`/`𐐀`) and the letters a Unicode-derived fold
/// would merge but NTFS keeps apart (`ı`/`I`, `ſ`/`S`, `ς`/`Σ`, Cherokee, the
/// Georgian capitals...) all stay what they are, and `ᾳ`/`ᾼ`, which Unicode
/// upper-cases to two letters, fold to one. On a Windows 11 NTFS volume this agreed
/// with the disk on every BMP case pair, where Unicode's own upper case disagreed on
/// 253. `$UpCase` is written when a volume is formatted, so one formatted by a much
/// older Windows can still differ on a letter added since. A lookup in memory, no
/// filesystem call: the tab title's `~` folds on every hook.
fn fold(units: &[u16]) -> Vec<u16> {
    let up = |u: u16| match u8::try_from(u) {
        Ok(b) if b.is_ascii() => b.to_ascii_uppercase().into(),
        // SAFETY: takes and returns a plain value; it reads only the system's
        // in-memory case table.
        _ => unsafe { RtlUpcaseUnicodeChar(u) },
    };
    units.iter().map(|&u| up(u)).collect()
}

/// The key of a volume or share prefix: `C:` for a drive, `\\SERVER\SHARE` for a
/// share whatever its separators or `\\?\` form, and any other prefix (`\\.\pipe`,
/// `\\?\GLOBALROOT`) folded as it is spelled.
///
/// The SERVER is folded in ASCII only, every other unit kept exact: it names a host,
/// and [`same_root`] lets a gitdir through on its say-so - so a name that merely
/// looks like the session's own host, `\\fıleserver` for `\\fileserver`, must be a
/// different root, refused before anything opens it, whatever a case table says.
fn prefix_key(q: &std::path::PrefixComponent) -> Vec<u16> {
    match q.kind() {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => vec![d.to_ascii_uppercase().into(), b':'.into()],
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let mut k = vec![b'\\'.into(), b'\\'.into()];
            k.extend(server.encode_wide().map(|u| match u8::try_from(u) {
                Ok(b) => b.to_ascii_uppercase().into(),
                Err(_) => u,
            }));
            k.push(b'\\'.into());
            k.extend(fold(&share.encode_wide().collect::<Vec<_>>()));
            k
        }
        _ => fold(&q.as_os_str().encode_wide().collect::<Vec<_>>()),
    }
}

/// `p` as its SPELLING compares, with no filesystem call: one key per component - the
/// prefix's ([`prefix_key`]), `\` for the root, and each name [`fold`]ed - paired
/// with the offset in `p`'s UTF-16 units where that component ends. `/` and `\` are
/// both separators, an empty or `.` component is skipped, and `..` is kept as it is.
///
/// This is the one comparison under [`same_path`], [`is_within`], [`same_root`] and
/// [`strip_home_prefix`]: the first two apply it after [`normalize`], the `~` of a
/// tab title to the spelling it has, so all of them agree on what "the same" means.
fn lexical(p: &Path) -> Vec<(Vec<u16>, usize)> {
    let w = wide(p);
    let sep = |u: &u16| *u == u16::from(b'\\') || *u == u16::from(b'/');
    let mut out = Vec::new();
    let mut at = 0;
    if let Some(Component::Prefix(q)) = p.components().next() {
        at = q.as_os_str().encode_wide().count();
        out.push((prefix_key(&q), at));
    }
    if w.get(at).is_some_and(sep) {
        at += 1;
        out.push((vec![b'\\'.into()], at));
    }
    for name in w[at..].split(sep) {
        let end = at + name.len();
        if !(name.is_empty() || name == [u16::from(b'.')]) {
            out.push((fold(name), end));
        }
        at = end + 1;
    }
    out
}

/// The keys of [`lexical`] alone.
fn folded(p: &Path) -> Vec<Vec<u16>> {
    lexical(p).into_iter().map(|(k, _)| k).collect()
}

/// What follows `home` in `path`, when `path` is `home` or lies beneath it: empty for
/// `home` itself, else the rest from its separator on, spelled as `path` spells it.
///
/// Judged by [`lexical`] on the spelling alone, because the tab title asks it on
/// every hook: `c:\users\me\x`, `C:/Users/ME/x` and `\\?\C:\Users\me\x` are all under
/// `C:\Users\me`. What only the disk could tell is NOT seen - an 8.3 short name
/// (`C:\Users\ALEXAN~1`) or a junction is a different spelling, and keeps its path.
///
/// A home that names no directory IN a volume - `C:\`, the `C:` that `config` trims
/// it to, `\\srv\share` - claims only itself, as `/` does on Unix: `C:\code` stays
/// `C:/code`, not `~/code`.
pub fn strip_home_prefix(path: &Path, home: &Path) -> Option<OsString> {
    let (p, h) = (lexical(path), lexical(home));
    let n = h.len();
    if n == 0 || p.len() < n || p.iter().zip(&h).any(|((a, _), (b, _))| a != b) {
        return None;
    }
    if !home.components().any(|c| matches!(c, Component::Normal(_) | Component::ParentDir)) {
        return (p.len() == n).then(OsString::new);
    }
    // `home` ends in a name, so the rest is empty or starts at the separator after it.
    Some(OsString::from_wide(&wide(path)[p[n - 1].1..]))
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

/// Whether a resolved gitdir, `GIT_DIR` or `HEAD` path at `p` may be handed to the
/// filesystem call that probes it, given `base` - the already-trusted directory it
/// was derived from (the walk's directory, the gitfile's own directory, or the
/// logical cwd for `GIT_DIR`).
///
/// This is the whole of the UNC/NTLM defence. A path that names a NETWORK SHARE -
/// `\\server\share\...`, `//server/share/...`, `\\?\UNC\...`, or a drive letter
/// mapped to a share - makes the first `exists`/`is_file`/`open` open an SMB
/// connection and hand the server an automatic NTLM authentication, which leaks the
/// user's hashed credentials off-box and stalls the hook for seconds; a DEVICE path
/// (`\\.\pipe\...`, `\\?\GLOBALROOT\...`) reaches a device object instead. A hostile
/// repository plants exactly such a path - in a `.git` gitfile's `gitdir:` line, in
/// `GIT_DIR`, or as a reparse point (junction or symlink) standing in for `.git` or
/// `HEAD`. On a refusal the caller yields no repository, so the tab falls back to the
/// plain path, exactly as for a directory that is not a repository.
///
/// ALLOWED: a local fixed/removable drive-letter path, OR any path that shares
/// `base`'s own volume or share - so a repository a user has deliberately opened on
/// `\\server\share` or on a mapped drive keeps painting `repo@branch`, as long as its
/// gitdir stays on that same root. REFUSED: every other UNC path, every device path,
/// and a mapped network drive (`DRIVE_REMOTE`) that points off `base`'s root.
///
/// The literal spelling is judged FIRST, with no system call, so a UNC or device
/// string is rejected before anything can touch it. Only once the spelling is local
/// is `p` stat-ed - a no-follow `symlink_metadata`, which cannot reach the network -
/// and if it is a reparse point its WHOLE chain is walked with `read_link` (which
/// does not follow a link either), every hop judged the same way. So a `.git` or
/// `HEAD` junction to a share - even behind a local junction that fronts for it - is
/// refused on its target, while a chain that stays local is followed.
pub fn gitpath_allowed(p: &Path, base: &Path) -> bool {
    if !spelling_local(p, base) {
        return false;
    }
    // Walk a reparse chain WITHOUT ever following it on the network. Each hop is read
    // with the no-follow `read_link` and its target judged by the same local-spelling
    // rule, so a chain that ends at - or merely passes through - a share is refused
    // before any following call (`exists`/`is_file`/`open`) can reach it: a single
    // local junction placed in front of a symlink-to-share would otherwise sail past
    // a one-hop check. The cap stops a cyclic or adversarially deep chain.
    let mut cur = p.to_path_buf();
    for _ in 0..MAX_REPARSE_HOPS {
        match fs::symlink_metadata(&cur) {
            // `cur`'s spelling is already known local, so this no-follow stat stays
            // on-box; a reparse point is judged on where it points, never by following.
            Ok(m) if m.file_type().is_symlink() => {
                let target = match fs::read_link(&cur) {
                    Ok(t) => t,
                    Err(_) => return false,
                };
                // A relative target resolves against the link's own directory.
                cur = if target.is_absolute() {
                    target
                } else {
                    cur.parent().unwrap_or(Path::new(".")).join(target)
                };
                if !spelling_local(&cur, base) {
                    return false;
                }
            }
            // Absent (the caller's own `exists`/`is_file` handles that) or a plain
            // file or directory: the chain ends on a local volume.
            _ => return true,
        }
    }
    false
}

/// How many reparse hops the guard follows before giving up. A `.git` or `HEAD` is a
/// link or two in practice; a longer chain is cyclic or adversarial and is refused
/// rather than followed onto who-knows-where.
const MAX_REPARSE_HOPS: usize = 40;

/// Whether `p`'s spelling names a local volume, or `base`'s own volume or share. No
/// system call but [`drive_type`], which only classifies a drive letter.
fn spelling_local(p: &Path, base: &Path) -> bool {
    let p = strip_verbatim(p);
    match p.components().next() {
        Some(Component::Prefix(q)) => match q.kind() {
            // A fixed or removable drive is local; a mapped network drive counts as
            // network unless it is `base`'s own root.
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                drive_type(d) != DRIVE_REMOTE || same_root(&p, base)
            }
            // A UNC share is allowed only when it is the one `base` lives on.
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => same_root(&p, base),
            // `\\.\device`, `\\?\GLOBALROOT\...` and the like: never a repository.
            _ => false,
        },
        // No prefix - a relative path, or one rooted on the current drive like
        // `\x` - resolves on the local cwd volume.
        _ => true,
    }
}

/// Whether `candidate`'s volume or share is the same as `base`'s, compared by
/// [`prefix_key`] - the server in ASCII case only.
///
/// `candidate` is read from its SPELLING alone and is never opened - it may be the
/// hostile path. `base` is the trusted cwd-derived directory, so when it sits on a
/// drive mapped to a network share its letter is resolved to that share; this opens
/// only the share the session is already on, and it lets a repository reached through
/// `\\server\share` directly, or through a drive mapped to it, match a gitdir git
/// spelled the other way - git records a worktree's gitdir as `//server/share/...`,
/// which never string-matches a `\\server\share` or `Y:` base.
fn same_root(candidate: &Path, base: &Path) -> bool {
    let c = match root_prefix(candidate) {
        Some(c) => c,
        None => return false,
    };
    if root_prefix(base).is_some_and(|b| b == c) {
        return true;
    }
    // `base` on a mapped network drive: compare against the share it resolves to. Only
    // `base` is canonicalized (the share the session already sits on); the hostile
    // `candidate` is never opened, so a second drive mapped elsewhere to the same share
    // is not matched - a documented corner, not a hole.
    if base_on_remote_disk(base) {
        if let Some(b) = fs::canonicalize(base).ok().and_then(|r| root_prefix(&r)) {
            return b == c;
        }
    }
    false
}

/// The volume-or-share prefix of `p` as [`prefix_key`] compares it - `C:`, or
/// `\\SERVER\SHARE` whatever its separators (`\\` or `//`) or verbatim form, built
/// from the parsed server and share - or `None` when `p` has no drive or share
/// prefix. Read from the spelling.
fn root_prefix(p: &Path) -> Option<Vec<u16>> {
    match p.components().next() {
        Some(Component::Prefix(q)) => match q.kind() {
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..) => {
                Some(prefix_key(&q))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Whether `base` sits on a drive letter mapped to a network share.
fn base_on_remote_disk(base: &Path) -> bool {
    match strip_verbatim(base).components().next() {
        Some(Component::Prefix(q)) => match q.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => drive_type(d) == DRIVE_REMOTE,
            _ => false,
        },
        _ => false,
    }
}

/// The command a report hands an operator to remove the directory `p` outright: a
/// PowerShell one, the path single-quoted - a profile path with a space in it is the
/// ordinary case here, and `rm -rf` is a parameter error in PowerShell.
///
/// Every character PowerShell reads as a single quote is doubled, not only `'`: its
/// tokenizer also ends a single-quoted string at U+2018, U+2019, U+201A and U+201B.
/// A directory named `it’s` - the apostrophe a phone or a word processor types -
/// otherwise closed the string early, and `a’,’C:\Users\me’,’b` turned one path into
/// an array of three, the middle one somebody's whole profile.
pub fn remove_dir_command(p: &Path) -> String {
    let mut quoted = String::new();
    for c in p.display().to_string().chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    format!("Remove-Item -Recurse -Force -LiteralPath '{}'", quoted)
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

// SAFETY: a process handle names a kernel object, not thread-local state: any thread
// of this process may use it, and `Process` owns it and closes it exactly once, as
// `OwnedHandle` (which is `Send`) does. It moves to the console worker inside a
// [`Claude`].
unsafe impl Send for Process {}

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
/// and any "no" paints nothing. All three, and the attach, are about ONE process:
/// once the walk has proven `claude_pid` an ancestor, it is opened as a [`Claude`],
/// and that handle - kept only if its process was created when the walk's was - is
/// what the PEB is read through and is held until the attach is over, so the pid
/// cannot be reissued in between, however soon Claude exits.
///   1. `claude_pid` is a LIVE ANCESTOR of this process: the parent chain is walked
///      up from here, each hop running and created no later than its child, so a
///      stale or recycled pid, a sibling, and every unrelated process are refused.
///      Only then is it opened for more than a query, as before.
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
    let Some(claude) = parse_pid(claude_pid).and_then(live_ancestor) else { return Ok(false) };
    let Some(stdout) = char_stdout_of(&claude.process) else { return Ok(false) };
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    // `claude` moves into the worker and is dropped there, after `title_console` has
    // returned: past AttachConsole, the screen-buffer check and the detach, abandoned
    // or not. An abandoned worker still owns it, so nothing closes it under a call in
    // flight, and the handle lives no longer than that call or this process.
    on_console_worker(move |abandoned| title_console(&claude, &stdout, &title, abandoned))
}

/// `$CLAUDE_PID`, opened ONCE with every right [`set_session_title`] needs of it, after
/// the ancestor walk and only if its process is the one the walk proved. Windows does
/// not reissue a pid while a handle to its process is open, so for as long as this
/// lives the one step that can only name the process by number - AttachConsole -
/// names this one, even if Claude has exited since: a pid freed between the checks and
/// the attach, and handed to another process's console, is what holding it rules out.
struct Claude {
    pid: u32,
    process: Process,
}

impl Claude {
    /// The process `pid`, still RUNNING and created at `created` - the creation time
    /// the walk read for that pid from a handle since closed - or nothing. A pid
    /// reissued in between names a process created later, and is refused; so is one
    /// that refuses the rights to read its PEB and duplicate its stdout, which guard 2
    /// needs anyway.
    fn open(pid: u32, created: u64) -> Option<Claude> {
        let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE;
        let process = open_process(pid, access)?;
        match probe_open(&process) {
            Probe::Running(born) if born == created => Some(Claude { pid, process }),
            _ => None,
        }
    }
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
/// AttachConsole takes a pid; `claude`, open for the whole call, keeps it Claude's.
fn title_console(
    claude: &Claude,
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
        if !live() || AttachConsole(claude.pid) == 0 {
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

/// `pid` opened as [`Claude`], when it is a live ancestor of this process within
/// [`ANCESTOR_HOPS`]. A hop whose recorded parent is YOUNGER than it has outlived that
/// parent, whose pid went to a newer process: nothing above it is ours, and the walk
/// stops there.
///
/// The hops are opened by pid for a query alone, so nothing but a proven ancestor is
/// opened for more. The handle that is kept must name the process the walk matched,
/// created at the same time: the same process, not merely the same number.
fn live_ancestor(pid: u32) -> Option<Claude> {
    let (mut born, mut parent) = hop(std::process::id())?;
    for _ in 0..ANCESTOR_HOPS {
        let (parent_born, grandparent) = hop(parent)?;
        if parent_born > born {
            return None;
        }
        if parent == pid {
            return Claude::open(pid, parent_born);
        }
        (born, parent) = (parent_born, grandparent);
    }
    None
}

/// A RUNNING process's creation time and the pid that created it, from one handle,
/// so the answer cannot mix two processes.
fn hop(pid: u32) -> Option<(u64, u32)> {
    let p = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let Probe::Running(created) = probe_open(&p) else { return None };
    Some((created, u32::try_from(basic_info(&p)?.parent).ok()?))
}

/// The standard output of process `p`, duplicated into this process, when it is a
/// character device - the Windows `/proc/<pid>/fd/1`. `p` is open for
/// `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE`, as a
/// [`Claude`] is. The PEB offsets are the 64-bit layout
/// (`PEB.ProcessParameters` at 0x20, and `StandardOutput` at 0x28 in that), stable
/// since NT; a WOW64 process keeps its current handles in its 32-bit copy, and a
/// 32-bit build cannot use these offsets, so both are refused.
fn char_stdout_of(p: &Process) -> Option<OwnedHandle> {
    if !cfg!(target_pointer_width = "64") {
        return None;
    }
    let mut wow = 0;
    // SAFETY: an open process handle and a live BOOL.
    if unsafe { IsWow64Process(p.0, &mut wow) } == 0 || wow != 0 {
        return None;
    }
    let params = read_word(p, basic_info(p)?.peb.checked_add(0x20)?)?;
    let handle = read_word(p, params.checked_add(0x28)?)?;
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
pub fn write_tty(_path: &Path, _bytes: &[u8]) -> io::Result<bool> {
    Ok(false)
}

/// A file's DACL read and set through the Win32 named-object calls - not through
/// [`security_of`] and [`create_secured`], which the tests using these are grading.
#[cfg(test)]
pub(crate) mod test_acl {
    use super::wide_nul;
    use std::path::Path;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    /// `p`'s DACL as SDDL (`D:PAI(...)...`): the flags and every entry, in order.
    pub fn sddl(p: &Path) -> String {
        let name = wide_nul(p);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let (owner, group, dacl, sacl) = (null_mut(), null_mut(), null_mut(), null_mut());
        // SAFETY: `name` is NUL-terminated; only the descriptor is asked for, and the
        // call allocates it with LocalAlloc, freed below.
        let rc = unsafe {
            GetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, owner, group, dacl, sacl, &mut sd)
        };
        assert_eq!(rc, 0, "GetNamedSecurityInfoW {}", p.display());
        let (mut s, mut n) = (std::ptr::null_mut::<u16>(), 0u32);
        // SAFETY: `sd` is the descriptor just returned; `s` receives a LocalAlloc'd
        // string of `n` units, freed below.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, &mut n)
        };
        assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW");
        // SAFETY: `s` points at `n` initialised units, the trailing NUL among them.
        let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, n as usize) });
        // SAFETY: both were allocated by the calls above and are not used again.
        unsafe {
            LocalFree(s.cast());
            LocalFree(sd);
        }
        out.trim_end_matches('\0').to_string()
    }

    /// Set `p`'s DACL from SDDL the way an ACL editor does: a `D:P` string protects
    /// it, anything else leaves it inheriting, and the directory's entries are merged
    /// in by the call.
    pub fn set_sddl(p: &Path, text: &str) {
        let w: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `w` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor,
        // freed below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut())
        };
        assert_ne!(ok, 0, "bad SDDL {text}");
        let (mut present, mut dacl, mut defaulted) = (0, std::ptr::null_mut::<ACL>(), 0);
        let (mut ctl, mut rev) = (0u16, 0u32);
        let name = wide_nul(p);
        // SAFETY: `sd` is valid until freed at the end; `dacl` points into it and is
        // only passed to SetNamedSecurityInfoW, which copies it.
        let rc = unsafe {
            GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
            GetSecurityDescriptorControl(sd, &mut ctl, &mut rev);
            let how = if ctl & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
            let none = std::ptr::null_mut();
            let rc = SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION | how, none, none, dacl, std::ptr::null());
            LocalFree(sd);
            rc
        };
        assert_eq!(rc, 0, "SetNamedSecurityInfoW {} {text}", p.display());
    }

    /// What someone hardening settings.json by hand does: inheritance off with the
    /// inherited entries kept as explicit ones (`icacls /inheritance:d`),
    /// Administrators removed, and an explicit deny of read to Guests (S-1-5-32-546).
    pub fn harden(p: &Path) {
        let now = sddl(p);
        let aces = now.find('(').map_or("", |i| &now[i..]);
        let mut out = String::from("D:P(D;;FR;;;BG)");
        for ace in aces.trim_start_matches('(').trim_end_matches(')').split(")(").filter(|a| !a.is_empty()) {
            let mut f: Vec<String> = ace.split(';').map(str::to_string).collect();
            if f.last().map(String::as_str) == Some("BA") {
                continue;
            }
            f[1] = f[1].replace("ID", "");
            out.push_str(&format!("({})", f.join(";")));
        }
        set_sddl(p, &out);
    }
}

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

    /// The temp file a rewrite makes is BORN private - a protected DACL granting its
    /// owner alone, so between its creation and the DACL set no one else may open it,
    /// not even for `READ_CONTROL` or `WRITE_DAC`, which share mode 0 does not bar -
    /// and the creating handle writes it and sets its security. Then it has the
    /// original's DACL before a byte is written, and nothing else can open it for its
    /// data, before or after. Nothing there: `None`. A long path works as through std.
    #[test]
    fn a_secured_file_is_born_private_and_then_takes_the_dacl() {
        let d = scratch("secured");
        let orig = d.join("settings.json");
        fs::write(&orig, b"{}").expect("write");
        test_acl::harden(&orig);
        let acl = test_acl::sddl(&orig);
        assert!(acl.starts_with("D:P") && acl.contains("(D;;FR;;;BG)"), "{acl}");
        let sec = security_of(&orig).expect("readable").expect("there");

        let born = d.join(".born");
        let f = create_private(&born).expect("made");
        assert_eq!(test_acl::sddl(&born), "D:P(A;;FA;;;OW)", "its owner alone until the DACL is set");
        let other = OpenOptions::new().read(true).open(&born).expect_err("not shared");
        assert_eq!(other.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        set_security(&f, DACL_SECURITY_INFORMATION, &sec).expect("the creator may set it");
        let other = OpenOptions::new().read(true).open(&born).expect_err("still not shared");
        assert_eq!(other.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        let mut w = &f;
        io::Write::write_all(&mut w, b"x").expect("the creator may write");
        drop(f);
        assert_eq!(test_acl::sddl(&born), acl);
        assert_eq!(create_private(&born).expect_err("never replaces").kind(), io::ErrorKind::AlreadyExists);

        let tmp = d.join(".settings.json.cctab-tmp.1");
        let f = create_secured(&tmp, &sec).expect("made");
        assert_eq!(test_acl::sddl(&tmp), acl, "before a byte is written");
        let other = OpenOptions::new().read(true).open(&tmp).expect_err("not shared");
        assert_eq!(other.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
        drop(f);
        assert!(security_of(&d.join("absent")).expect("no error").is_none());

        let deep = d.join("a".repeat(120)).join("b".repeat(120));
        fs::create_dir_all(&deep).expect("mkdir long");
        let long = deep.join("settings.json");
        assert!(long.as_os_str().len() > 260, "past MAX_PATH");
        drop(create_secured(&long, &sec).expect("a long path"));
        assert_eq!(fs::metadata(&long).expect("there").len(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    /// A file on a filesystem with no Windows ACL - the WSL 9P share - is told apart
    /// from one whose ACL this user may not read. Needs such a file, so run by hand:
    /// `CCTAB_TEST_NO_ACL_FILE=\\wsl.localhost\<distro>\tmp\<dir>\f cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a file on a WSL share, named by CCTAB_TEST_NO_ACL_FILE"]
    fn a_file_on_a_share_with_no_acl_is_unsupported_not_unreadable() {
        let p = PathBuf::from(std::env::var_os("CCTAB_TEST_NO_ACL_FILE").expect("CCTAB_TEST_NO_ACL_FILE"));
        fs::read(&p).expect("its data is readable");
        let e = security_of(&p).err().expect("no ACL to read");
        assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{e}");
    }

    /// The spelling a raw Win32 call gets for a long path.
    #[test]
    fn a_long_path_is_spelled_verbatim_for_win32() {
        let v = |s: &str| verbatim(Path::new(s)).map(|o| o.to_string_lossy().into_owned());
        assert_eq!(v(r"C:\a/b\.\c\..\d").as_deref(), Some(r"\\?\C:\a\b\d"));
        assert_eq!(v(r"\\srv\share\x\y").as_deref(), Some(r"\\?\UNC\srv\share\x\y"));
        assert_eq!(v(r"a\b"), None);
        assert_eq!(v(r"C:a"), None);
        assert_eq!(v(r"\\?\C:\a"), None);
        // Short: exactly what it always was.
        assert_eq!(wide_nul_long(Path::new(r"C:\a")), wide_nul(Path::new(r"C:\a")));
    }

    /// A junction made where the path to it is past MAX_PATH: std makes the scratch
    /// directory, and the no-replace rename into place has to reach it as well.
    #[test]
    fn a_junction_lands_at_a_path_past_max_path() {
        let d = scratch("longjunction");
        let target = d.join("target");
        fs::create_dir_all(&target).expect("mkdir");
        let mut deep = d.clone();
        while deep.as_os_str().len() < 300 {
            deep.push("a-directory-name-of-some-length");
        }
        fs::create_dir_all(&deep).expect("mkdir");
        let link = deep.join("claude-tabstatus");
        link_dir(&target, &link).expect("junction past MAX_PATH");
        assert!(fs::symlink_metadata(&link).expect("lstat").file_type().is_symlink());
        fs::write(target.join("probe"), b"x").expect("write");
        assert_eq!(fs::read(link.join("probe")).expect("through the junction"), b"x");
        remove_dir_link(&link).expect("unlink");
        assert!(target.is_dir(), "the target is untouched");
        let _ = fs::remove_dir_all(&d);
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

    /// Guard 1. The process that started this test is an ancestor, and is what is
    /// held; this process, a child it spawned - running, then exited - and a pid
    /// nothing has are not. Nothing here touches a console.
    #[test]
    fn only_a_running_ancestor_passes_the_walk() {
        let me = std::process::id();
        let (_, parent) = hop(me).expect("this process");
        let held = live_ancestor(parent).map(|c| c.pid);
        assert_eq!(held, Some(parent), "the process that started this test");
        assert!(live_ancestor(me).is_none(), "not its own ancestor");
        assert!(live_ancestor(u32::MAX).is_none(), "no such pid");
        let mut child = held_child(Stdio::null());
        let pid = child.id();
        let (created, _) = hop(pid).expect("the child, running");
        assert!(live_ancestor(pid).is_none(), "a child is not an ancestor");
        assert!(Claude::open(pid, created).is_some(), "the child can be opened while it runs");
        drop(child.stdin.take());
        child.wait().expect("wait");
        // `child` still holds its handle, so the pid is not reissued: it is the exited
        // process itself that the open refuses, at its own creation time.
        assert!(Claude::open(pid, created).is_none(), "an exited process is not held");
        assert!(live_ancestor(pid).is_none(), "nor once it has exited");
    }

    /// The handle kept is the process the walk matched, not merely its pid: opened
    /// with another creation time than the walk read - what a pid reissued between the
    /// walk and the open would show - it is refused, and with that one accepted. The
    /// ancestor is only opened and queried; no console is touched.
    #[test]
    fn a_pid_created_at_another_time_than_the_walk_read_is_not_held() {
        let (_, parent) = hop(std::process::id()).expect("this process");
        let (created, _) = hop(parent).expect("the process that started this test");
        assert!(Claude::open(parent, created).is_some(), "the process the walk read");
        // `hop` never reads a creation time of 0, so neither neighbour wraps.
        for forged in [created - 1, created + 1] {
            assert!(Claude::open(parent, forged).is_none(), "created {forged}, the walk {created}");
        }
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
            let pid = child.id();
            let held = hop(pid).and_then(|(created, _)| Claude::open(pid, created));
            assert_eq!(held.and_then(|c| char_stdout_of(&c.process)).is_some(), char_device, "{what}");
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

    /// `head` followed by `units`, which may hold an unpaired surrogate.
    fn wide_path(head: &str, units: &[u16]) -> PathBuf {
        let mut w: Vec<u16> = head.encode_utf16().collect();
        w.extend_from_slice(units);
        PathBuf::from(OsString::from_wide(&w))
    }

    /// An unpaired surrogate shows as one U+FFFD, as `to_string_lossy` shows it; a
    /// surrogate PAIR, being a real character, and plain UTF-8 pass untouched.
    #[test]
    fn a_surrogate_displays_as_one_replacement_character() {
        for units in [&[0x61, 0xD800, 0x5C, 0xDFFF, 0xDBFF][..], &[0xD801, 0xDC28, 0xDC00], &[0x41, 0xE9]] {
            let os = OsString::from_wide(units);
            let shown = display_bytes(os.as_encoded_bytes());
            assert_eq!(std::str::from_utf8(&shown).expect("utf-8"), os.to_string_lossy());
        }
    }

    /// Bytes sliced out of an `OsStr` come back as the units they were, an unpaired
    /// surrogate included; bytes no `OsStr` holds become U+FFFD, one per sequence.
    #[test]
    fn wtf8_bytes_round_trip_and_anything_else_is_a_replacement_character() {
        for units in [&[0x61, 0xD800, 0x62][..], &[0xDFFF], &[0xD801, 0x5C], &[0xFFFD, 0xDC00]] {
            let os = OsString::from_wide(units);
            let back = os_str_from_bytes(os.as_encoded_bytes()).into_owned();
            assert_eq!(back.encode_wide().collect::<Vec<_>>(), units);
            assert_eq!(os_string_from_vec(os.as_encoded_bytes().to_vec()), os);
        }
        assert_eq!(
            os_str_from_bytes(b"a\xffb\xf0\x9f\x98x\xed\x9f"),
            OsStr::new("a\u{fffd}b\u{fffd}x\u{fffd}")
        );
    }

    /// The comparison key is the UTF-16 units:`<D800>`, `<D801>` and U+FFFD - one
    /// string to a lossy decode - stay three, while case, separators, `\\?\` and empty
    /// or `.` components still do not count.
    #[test]
    fn the_comparison_key_keeps_an_unpaired_surrogate_and_folds_case() {
        let (d800, d801, fffd) = (
            wide_path(r"C:\x\a", &[0xD800]),
            wide_path(r"C:\x\a", &[0xD801]),
            wide_path(r"C:\x\a", &[0xFFFD]),
        );
        assert_eq!(d800.to_string_lossy(), fffd.to_string_lossy(), "what the lossy fold compared");
        assert_ne!(folded(&d800), folded(&d801));
        assert_ne!(folded(&d800), folded(&fffd));
        assert_ne!(folded(&d801), folded(&fffd));
        assert_eq!(folded(&d800), folded(&wide_path(r"c:/X/A", &[0xD800])), "case around a surrogate");
        let plain = folded(Path::new(r"C:\Users\Me\x"));
        for v in [r"c:\USERS\me\x", "C:/Users/Me/x", r"\\?\C:\Users\me\x", r"C:\Users\\me\.\x\"] {
            assert_eq!(folded(Path::new(v)), plain, "{v}");
        }
        assert_ne!(folded(Path::new(r"C:\Users\Me2\x")), plain);
        // One unit to one unit, as `$UpCase` maps: `é` folds, `ß` has no one-letter
        // upper case, and a character beyond the BMP is left alone.
        assert_eq!(folded(Path::new("C:\\\u{e9}")), folded(Path::new("C:\\\u{c9}")));
        assert_ne!(folded(Path::new("C:\\stra\u{df}e")), folded(Path::new(r"C:\STRASSE")));
        assert_ne!(folded(Path::new("C:\\\u{10428}")), folded(Path::new("C:\\\u{10400}")));
        // What NTFS keeps apart stays apart, though Unicode upper-cases one onto the
        // other: dotless `ı` and long `ſ` are not `I` and `S`, final `ς` is not `Σ`.
        let k = |s: &str| fold(&s.encode_utf16().collect::<Vec<_>>());
        for (a, b) in [("\u{131}", "I"), ("\u{131}", "i"), ("\u{17f}", "S"), ("\u{3c2}", "\u{3a3}")] {
            assert_ne!(k(a), k(b), "{a:?} {b:?}");
        }
        assert_eq!(k("\u{3c3}"), k("\u{3a3}"));
        assert_eq!(k("\u{1fb3}"), k("\u{1fbc}"), "two letters to Unicode, one unit to NTFS");
        // Share prefixes, whatever their form, by server and share.
        assert_eq!(folded(Path::new(r"\\?\UNC\h\s\x")), folded(Path::new("//H/S/x")));
    }

    /// Real directories whose names differ only by a surrogate are different
    /// directories to `same_path` and `is_within`, and each still matches its own
    /// case and `\\?\` spellings.
    #[test]
    fn names_that_differ_only_by_a_surrogate_are_different_directories() {
        let d = normalize(&scratch("surrogate"));
        let named = |units: &[u16]| d.join(OsString::from_wide(units));
        let (a, b, r) = (named(&[0x61, 0xD800]), named(&[0x61, 0xD801]), named(&[0x61, 0xFFFD]));
        for x in [&a, &b, &r] {
            fs::create_dir(x).expect("mkdir");
        }
        assert_eq!(fs::read_dir(&d).expect("list").count(), 3, "three names on disk");
        assert!(!same_path(&a, &b) && !same_path(&a, &r) && !same_path(&b, &r));
        let upper = named(&[0x41, 0xD800]);
        let mut verbatim = OsString::from(r"\\?\");
        verbatim.push(&a);
        for v in [&a, &upper, Path::new(&verbatim)] {
            assert!(same_path(v, &a), "{}", v.display());
            assert!(is_within(&v.join("new"), &a), "{}", v.display());
        }
        assert!(!is_within(&b.join("x"), &a) && !is_within(&r.join("x"), &a));
        assert!(!is_within(&a.join("x"), &r));
        let _ = fs::remove_dir_all(&d);
    }

    /// The gitdir guard's root compare is lossless too, and touches nothing on a share.
    #[test]
    fn a_share_named_with_a_surrogate_is_its_own_root() {
        let share = |units: &[u16]| wide_path(r"\\cctab-nohost\s", units);
        let own = share(&[0xD800]);
        assert!(same_root(&own.join("r"), &own));
        assert!(same_root(&wide_path(r"//CCTAB-NOHOST/S", &[0xD800]).join("r"), &own), "case + /");
        assert!(same_root(&wide_path(r"\\?\UNC\cctab-nohost\s", &[0xD800]), &own), "verbatim");
        assert!(!same_root(&share(&[0xD801]), &own));
        assert!(!same_root(&share(&[0xFFFD]), &own));
        assert!(!spelling_local(&share(&[0xFFFD]).join(".git"), &own));
    }

    /// A host is the same host only up to ASCII case: a name that merely looks like
    /// the session's own server is another root, so its gitdir is refused unopened.
    #[test]
    fn a_lookalike_server_is_another_root() {
        let base = Path::new(r"\\cctab-nohost\proj\repo");
        assert!(same_root(Path::new("//CCTAB-NOHOST/Proj/x/.git"), base));
        assert!(same_root(Path::new(r"\\?\UNC\cctab-nohost\PROJ\x"), base));
        for p in [
            "\\\\cctab-noh\u{131}st\\proj\\x\\.git",
            "//cctab-no\u{17f}t/proj/x/.git",
            "\\\\?\\UNC\\cctab-noh\u{131}st\\proj\\x",
        ] {
            assert!(!same_root(Path::new(p), base), "{p}");
            assert!(!spelling_local(Path::new(p), base), "{p}");
        }
        // Beyond ASCII a host matches exactly, case and all; the share, on the one
        // host already trusted, still folds.
        let accented = Path::new("\\\\serv\u{e9}r\\\u{e9}t\u{e9}\\repo");
        assert!(same_root(Path::new("\\\\SERV\u{e9}R\\\u{c9}T\u{c9}\\x"), accented));
        assert!(!same_root(Path::new("\\\\SERV\u{c9}R\\\u{e9}t\u{e9}\\x"), accented));
    }

    /// The fold agrees with the volume the tests run on, measured with real
    /// directories: a name created one way is found the other way exactly when the
    /// two fold alike - ASCII, the letters a Unicode fold would merge but NTFS does
    /// not, and the one-unit fold Unicode spells as two letters.
    #[test]
    fn the_fold_agrees_with_the_volume() {
        let d = scratch("fold");
        let pairs = [
            ("a", "A"), ("\u{e9}", "\u{c9}"), ("\u{131}", "I"), ("\u{17f}", "S"),
            ("\u{3c2}", "\u{3a3}"), ("\u{3c3}", "\u{3a3}"), ("\u{b5}", "\u{39c}"),
            ("\u{1c5}", "\u{1c4}"), ("\u{1fb3}", "\u{1fbc}"), ("\u{10428}", "\u{10400}"),
        ];
        for (i, (a, b)) in pairs.iter().enumerate() {
            let (x, y) = (d.join(format!("{i}{a}")), d.join(format!("{i}{b}")));
            fs::create_dir(&x).expect("mkdir");
            assert_eq!(y.exists(), folded(&x) == folded(&y), "{a:?} {b:?}");
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// HOME is recognised by its spelling alone, and what follows it keeps the path's.
    #[test]
    fn home_is_recognised_by_spelling_alone() {
        let s = |p: &str, h: &str| {
            strip_home_prefix(Path::new(p), Path::new(h)).map(|r| r.into_string().expect("utf-8"))
        };
        let home = r"C:\Users\Me";
        assert_eq!(s(home, home).as_deref(), Some(""));
        assert_eq!(s(r"c:\users\me\Code\x", home).as_deref(), Some(r"\Code\x"));
        assert_eq!(s(r"\\?\C:\Users\Me\Code", home).as_deref(), Some(r"\Code"));
        assert_eq!(s("C:/USERS/ME/Code", home).as_deref(), Some("/Code"));
        assert_eq!(s(r"C:\Users\Me\\Code\", home).as_deref(), Some(r"\\Code\"), "spelled as is");
        assert_eq!(s(r"C:\Users\Me\x", r"c:/users/me/").as_deref(), Some(r"\x"));
        assert_eq!(s(r"C:\", r"C:\").as_deref(), Some(""));
        assert_eq!(s("c:/", r"C:\").as_deref(), Some(""));
        // Not under HOME: a sibling, the parent, another drive, a bare-root home's
        // children (as `/` on Unix), no home at all - and an 8.3 short name, which
        // only the disk could expand.
        for (p, h) in [
            (r"C:\Users\Me2\x", home),
            (r"C:\Users", home),
            (r"D:\Users\Me", home),
            (r"C:\x", r"C:\"),
            (r"C:\x", "C:"),
            (r"C:\", "C:"),
            (r"\\srv\share\x", r"\\srv\share"),
            (r"C:\x", ""),
            (r"C:\Users\ALEXAN~1\x", r"C:\Users\Alexandre Flament"),
        ] {
            assert_eq!(s(p, h), None, "{p} under {h}");
        }
    }

    #[test]
    fn the_removal_hint_is_a_powershell_command_with_the_path_quoted() {
        assert_eq!(
            remove_dir_command(Path::new(r"C:\Users\A B\it's")),
            r"Remove-Item -Recurse -Force -LiteralPath 'C:\Users\A B\it''s'"
        );
        // ...and so is every typographic quote PowerShell also ends that string at.
        assert_eq!(
            remove_dir_command(Path::new("C:\\x\\a\u{2019},\u{2018}C:\\Users\\me\u{201A},\u{201B}b")),
            "Remove-Item -Recurse -Force -LiteralPath \
             'C:\\x\\a\u{2019}\u{2019},\u{2018}\u{2018}C:\\Users\\me\u{201A}\u{201A},\u{201B}\u{201B}b'"
        );
    }

    /// The spelling half of the gitdir guard, judged on strings alone - no disk and,
    /// crucially, no network, because a UNC or device spelling is refused before any
    /// call could reach it. Every vector a hostile repository can spell is here.
    #[test]
    fn a_network_or_device_spelling_is_refused_and_a_local_one_allowed() {
        let local = Path::new(r"C:\repo");
        // Every UNC spelling, against a LOCAL base: refused.
        for p in [
            r"\\cctab-nohost\share\x",
            "//cctab-nohost/share/x",
            r"\\?\UNC\cctab-nohost\share\x",
            r"\\127.0.0.1\share\q",
            r"\\localhost\c$\x",
        ] {
            assert!(!spelling_local(Path::new(p), local), "UNC {p}");
        }
        // Device namespaces: never a repository, whatever the base.
        for p in [r"\\.\pipe\x", r"\\?\GLOBALROOT\Device\HarddiskVolume1\x", r"\\.\PhysicalDrive0"] {
            assert!(!spelling_local(Path::new(p), local), "device {p}");
        }
        // A local drive-letter path, a verbatim one, and a rooted-but-local one: kept
        // regardless of the base (a fixed drive is not DRIVE_REMOTE).
        for p in [r"C:\repo\.git", r"\\?\C:\repo\.git", r"\on-current-drive\x", r"..\sib\.git"] {
            assert!(spelling_local(Path::new(p), local), "local {p}");
        }
    }

    /// The share exception: a gitdir ON the share the session already sits on is kept,
    /// and one on any OTHER share - the credential-leak shape - is refused. Pure string
    /// work: `same_root` touches nothing, so the non-resolving host is never contacted.
    #[test]
    fn a_gitdir_on_the_sessions_own_share_is_kept_and_another_share_is_not() {
        let on_share = Path::new(r"\\cctab-nohost\share\repo");
        assert!(spelling_local(Path::new(r"\\cctab-nohost\share\repo\.git"), on_share), "same share");
        assert!(
            spelling_local(Path::new(r"\\CCTAB-NOHOST\SHARE\repo\.git\HEAD"), on_share),
            "same share, case-folded"
        );
        assert!(!spelling_local(Path::new(r"\\cctab-nohost\other\x"), on_share), "sibling share");
        assert!(!spelling_local(Path::new(r"\\cctab-evil\share\x"), on_share), "another host");
        // And `same_root` itself, drive letters included.
        assert!(same_root(Path::new(r"C:\a"), Path::new(r"C:\b\c")));
        assert!(same_root(Path::new(r"c:\a"), Path::new(r"C:\b")), "case");
        assert!(!same_root(Path::new(r"C:\a"), Path::new(r"D:\a")));
        assert!(!same_root(Path::new(r"\\h\s\a"), Path::new(r"\\h\s2\a")), "share differs");
        // Separators must not matter: git records a worktree's gitdir with forward
        // slashes (`//server/share/...`), which must still match a `\\server\share`
        // base the session was opened on, or a worktree on a share stops resolving.
        assert!(
            same_root(Path::new("//h/s/r/.git/worktrees/w"), Path::new(r"\\h\s\r")),
            "forward-slash UNC gitdir matches a back-slash base on the same share"
        );
        assert!(same_root(Path::new(r"\\h\s\a"), Path::new("//H/S/b")), "case + separators");
    }

    /// The reparse half: a `.git` (or HEAD) junction to a LOCAL directory is followed,
    /// and `gitpath_allowed` says yes without ever following it to decide.
    #[test]
    fn a_git_junction_to_a_local_directory_is_allowed() {
        let d = scratch("guard-junction-local");
        let target = d.join("real-git");
        fs::create_dir_all(&target).expect("mkdir");
        let dotgit = d.join(".git");
        link_dir(&target, &dotgit).expect("junction");
        assert!(gitpath_allowed(&dotgit, &d), "a junction to a local dir is followed");
        // An absent path is the caller's business, not a refusal.
        assert!(gitpath_allowed(&d.join("nope"), &d));
        let _ = fs::remove_dir_all(&d);
    }

    /// The residual the critic found: a `.git` that is a reparse point to a SHARE. The
    /// link is judged on its target by `read_link`, which does not follow it, so the
    /// non-resolving host is never contacted - and the guard refuses. A directory
    /// symlink needs Developer Mode or elevation, so where the OS refuses to create
    /// one the test says why and stops rather than failing.
    #[test]
    fn a_git_symlink_to_a_share_is_refused_without_following_it() {
        let d = scratch("guard-symlink-unc");
        let link = d.join(".git");
        let unc = Path::new(r"\\cctab-nohost\share\x");
        if let Err(e) = std::os::windows::fs::symlink_dir(unc, &link) {
            eprintln!("skipped: cannot create a directory symlink here ({e}); needs Developer Mode");
            let _ = fs::remove_dir_all(&d);
            return;
        }
        // read_link hands back the UNC target; the guard refuses it. Nothing follows
        // the link, so no SMB connection is attempted.
        assert_eq!(fs::read_link(&link).ok().as_deref(), Some(unc), "the link names the share");
        assert!(!gitpath_allowed(&link, &d), "a reparse point to a share is refused");
        let _ = fs::remove_dir_all(&d);
    }

    /// A reparse CHAIN: a `.git` junction (no privilege) fronting a symlink that
    /// points at a share. A one-hop check would read only the junction's LOCAL target
    /// and wave it through, then the caller's `exists` would follow the rest to the
    /// share. The guard must walk every hop, so it refuses on the symlink's UNC target
    /// without any following call. The symlink hop needs Developer Mode; where the OS
    /// refuses it the test says why and stops rather than failing.
    #[test]
    fn a_git_junction_fronting_a_symlink_to_a_share_is_refused() {
        let d = scratch("guard-chain-unc");
        let mid = d.join("mid"); // a symlink -> share
        let unc = Path::new(r"\\cctab-nohost\share\x");
        if let Err(e) = std::os::windows::fs::symlink_dir(unc, &mid) {
            eprintln!("skipped: cannot create a directory symlink here ({e}); needs Developer Mode");
            let _ = fs::remove_dir_all(&d);
            return;
        }
        let dotgit = d.join(".git"); // a junction -> the local `mid`
        link_dir(&mid, &dotgit).expect("junction");
        assert!(
            !gitpath_allowed(&dotgit, &d),
            "the chain is walked to the share's symlink and refused"
        );
        let _ = fs::remove_dir_all(&d);
    }

    /// An all-local reparse chain - a junction to a junction to a real directory - is
    /// followed to the end, so a legitimate layered link keeps resolving.
    #[test]
    fn a_local_reparse_chain_is_followed() {
        let d = scratch("guard-chain-local");
        let real = d.join("real");
        fs::create_dir_all(&real).expect("mkdir");
        let hop1 = d.join("hop1");
        link_dir(&real, &hop1).expect("junction 1");
        let dotgit = d.join(".git");
        link_dir(&hop1, &dotgit).expect("junction 2");
        assert!(gitpath_allowed(&dotgit, &d), "a local chain is followed to the end");
        let _ = fs::remove_dir_all(&d);
    }
}
