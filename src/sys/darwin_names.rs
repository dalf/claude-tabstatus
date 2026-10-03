//! Management-only evidence for missing Unicode components on APFS/HFS volumes.
//! Like Git's composition probe, ask the filesystem about the exact names. Never
//! fold Unicode in userspace, write the requested destination, or persist a cache.

use super::{darwin_acl, file_id, FileId};
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

fn check(result: libc::c_int) -> io::Result<()> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn stat_at(dir: &File, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: live directory fd, NUL-terminated single component, writable output.
    check(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(unsafe { stat.assume_init() })
}

fn identity(stat: &libc::stat) -> FileId {
    (stat.st_dev as u64, stat.st_ino as u128)
}

fn open_dir(parent: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: live parent and component; successful fd becomes owned exactly once.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn remove_dir(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: live fd and component. Remove only an empty directory, never recurse
    // or follow a replacement symlink, even when tidying a failed probe.
    check(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) })
}

struct Probe {
    parent: File,
    name: CString,
    path: PathBuf,
    dir: Option<File>,
    children: Vec<CString>,
    removed: bool,
}

impl Probe {
    fn create(ancestor: &Path) -> io::Result<Self> {
        let parent = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(ancestor)?;
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: live fd and statfs output; libc includes Darwin's inode64 ABI.
        check(unsafe { libc::fstatfs(parent.as_raw_fd(), fs.as_mut_ptr()) })?;
        let fs = unsafe { fs.assume_init() };
        // These native filesystems have volume-wide filename rules. A sibling
        // probe cannot establish a remote/third-party filesystem's per-directory
        // behaviour, so those retain uncertainty. The probe itself must be on
        // this volume, not in TMPDIR or another cached filesystem.
        let kind: Vec<u8> = fs
            .f_fstypename
            .iter()
            .map(|c| *c as u8)
            .take_while(|c| *c != 0)
            .collect();
        if kind != b"apfs" && kind != b"hfs" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "Unicode filename probes require APFS or HFS (found {})",
                    String::from_utf8_lossy(&kind)
                ),
            ));
        }
        for _ in 0..16 {
            // SAFETY: arc4random is provided by Darwin libc and needs no seed.
            let name = format!(
                ".cctab-name-probe-{:08x}{:08x}{:08x}{:08x}",
                unsafe { libc::arc4random() },
                unsafe { libc::arc4random() },
                unsafe { libc::arc4random() },
                unsafe { libc::arc4random() }
            );
            let path = ancestor.join(&name);
            match darwin_acl::create_probe_directory(&path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
                Ok(()) => {}
            }
            let mut probe = Self {
                parent,
                name: CString::new(name).unwrap(),
                path,
                dir: None,
                children: Vec::new(),
                removed: false,
            };
            let setup = (|| {
                probe.dir = Some(open_dir(&probe.parent, &probe.name)?);
                let md = probe.dir.as_ref().unwrap().metadata()?;
                if md.uid() != unsafe { libc::geteuid() }
                    || md.mode() & 0o077 != 0
                    || md.dev() != probe.parent.metadata()?.dev()
                {
                    return Err(io::Error::other(
                        "filename probe is not private on the destination volume",
                    ));
                }
                darwin_acl::verify_probe_directory(probe.dir.as_ref().unwrap())?;
                // Detect an ancestor substitution before accepting the probe's evidence.
                if file_id(&std::fs::metadata(ancestor)?) != file_id(&probe.parent.metadata()?) {
                    return Err(io::Error::other(
                        "filename probe ancestor changed during inspection",
                    ));
                }
                Ok(())
            })();
            if let Err(e) = setup {
                if let Err(cleanup) = probe.cleanup() {
                    return Err(io::Error::other(format!(
                        "{}; cannot remove filename probe {}: {}",
                        e,
                        probe.path.display(),
                        cleanup
                    )));
                }
                return Err(e);
            }
            return Ok(probe);
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot reserve a private filename probe",
        ))
    }

    fn compare(&mut self, a: &[u8], b: &[u8]) -> io::Result<bool> {
        let a = CString::new(a).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let b = CString::new(b).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let dir = self.dir.as_ref().unwrap();
        // SAFETY: live, private directory and one NUL-terminated component. No
        // existing entry is reused; this directory starts empty.
        check(unsafe { libc::mkdirat(dir.as_raw_fd(), a.as_ptr(), 0o700) })?;
        self.children.push(a);
        let created = stat_at(dir, &self.children[0])?;
        match stat_at(dir, &b) {
            Ok(found)
                if found.st_mode & libc::S_IFMT == libc::S_IFDIR
                    && identity(&created) == identity(&found) =>
            {
                Ok(true)
            }
            Ok(_) => Err(io::Error::other(
                "unexpected entry in private filename probe",
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Establish separation by actually creating both names. A failed
                // creation (invalid UTF-8, unsupported codepoint, etc.) is not
                // evidence that the proposed installation can safely proceed.
                check(unsafe { libc::mkdirat(dir.as_raw_fd(), b.as_ptr(), 0o700) })?;
                self.children.push(b);
                let other = stat_at(dir, &self.children[1])?;
                if other.st_mode & libc::S_IFMT == libc::S_IFDIR
                    && identity(&created) != identity(&other)
                    && identity(&created) == identity(&stat_at(dir, &self.children[0])?)
                {
                    Ok(false)
                } else {
                    Err(io::Error::other("filename probe changed during inspection"))
                }
            }
            Err(e) => Err(e),
        }
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        while let Some(child) = self.children.last() {
            remove_dir(self.dir.as_ref().unwrap(), child)?;
            self.children.pop();
        }
        if let Some(dir) = &self.dir {
            if identity(&stat_at(&self.parent, &self.name)?) != file_id(&dir.metadata()?).unwrap() {
                return Err(io::Error::other(
                    "filename probe was replaced; refusing to remove its replacement",
                ));
            }
        }
        remove_dir(&self.parent, &self.name)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // Retry on an early failure/unwind; ordinary completion checks cleanup
        // explicitly and cannot accept an answer if removing the probe failed.
        let _ = self.cleanup();
    }
}

pub(super) fn same_missing_name(ancestor: &Path, a: &[u8], b: &[u8]) -> Result<bool, String> {
    let mut probe = Probe::create(ancestor).map_err(|e| format!(
        "cannot probe missing Unicode names beneath {}: {}; create the intended ancestor directory first",
        ancestor.display(), e))?;
    let result = probe.compare(a, b);
    if let Err(e) = probe.cleanup() {
        return Err(format!(
            "cannot remove filename probe {}: {}; comparison remains uncertain",
            probe.path.display(),
            e
        ));
    }
    result.map_err(|e| {
        format!(
            "cannot inspect missing Unicode names beneath {}: {}",
            ancestor.display(),
            e
        )
    })
}
