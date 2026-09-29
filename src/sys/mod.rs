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
//! And PATH IDENTITY: [`normalize`], [`same_path`] and [`is_within`]. On Unix a path
//! is its bytes and all three are the plain comparison. On Windows one directory has
//! many spellings - any letter case, an 8.3 short name, a `\\?\` prefix - and every
//! refusal, "already correct" and "orphan" decision install makes compares paths, so
//! they have to agree on what "the same directory" means.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub use imp::{
    create_private_dir, file_id, home_fallback, is_executable, is_line_end, is_within,
    kernel_hostname_file, link_dir, mode, normalize, os_str_from_bytes, os_string_from_vec,
    probe_dir_link, process_alive, process_start_time, remove_dir_command, remove_dir_link,
    replace_dir_link, replace_running, same_path, session_tty, set_mode, sweep_replaced,
    with_mode, write_tty, DIR_LINK, HAS_FILE_ID, HAS_MODES, HAS_SESSION_TTY, HAS_UNLINK_RUNNING,
};

/// What identifies a file independently of its name: device and inode on Unix.
/// [`file_id`] answers `None` where there is no such identity to offer, which
/// callers read as "cannot prove same file".
pub type FileId = (u64, u64);

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

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
        let m = std::fs::metadata(std::env::current_exe().expect("exe")).expect("meta");
        assert_eq!(HAS_FILE_ID, file_id(&m).is_some());
        assert_eq!(HAS_MODES, mode(&m).is_some());
        assert_eq!(HAS_MODES, is_executable(&m).is_some());
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
