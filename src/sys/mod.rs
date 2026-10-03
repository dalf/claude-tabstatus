//! The platform seam: everything that differs between Unix and Windows, behind
//! one API, so the modules above it read the same on both.
//!
//! The shape is std's own: a `unix` and a `windows` backend with identical
//! signatures, and exactly one of them re-exported here. Callers never write
//! `#[cfg]`; when a question has no answer on a platform, the function says so in
//! its type (`Option`, `bool`) and the caller's existing "unknown" path handles it.
//! The `HAS_*` constants are the same facts for a caller that has no file in hand
//! to ask about - a report line, or a layer deciding whether to switch itself on.
//!
//! What lives here is deliberately small - the OS facts, not the policy. Whether a
//! missing process start time means "keep the record" or a missing mode means
//! "leave it alone" is decided by the caller, where the reason is documented.
//!
//! The one family with more than a syscall in it is the LIVE-WIRING writes: the
//! plugin link and the binary hooks exec. [`link_dir`] makes a directory link - a
//! symlink on Unix, a junction on Windows, which needs no privilege - and
//! [`replace_dir_link`] and [`replace_running`] swap one in place. On Unix each is
//! exactly one `rename(2)`, atomic. On Windows each tries the one rename first -
//! which NTFS grants for a junction - and falls back, where Windows refuses it (a
//! running program's file, always), to the least-bad sequence of renames, with its
//! brief window documented where it is implemented; [`sweep_replaced`] collects what
//! that sequence could not delete at the time.
//!
//! And MANAGEMENT PATH IDENTITY: [`normalize`] preserves root-link inspection;
//! [`same_path`] and [`is_within`] return errors when identity or destination
//! containment cannot be established. Unix identifies existing directories by
//! device/inode, following permitted ancestor links; Darwin asks the filesystem
//! about missing ASCII names and retains uncertainty for missing Unicode aliases.
//! Windows retains its distinction between link spelling and resolved destination.
//! [`destination_ancestors`] supplies the physical checkout-containment walk.
//! [`strip_home_prefix`] is separate: the tab title's `~` uses spelling alone, with no filesystem
//! call: on Windows letter case, `/` or `\` and a `\\?\` prefix still do not matter,
//! but an 8.3 short name - which only the disk can expand - does.
//!
//! And THE RECORD LOCK, which is the state layer's whole concurrency story and the
//! same algorithm on both: lock the record's file with [`lock_exclusive`], prove the
//! path still names the locked file ([`file_id_of`] against [`file_id_at`]), and
//! write by [`replace_file`] over the file while still holding it. On Unix that is
//! `flock`, `fstat`/`lstat` and `rename(2)`, exactly as before this seam existed. On
//! Windows the lock is `LockFileEx`, which is MANDATORY - it refuses every other
//! handle's read of the bytes it covers - so it covers one byte far past any record
//! and no reader ever meets it; identity is `FILE_ID_INFO` from a handle; and the
//! replace is a POSIX-semantics rename, the only kind that replaces a file its writer
//! holds open. [`replaces_open_files`] says where that kind exists.
//!
//! And A REWRITE'S PROTECTION beyond its mode: [`security_of`] reads what the file
//! being replaced carries besides its mode bits, and [`create_secured`] creates the
//! replacement with it before a byte is written. Linux retains its mode-only
//! policy without additional syscalls. Darwin also preserves owner/group and ordered
//! ACL entries and flags, including absence of an ACL. On Windows it is the DACL (and
//! its owner and group where they can be set), which the rename over it would otherwise
//! replace with the directory's inherited ACL.
//!
//! And THE SESSION'S TAB, for the two edges `terminalSequence` cannot carry. On Unix
//! it is a pty, resolved by [`session_tty`] and written as bytes. On Windows it is
//! the console Claude Code runs in, and [`set_session_title`] sets that console's
//! title, which the pseudo console forwards to the terminal as an OSC 0. Each backend
//! has both functions; the one with nothing to reach says so (`None`, `Ok(false)`),
//! and [`HAS_SESSION_TTY`] and [`HAS_SESSION_CONSOLE`] say which route exists. Both
//! routes sit behind the same HEADLESS GUARD - paint only a terminal that provably
//! belongs to this session - documented at each.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub use imp::{
    destination_ancestors, copy_secured, create_private_dir, create_secured, display_bytes, file_id, file_id_at, file_id_of,
    gitpath_allowed, home_fallback,
    is_executable, is_line_end, is_set_aside, is_within, kernel_hostname_file, link_dir, lock_exclusive, mode,
    normalize, os_str_from_bytes, os_string_from_vec, probe_dir_link, process_alive,
    process_start_time, remove_dir_command, remove_dir_link, replace_dir_link, replace_file,
    replace_running, replaces_open_files, reserved_name, same_path, same_process, security_of,
    session_tty, set_mode, set_session_title, strip_home_prefix, sweep_replaced, verify_security,
    with_mode, write_tty, Security, CAN_FORCE_ACL, DIR_LINK, HAS_MODES, HAS_SECURITY,
    HAS_RECORD_LOCK, HAS_SESSION_CONSOLE, HAS_SESSION_TTY, HAS_UNLINK_RUNNING, NO_STATE_DIR,
    ORIGIN_KEY, RUNTIME_DIR_VAR,
};

/// What identifies a file independently of its name: device and inode on Unix, the
/// volume serial and the 128-bit file id on Windows (ReFS uses all 128 bits). The
/// functions answer `None` where there is no such identity to offer, which callers
/// read as "cannot prove same file".
pub type FileId = (u64, u128);

/// Test helpers that set and read a file's DACL through Win32 directly, so a test of
/// [`security_of`] and [`create_secured`] does not grade them with themselves.
#[cfg(all(test, windows))]
pub(crate) use imp::test_acl;

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// The directory command is pasted into a shell as it stands, so a path any shell
    /// would split or expand is quoted - and an ordinary one reads exactly as before.
    #[test]
    #[cfg(unix)]
    fn the_removal_hint_quotes_a_path_a_shell_would_split() {
        use std::path::Path;
        assert_eq!(
            remove_dir_command(Path::new("/home/me/.local/share/claude-tabstatus")),
            "rm -rf /home/me/.local/share/claude-tabstatus"
        );
        assert_eq!(remove_dir_command(Path::new("/home/me/my tree/t")), "rm -rf '/home/me/my tree/t'");
        assert_eq!(remove_dir_command(Path::new("/tmp/it's;$(x)*")), r"rm -rf '/tmp/it'\''s;$(x)*'");
        assert_eq!(remove_dir_command(Path::new("/tmp/~x")), "rm -rf '/tmp/~x'");
    }

    #[test]
    fn utf8_bytes_round_trip_on_every_platform() {
        let s = OsStr::new("~/code/caf\u{e9}@main");
        assert_eq!(os_str_from_bytes(s.as_encoded_bytes()), s);
        assert_eq!(os_string_from_vec(s.as_encoded_bytes().to_vec()), s);
    }

    /// The platform facts agree with the functions that answer the same question,
    /// so a caller branching on a constant cannot contradict one calling the
    /// function.
    #[test]
    fn the_capability_constants_match_the_functions() {
        let exe = std::env::current_exe().expect("exe");
        let m = std::fs::metadata(&exe).expect("meta");
        let lm = std::fs::symlink_metadata(&exe).expect("lstat");
        let f = std::fs::File::open(&exe).expect("open");
        // A handle and the path it was opened by agree on what file that is.
        let proven = file_id_of(&f).is_some() && file_id_at(&exe, &lm) == file_id_of(&f);
        assert_eq!(HAS_RECORD_LOCK, proven);
        assert_eq!(HAS_MODES, mode(&m).is_some());
        assert_eq!(HAS_MODES, is_executable(&m).is_some());
        assert_eq!(HAS_SECURITY, security_of(&exe).expect("readable").is_some());
        // Without a console route the function answers "not painted" for anything,
        // our own pid included - so a caller branching on the constant and one calling
        // the function agree.
        if !HAS_SESSION_CONSOLE {
            let me = std::process::id().to_string();
            assert!(!set_session_title(OsStr::new(&me), "x").expect("no error"));
        }
    }

    /// The record lock excludes another locker - a second handle, in this very
    /// process - and refuses NO reader. The second half is the one Windows has to be
    /// made to keep: `LockFileEx` refuses every read of the bytes it covers.
    #[test]
    fn the_record_lock_excludes_another_locker_and_no_reader() {
        let d = scratch("lock");
        let rec = d.join("rec");
        std::fs::write(&rec, b"cts5\nb w\n").expect("write");
        let open = || {
            std::fs::OpenOptions::new().read(true).write(true).open(&rec).expect("open")
        };
        let held = open();
        lock_exclusive(&held).expect("locked");
        assert_eq!(std::fs::read(&rec).expect("a reader is not refused"), b"cts5\nb w\n");
        let second = open();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            lock_exclusive(&second).expect("locked in turn");
            tx.send(()).expect("send");
        });
        let wait = std::time::Duration::from_millis(300);
        assert!(rx.recv_timeout(wait).is_err(), "a second locker waits for the first");
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(10)).expect("and gets it once released");
        waiter.join().expect("join");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The write the state layer makes: a new file replaces the record while its
    /// writer still holds the old one open and locked. The name then names the new
    /// file, the held handle still reads the old one, and their identities differ -
    /// which is how a hook queued on the old file learns it must lock again.
    ///
    /// Also where the record's path is longer than Windows' MAX_PATH (260): every
    /// other operation std makes reaches such a path, so the replace must too, or
    /// the layer is on and silently records nothing.
    #[test]
    fn a_replace_lands_while_the_old_file_is_held_open_and_locked() {
        let d = scratch("replace-held");
        let mut deep = d.join("deep");
        while deep.as_os_str().len() < 300 {
            deep.push("abcdefghijklmnopqrstuvwxyz0123456789");
        }
        std::fs::create_dir_all(&deep).expect("mkdir deep");
        for dir in [&d, &deep] {
            assert!(replaces_open_files(dir), "the platform temp directory can");
            let (rec, tmp) = (dir.join("rec"), dir.join("rec.1.tmp"));
            std::fs::write(&rec, b"old").expect("write");
            let held = std::fs::OpenOptions::new().read(true).write(true).open(&rec).expect("open");
            lock_exclusive(&held).expect("locked");
            std::fs::write(&tmp, b"new").expect("write");
            replace_file(&tmp, &rec).expect("replaced while held");
            assert_eq!(std::fs::read(&rec).expect("read"), b"new");
            let mut old = Vec::new();
            std::io::Read::read_to_end(&mut &held, &mut old).expect("read held");
            assert_eq!(old, b"old", "the held handle keeps the file it opened");
            let lm = std::fs::symlink_metadata(&rec).expect("lstat");
            assert_ne!(file_id_of(&held), file_id_at(&rec, &lm), "the name moved on");
        }
        assert_eq!(names(&d), vec!["deep", "rec"]);
        assert_eq!(names(&deep), vec!["rec"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Not a test: a process for the liveness test below, alive until its stdin closes.
    #[test]
    #[ignore = "a child process for another test"]
    fn live_until_stdin_closes() {
        if std::env::var_os("CCTAB_TEST_HOLD").is_some() {
            let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
        }
    }

    /// The reaper's one question: is the process that recorded (pid, start) still
    /// that process? Yes for this one; no for a wrong start (a reused pid), for a
    /// child that has exited - also while its handle is still held, which on Windows
    /// keeps the pid reserved - and for a pid nothing has.
    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn a_process_is_the_same_process_only_while_it_runs() {
        let me = std::process::id();
        let start = process_start_time(me).expect("our own start time");
        assert_eq!(process_start_time(me), Some(start), "immutable");
        assert_eq!(same_process(me, start), Some(true));
        assert_eq!(same_process(me, start + 1), Some(false));
        assert_eq!(process_alive(me), Some(true));
        let mut child = std::process::Command::new(std::env::current_exe().expect("exe"))
            .args(["--exact", "sys::tests::live_until_stdin_closes", "--ignored"])
            .env("CCTAB_TEST_HOLD", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let born = process_start_time(pid).expect("a running child has a start time");
        assert_eq!(same_process(pid, born), Some(true));
        drop(child.stdin.take());
        child.wait().expect("wait");
        assert_eq!(same_process(pid, born), Some(false), "exited, handle still held");
        assert_eq!(process_start_time(pid), None);
        drop(child);
        assert_eq!(same_process(pid, born), Some(false), "exited, handle released");
        assert_eq!(same_process(u32::MAX, 1), Some(false), "no such pid");
    }

    /// A word of the session-id grammar that Windows opens as a DEVICE in any
    /// directory; nothing is reserved on Unix.
    #[test]
    fn only_dos_device_names_are_reserved() {
        for n in ["NUL", "nul", "Con", "PRN", "aux", "COM1", "lpt9", "COM0"] {
            assert_eq!(reserved_name(n), cfg!(windows), "{n}");
        }
        for n in ["NULL", "COM", "COM10", "LPT", "s1", "aec0f2b1-4d31-4e11-9a41-2c7d55e1a900"] {
            assert!(!reserved_name(n), "{n}");
        }
    }

    #[test]
    fn a_newline_ends_a_line_everywhere_and_a_lone_cr_only_on_windows() {
        assert!(is_line_end(b'\n'));
        assert_eq!(is_line_end(b'\r'), cfg!(windows));
        assert!(!is_line_end(b'x'));
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cctab-sys-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    /// Everything in `dir`, by name, sorted - so a test can say "and nothing else".
    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The contract the plugin link rests on, on both platforms: the link reads back
    /// as a link naming the target, the target's files are reachable through it, and
    /// removing the LINK leaves the target and everything in it where it was.
    #[test]
    fn a_directory_link_names_its_target_and_removing_it_spares_the_target() {
        let d = scratch("link");
        let target = d.join("caf\u{e9} tree");
        std::fs::create_dir_all(target.join("bin")).expect("mkdir");
        std::fs::write(target.join("bin").join("keep"), b"payload").expect("write");
        let link = d.join("skills-link");

        link_dir(&target, &link).expect("linked");
        let md = std::fs::symlink_metadata(&link).expect("link is there");
        assert!(md.file_type().is_symlink(), "std reads it as a link");
        assert_eq!(std::fs::read_link(&link).expect("read_link"), target);
        assert_eq!(std::fs::read(link.join("bin").join("keep")).expect("through"), b"payload");

        remove_dir_link(&link).expect("removed");
        assert!(std::fs::symlink_metadata(&link).is_err(), "the link is gone");
        assert_eq!(std::fs::read(target.join("bin").join("keep")).expect("intact"), b"payload");
        assert_eq!(names(&d), vec!["caf\u{e9} tree"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The repoint `install` does: a new link at a temp name replaces the live one.
    /// Afterwards the live name reads the new target, the temp name is gone, neither
    /// target lost a file, and - once swept - the directory holds nothing it did not.
    #[test]
    fn replacing_a_directory_link_repoints_it_and_spares_both_targets() {
        let d = scratch("relink");
        let (a, b) = (d.join("a"), d.join("b"));
        for t in [&a, &b] {
            std::fs::create_dir_all(t).expect("mkdir");
            std::fs::write(t.join("f"), t.to_string_lossy().as_bytes()).expect("write");
        }
        let link = d.join("live");
        let tmp = d.join(".live.cctab-tmp.1");
        link_dir(&a, &link).expect("linked");

        link_dir(&b, &tmp).expect("temp link");
        replace_dir_link(&tmp, &link).expect("replaced");
        assert_eq!(std::fs::read_link(&link).expect("read_link"), b);
        assert_eq!(std::fs::read(link.join("f")).expect("through"), b.to_string_lossy().as_bytes());
        assert!(std::fs::symlink_metadata(&tmp).is_err(), "the temp link was moved, not copied");
        for t in [&a, &b] {
            assert_eq!(std::fs::read(t.join("f")).expect("intact"), t.to_string_lossy().as_bytes());
        }
        sweep_replaced(&d);
        assert_eq!(names(&d), vec!["a", "b", "live"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The preflight probe answers without leaving anything behind - on Unix by
    /// touching nothing, on Windows by making and removing a junction.
    #[test]
    fn the_link_probe_leaves_nothing_behind() {
        let d = scratch("probe");
        probe_dir_link(&d.join("not-yet"), &d).expect("a link can be made here");
        assert!(names(&d).is_empty(), "{:?}", names(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file nothing is running is simply replaced, on both platforms, and leaves no
    /// aside copy for a sweep to find.
    #[test]
    fn replacing_a_file_nothing_runs_is_one_rename() {
        let d = scratch("replace");
        let (new, dst) = (d.join(".bin.cctab-tmp.1"), d.join("bin"));
        std::fs::write(&dst, b"old").expect("write");
        std::fs::write(&new, b"new").expect("write");
        assert_eq!(replace_running(&new, &dst).expect("replaced"), None);
        assert_eq!(std::fs::read(&dst).expect("read"), b"new");
        assert_eq!(names(&d), vec!["bin"]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
