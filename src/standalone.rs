//! `standalone` - materialise a self-contained plugin tree from the running
//! binary, then install it.
//!
//! WHY a separate verb rather than teaching `install` to write the manifests.
//! Locally the plugin directory IS the checkout: `hooks/hooks.json` is a tracked
//! file the user reads, diffs and edits, and an installer that rewrote it from a
//! copy compiled into a possibly-older binary would silently revert their edit and
//! show up as a modification in `git status` that nobody made. So the embedded
//! bytes are authoritative in exactly one place - a tree THIS TOOL generated - and
//! `install`, `uninstall` and the paint path are untouched by this module.
//!
//! Ownership is asserted by POSITIVE EVIDENCE, never inferred: a generated tree
//! carries `.tabstatus-generated`, written by this module, and a directory without
//! it is refused rather than written into. That is what makes "never overwrite the
//! repo's own files" a property of the code and not of a heuristic - a checkout
//! has no marker, and a `.git` beside the target is refused a second time even if
//! a marker somehow appears there.
//!
//! Inside a tree we do own, the embedded bytes WIN unconditionally, which is the
//! opposite rule and deliberately so. "On-disk wins when present" would make an
//! upgrade silently do nothing: a newer binary dropped next to an older generated
//! tree would keep the old hooks.json, so a release that adds a twelfth hook edge
//! would install cleanly and that edge would never fire, with nothing anywhere
//! saying why. Every generated file is therefore rewritten, files a newer version
//! dropped are pruned from the marker's list, and a file whose bytes CHANGED is
//! named - overwriting is right here, doing it silently is not.
//!
//! The tree is found by `manage::repo_root`'s ordinary upward walk: it looks for
//! `.claude-plugin/plugin.json` and `hooks/hooks.json` above the executable, and a
//! materialised tree satisfies that test verbatim, which is why nothing in the
//! installer had to change.

use crate::json;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The proof of ownership. A dotfile so it does not read as plugin content;
/// `claude plugin validate --strict` is happy with it at the tree root.
pub const MARKER: &str = ".tabstatus-generated";

const MARKER_VERSION: i32 = 1;

/// The binary `hooks/hooks.json` invokes, relative to the plugin root. In the
/// checkout this is a symlink to `bin/tabstatus-<triple>`; in a generated tree it
/// is a real copy, because the whole point is that ONE file was shipped.
pub const BIN: &str = "bin/tabstatus";

/// Every path this module writes, in the order it writes them. `bin/tabstatus`
/// last, so a tree is never loadable-but-unrunnable for longer than one copy.
fn generated_paths() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = crate::embedded::MANIFESTS.iter().map(|(p, _)| *p).collect();
    v.push(BIN);
    v
}

// --- the marker --------------------------------------------------------------

pub struct Marker {
    /// The `tabstatus version` that wrote the tree.
    pub version: Vec<u8>,
    /// The target triple of the binary that wrote it, so `doctor` can tell a tree
    /// materialised by one architecture and later run by another.
    pub target: Vec<u8>,
    /// What that run generated, which is what a later run prunes against.
    pub files: Vec<Vec<u8>>,
}

pub fn marker_path(tree: &Path) -> PathBuf {
    tree.join(MARKER)
}

/// Is this tree ours to rewrite? The FILE's existence is the answer, not its
/// contents: we wrote it, so a marker we can no longer parse is still ours, and
/// `doctor` reports the unparseable marker rather than this deciding the tree is
/// somebody else's and refusing to refresh it.
pub fn is_generated(tree: &Path) -> bool {
    fs::metadata(marker_path(tree)).map(|m| m.is_file()).unwrap_or(false)
}

pub fn marker_text(version: &str, target: &str, files: &[&str]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"marker_version\": {},\n", MARKER_VERSION));
    out.push_str("  \"written_by\": \"tabstatus standalone\",\n");
    out.push_str(&format!("  \"tabstatus_version\": {},\n", json::quote(version.as_bytes())));
    out.push_str(&format!("  \"target\": {},\n", json::quote(target.as_bytes())));
    out.push_str("  \"files\": [\n");
    let mut sorted: Vec<&str> = files.to_vec();
    sorted.sort_unstable();
    for (i, f) in sorted.iter().enumerate() {
        let comma = if i + 1 == sorted.len() { "" } else { "," };
        out.push_str(&format!("    {}{}\n", json::quote(f.as_bytes()), comma));
    }
    out.push_str("  ]\n}\n");
    out.into_bytes()
}

/// `None` when there is no marker at all; `Err` when there is one and it cannot be
/// read as ours, which `doctor` reports and `standalone` treats as "prune nothing".
pub fn read_marker(tree: &Path) -> Option<Result<Marker, String>> {
    let p = marker_path(tree);
    if !p.is_file() {
        return None;
    }
    Some((|| {
        let raw = fs::read(&p).map_err(|e| format!("cannot read {}: {}", p.display(), e))?;
        let v = json::parse(&raw).map_err(|e| format!("{} is not valid JSON ({})", p.display(), e))?;
        let o = v.as_obj().ok_or_else(|| format!("{} is not a JSON object", p.display()))?;
        let text = |k: &str| o.get(k).and_then(|m| m.val.as_str()).map(<[u8]>::to_vec);
        let files = o
            .get("files")
            .and_then(|m| m.val.as_arr())
            .map(|a| a.iter().filter_map(|e| e.as_str().map(<[u8]>::to_vec)).collect())
            .unwrap_or_default();
        Ok(Marker {
            version: text("tabstatus_version").unwrap_or_default(),
            target: text("target").unwrap_or_default(),
            files,
        })
    })())
}

// --- where the tree goes -----------------------------------------------------

/// `$XDG_DATA_HOME/claude-tabstatus`, or `$HOME/.local/share/claude-tabstatus`.
///
/// Deliberately NOT under the config directory. Materialising into
/// `<config>/skills/claude-tabstatus` makes `install` refuse - it is then being
/// asked to symlink a directory to itself - and `skills/` is a namespace Claude
/// Code owns.
pub fn default_tree() -> Result<PathBuf, String> {
    if let Some(d) = crate::config::var_nonempty("XDG_DATA_HOME") {
        return Ok(PathBuf::from(d).join("claude-tabstatus"));
    }
    match crate::config::var_nonempty("HOME") {
        Some(h) => Ok(PathBuf::from(h).join(".local/share/claude-tabstatus")),
        None => Err("neither XDG_DATA_HOME nor HOME is set, so there is no default \
                     place for the tree. Give one: tabstatus standalone <dir>"
            .to_string()),
    }
}

/// Why this directory must not be the target. `None` means go ahead.
///
/// Three refusals, and each one exists because the alternative is a confusing
/// failure LATER: under `<config>/skills` the installer would be asked to link a
/// directory to itself, a `.git` beside the target means a checkout whose tracked
/// files are not ours to rewrite, and a non-empty directory with no marker is
/// somebody else's.
pub fn refuse_target(tree: &Path, skills: &Path) -> Option<String> {
    if tree == skills || tree.starts_with(skills) {
        return Some(format!(
            "{} is under {}, which is where install puts the symlink TO the tree. \
             A tree there would make install symlink a directory to itself. Pick \
             somewhere else, or pass no directory at all for the default.",
            tree.display(),
            skills.display()
        ));
    }
    if tree.join(".git").exists() {
        return Some(format!(
            "{} has a .git in it, so it is a checkout and its files are not this \
             command's to rewrite. Run `tabstatus install` there instead. Nothing \
             has been changed.",
            tree.display()
        ));
    }
    // Absent is exactly what we want. Anything else that is not a READABLE
    // DIRECTORY is a named refusal here rather than an errno from create_dir_all
    // three lines later: `cannot create <path>/.claude-plugin: File exists` reads
    // like a bug in this program, prints after the `mode:` line as though work had
    // begun, and lacks the "Nothing has been changed." every real refusal ends with.
    match fs::metadata(tree) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!(
            "{} exists but cannot be inspected: {}. Nothing has been changed.",
            tree.display(),
            e
        )),
        Ok(md) if !md.is_dir() => Some(format!(
            "{} already exists and is not a directory, so a plugin tree cannot be \
             written there. Pass a different directory. Nothing has been changed.",
            tree.display()
        )),
        Ok(_) => match fs::read_dir(tree) {
            Err(e) => Some(format!(
                "{} exists but cannot be read: {}. Nothing has been changed.",
                tree.display(),
                e
            )),
            Ok(mut rd) => {
                if is_generated(tree) || rd.next().is_none() {
                    None
                } else {
                    Some(format!(
                        "{} already exists, is not empty, and carries no {} - so it was \
                         not written by `tabstatus standalone` and is not ours to \
                         overwrite. If it is a checkout, run `tabstatus install` there. \
                         If it is nothing you need, remove it - `rm -rf {}` - and \
                         re-run. Otherwise pass a different directory. Nothing has \
                         been changed.",
                        tree.display(),
                        MARKER,
                        tree.display()
                    ))
                }
            }
        },
    }
}

// --- materialising -----------------------------------------------------------

/// Write the embedded manifests and the running binary into `tree`, prune what a
/// previous run generated and this one does not, and record what was written.
///
/// Returns the report lines rather than printing them, so the one place that
/// writes to stdout stays `manage::say`.
pub fn materialise(tree: &Path, exe: &Path, version: &str, target: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let existed = tree.exists();
    let prior = read_marker(tree);

    fs::create_dir_all(tree).map_err(|e| format!("cannot create {}: {}", tree.display(), e))?;
    out.push(format!(
        "tree:     {} ({})",
        tree.display(),
        if existed { "refreshed" } else { "created" }
    ));

    // The marker FIRST, listing what this run intends to write, and rewritten at
    // the end with what actually landed. Ownership has to be claimed BEFORE the
    // files, because the marker is the only evidence `refuse_target` accepts: a run
    // killed anywhere in the window below - ENOSPC during the 635 KB binary copy on
    // a small VM, a dropped ssh, an OOM, a Ctrl-C - would otherwise leave both
    // manifests and no marker, which the next run classifies as somebody else's
    // directory and refuses FOREVER, on exactly the machine this verb exists for.
    // Marker first makes every partial state re-enterable by construction, which is
    // the property the marker exists to provide, and it costs nothing: `read_marker`
    // already tolerates a marker it cannot parse and a stale file list.
    //
    // ONE write, not a claim and a later correction: the list cannot change between
    // here and the end, because every step below returns Err rather than carrying
    // on, and a single write leaves the marker provably older than the files it
    // vouches for - which is the ordering, testable from outside.
    let now = generated_paths();
    crate::manage::write_atomic(&marker_path(tree), &marker_text(version, target, &now), 0o644)?;
    out.push(format!(
        "marker:   {} ({} files, written first so an interrupted run is resumable)",
        MARKER,
        now.len()
    ));

    for (rel, text) in crate::embedded::MANIFESTS {
        let dst = tree.join(rel);
        let before = fs::read(&dst).ok();
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        crate::manage::write_atomic(&dst, text.as_bytes(), 0o644)?;
        // Overwriting is correct in a tree we own; doing it silently is not, so a
        // file whose bytes CHANGED says so and says what it was.
        let what: String = match &before {
            None => "new".to_string(),
            Some(b) if b == text.as_bytes() => "unchanged".to_string(),
            // A version bump rarely changes a manifest's LENGTH, and "REPLACED, was
            // 438 bytes" beside "438 bytes" reads as a false positive rather than as
            // the answer. Same distinction `doctor` draws, in the same words.
            Some(b) if b.len() == text.len() => {
                format!("REPLACED - same {} bytes, different content", b.len())
            }
            Some(b) => format!("REPLACED, was {} bytes", b.len()),
        };
        out.push(format!("wrote:    {} ({} bytes, {})", rel, text.len(), what));
    }

    // The binary LAST, and skipped when it is the one running: copying onto a
    // running executable is ETXTBSY, and re-running `standalone` through the
    // installed tree is a normal thing to do.
    let dst = tree.join(BIN);
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
    }
    let same_file = fs::canonicalize(&dst).ok() == fs::canonicalize(exe).ok() && dst.exists();
    if same_file {
        out.push(format!("wrote:    {} (skipped - it IS the running binary)", BIN));
    } else {
        // Removed first: fs::copy onto a file some other session is executing
        // would be ETXTBSY, and an unlink leaves that process running its own
        // inode.
        let _ = fs::remove_file(&dst);
        let n = fs::copy(exe, &dst)
            .map_err(|e| format!("cannot copy {} to {}: {}", exe.display(), dst.display(), e))?;
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot chmod {}: {}", dst.display(), e))?;
        out.push(format!("wrote:    {} ({} bytes, copied from {})", BIN, n, exe.display()));
    }

    // Prune what an OLDER version generated and this one no longer does, so an
    // upgrade cannot leave a file behind that nothing rewrites.
    if let Some(Ok(m)) = &prior {
        for f in &m.files {
            let name = String::from_utf8_lossy(f).to_string();
            if now.contains(&name.as_str()) || !safe_relative(f) {
                continue;
            }
            let p = tree.join(PathBuf::from(OsString::from_vec(f.clone())));
            if p.is_file() && fs::remove_file(&p).is_ok() {
                out.push(format!("pruned:   {} (generated by an older version)", name));
            }
        }
    }

    Ok(out)
}

/// A marker path we are willing to unlink: relative, no `..`, no leading `/`. The
/// marker is ours, but it is still a file on disk that something could have
/// edited, and prune is the one place here that REMOVES anything.
fn safe_relative(p: &[u8]) -> bool {
    let s = Path::new(OsStr::from_bytes(p));
    !p.is_empty()
        && !p.starts_with(b"/")
        && s.components().all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// Files inside a tree we own that the marker does NOT list, as tree-relative
/// paths, so `--purge-tree` can name what it is about to take with it.
///
/// `standalone` is scrupulous - prune only ever touches the marker's list, so a few
/// refresh runs teach the operator by behaviour that their own files are safe in
/// that directory. `--purge-tree` is `remove_dir_all` and takes them anyway.
/// Refusing would be over-cautious for a flag with `purge` in its name; staying
/// silent is the part that is wrong.
pub fn extra_files(tree: &Path, known: &[Vec<u8>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![PathBuf::new()];
    // A report, not a traversal anybody depends on: bounded so a pathological tree
    // cannot make `uninstall` hang, and symlink_metadata so a link out of the tree
    // is one entry rather than a directory to wander into.
    let mut budget = 4096usize;
    'walk: while let Some(rel) = stack.pop() {
        let Ok(rd) = fs::read_dir(tree.join(&rel)) else { continue };
        for e in rd.flatten() {
            if budget == 0 {
                break 'walk;
            }
            budget -= 1;
            let child = rel.join(e.file_name());
            let Ok(md) = fs::symlink_metadata(tree.join(&child)) else { continue };
            if md.is_dir() {
                stack.push(child);
                continue;
            }
            let bytes = child.as_os_str().as_bytes().to_vec();
            // The marker is ours whether or not it lists itself.
            if bytes == MARKER.as_bytes() || known.contains(&bytes) {
                continue;
            }
            out.push(child.to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

/// Run the copy we just made: ONE named refusal at install time instead of eleven
/// hooks silently failing in every future session. It is a probe, not a prediction,
/// so it needs to know nothing about the machine.
///
/// It does NOT cover a wrong architecture, and must not claim to: the copy is
/// `current_exe`, so it is by construction the same architecture as the process
/// running this check, and on an aarch64 machine the kernel refuses the scp'd
/// binary with its own *Exec format error* before a line of this program runs. What
/// is left is real: a filesystem mounted `noexec`, a lost exec bit, a copy that is
/// not what we think it is.
pub fn verify(tree: &Path, version: &str) -> Result<String, String> {
    let bin = tree.join(BIN);
    let out = std::process::Command::new(&bin).arg("version").output().map_err(|e| {
        format!(
            "{} was written but will not run: {}.\n\
             It is a copy of the binary you just ran, so this is not an architecture \
             mismatch: the likely causes are a filesystem mounted `noexec` and a lost \
             exec bit. Nothing else has been changed.",
            bin.display(),
            e
        )
    })?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let want = format!("tabstatus {} ", version);
    if !out.status.success() || !said.starts_with(&want) {
        return Err(format!(
            "{} ran but said {:?} instead of {:?}..., so the copy is not the binary \
             this run embedded. Nothing else has been changed.",
            bin.display(),
            said,
            want
        ));
    }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cctab-sa-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn the_marker_round_trips_what_prune_and_doctor_read_back() {
        let raw = marker_text("0.1.0", "x86_64-unknown-linux-musl", &generated_paths());
        let d = scratch("marker");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(marker_path(&d), &raw).expect("write");
        let m = read_marker(&d).expect("present").expect("parses");
        assert_eq!(m.version, b"0.1.0");
        assert_eq!(m.target, b"x86_64-unknown-linux-musl");
        // Sorted, so the file is stable across runs.
        let files: Vec<String> = m.files.iter().map(|f| String::from_utf8_lossy(f).into()).collect();
        assert_eq!(files, vec![".claude-plugin/plugin.json", "bin/tabstatus", "hooks/hooks.json"]);
        assert!(is_generated(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_marker_that_is_not_ours_is_an_error_but_still_proves_ownership() {
        let d = scratch("badmarker");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(marker_path(&d), b"not json at all").expect("write");
        // Ownership is the FILE, so a refresh is still allowed...
        assert!(is_generated(&d));
        // ...and the unparseable marker is reported rather than swallowed.
        assert!(read_marker(&d).expect("present").is_err());
        // Absent is a third answer, distinct from both.
        fs::remove_file(marker_path(&d)).expect("rm");
        assert!(read_marker(&d).is_none());
        assert!(!is_generated(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn every_reason_to_refuse_a_target_is_named_and_an_empty_dir_is_not_one() {
        let d = scratch("refuse");
        let skills = d.join("cfg/skills");
        fs::create_dir_all(&skills).expect("mkdir");

        // Absent: fine.
        assert!(refuse_target(&d.join("fresh"), &skills).is_none());
        // Empty: fine - `mkdir -p` then run is a natural sequence.
        fs::create_dir_all(d.join("empty")).expect("mkdir");
        assert!(refuse_target(&d.join("empty"), &skills).is_none());
        // Under skills, and the link itself.
        for p in [skills.join("claude-tabstatus"), skills.clone(), skills.join("a/b")] {
            let why = refuse_target(&p, &skills).expect("refused");
            assert!(why.contains("symlink a directory to itself"), "{}", why);
        }
        // A checkout, even with a marker dropped in it: tracked files are not ours.
        let co = d.join("checkout");
        fs::create_dir_all(co.join(".git")).expect("mkdir");
        fs::write(marker_path(&co), b"{}").expect("write");
        assert!(refuse_target(&co, &skills).expect("refused").contains(".git"));
        // Non-empty and unmarked.
        let other = d.join("other");
        fs::create_dir_all(&other).expect("mkdir");
        fs::write(other.join("something"), b"x").expect("write");
        assert!(refuse_target(&other, &skills).expect("refused").contains(MARKER));
        // ...and marked, which is the one that goes ahead.
        fs::write(marker_path(&other), b"{}").expect("write");
        assert!(refuse_target(&other, &skills).is_none());

        // A path that exists and is not a directory. This used to fall through to
        // `cannot create <path>: File exists (os error 17)` from create_dir_all,
        // which reads like a bug and prints after the mode line as though work had
        // begun.
        let afile = d.join("afile");
        fs::write(&afile, b"not a directory\n").expect("write");
        let why = refuse_target(&afile, &skills).expect("refused");
        assert!(why.contains("is not a directory"), "{}", why);
        assert!(why.contains("Nothing has been changed."), "{}", why);

        // Every refusal ends the same way, which is how the operator knows the
        // filesystem was not touched.
        for p in [&other_unmarked(&d), &d.join("checkout")] {
            if let Some(why) = refuse_target(p, &skills) {
                assert!(why.contains("Nothing has been changed."), "{}", why);
            }
        }

        let _ = fs::remove_dir_all(&d);
    }

    /// A non-empty unmarked directory, built fresh: the message names removing it,
    /// because for years the only two suggestions were "run install there" (it is not
    /// a checkout) and "pass a different directory" (which loses doctor and
    /// uninstall), and the actual remedy was never written down.
    fn other_unmarked(d: &Path) -> PathBuf {
        let p = d.join("unmarked");
        fs::create_dir_all(&p).expect("mkdir");
        fs::write(p.join("theirs"), b"x").expect("write");
        p
    }

    /// The property the marker exists to provide: a run killed part-way is
    /// RE-ENTERABLE. Simulated at the only point that matters - the marker is on disk
    /// before the manifests and the binary copy are - because an unmarked
    /// half-written tree is refused forever by `refuse_target` and only `rm -rf`
    /// recovers it.
    #[test]
    fn a_tree_a_killed_run_left_behind_is_still_ours_to_finish() {
        let d = scratch("partial");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write");
        let skills = d.join("cfg/skills");

        // The state a SIGKILL between the marker and the last write leaves.
        fs::create_dir_all(tree.join("hooks")).expect("mkdir");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &generated_paths())).expect("write");
        fs::write(tree.join(crate::embedded::HOOKS_JSON_PATH), b"half a hooks.json").expect("write");
        assert!(is_generated(&tree));
        // ...and it is NOT refused, which is the whole point.
        assert!(refuse_target(&tree, &skills).is_none());
        materialise(&tree, &exe, "0.1.0", "t").expect("resumed");
        for (rel, text) in crate::embedded::MANIFESTS {
            assert_eq!(fs::read(tree.join(rel)).expect("written"), text.as_bytes());
        }

        // And the ordering itself: no marker, both manifests, is the shape that used
        // to wedge the tool. It cannot arise from a successful run any more, so the
        // assertion is on the ordering - the marker is written before the binary.
        let fresh = d.join("fresh");
        materialise(&fresh, &exe, "0.1.0", "t").expect("materialised");
        let marker_first = fs::metadata(marker_path(&fresh)).expect("marker").modified();
        let bin_at = fs::metadata(fresh.join(BIN)).expect("bin").modified();
        if let (Ok(a), Ok(b)) = (marker_first, bin_at) {
            assert!(a <= b, "the marker must not be newer than the binary it claims");
        }

        let _ = fs::remove_dir_all(&d);
    }

    /// What `--purge-tree` is about to take with it. prune never touches these, so
    /// nothing else in the program would ever mention them.
    #[test]
    fn extra_files_names_what_the_marker_does_not_list() {
        let d = scratch("extra");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write");
        materialise(&tree, &exe, "0.1.0", "t").expect("materialised");
        let known: Vec<Vec<u8>> = read_marker(&tree).expect("present").expect("parses").files;

        // A freshly materialised tree holds nothing else, marker included.
        assert!(extra_files(&tree, &known).is_empty());

        fs::write(tree.join("NOTES.txt"), b"mine\n").expect("write");
        fs::create_dir_all(tree.join("commands")).expect("mkdir");
        fs::write(tree.join("commands/mine.md"), b"mine\n").expect("write");
        assert_eq!(extra_files(&tree, &known), vec!["NOTES.txt", "commands/mine.md"]);

        // An empty marker list makes everything extra, which is what a tree with an
        // unparseable marker gets - and it still must not name the marker itself.
        let all = extra_files(&tree, &[]);
        assert!(all.contains(&"bin/tabstatus".to_string()), "{:?}", all);
        assert!(!all.iter().any(|f| f == MARKER), "{:?}", all);

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn prune_only_touches_relative_paths_inside_the_tree() {
        assert!(safe_relative(b"hooks/hooks.json"));
        assert!(safe_relative(b"bin/tabstatus"));
        assert!(!safe_relative(b""));
        assert!(!safe_relative(b"/etc/passwd"));
        assert!(!safe_relative(b"../outside"));
        assert!(!safe_relative(b"bin/../../outside"));
    }

    /// The exec probe, both ways. This is the check that turns a wrong architecture,
    /// a noexec mount and a lost exec bit into ONE named refusal instead of eleven
    /// hooks failing silently in every future session - and because `materialise`
    /// chmods what it copies to 755, a shell script stands in for the binary and the
    /// probe can be driven without a second architecture to hand.
    #[test]
    fn verify_runs_the_copy_and_refuses_one_that_answers_wrong() {
        let d = scratch("verify");
        fs::create_dir_all(&d).expect("mkdir");

        let good = d.join("good");
        fs::write(&good, "#!/bin/sh\nprintf 'tabstatus 9.9.9 (some-triple)\\n'\n").expect("write");
        materialise(&d.join("ok"), &good, "9.9.9", "t").expect("materialised");
        assert_eq!(verify(&d.join("ok"), "9.9.9").expect("runs"), "tabstatus 9.9.9 (some-triple)");

        // Right shape, wrong version: a copy that is not the binary this run
        // embedded.
        assert!(verify(&d.join("ok"), "0.1.0").expect_err("refused").contains("instead of"));

        // Runs, says something else entirely.
        let bad = d.join("bad");
        fs::write(&bad, "#!/bin/sh\nprintf 'not me\\n'\n").expect("write");
        materialise(&d.join("no"), &bad, "9.9.9", "t").expect("materialised");
        assert!(verify(&d.join("no"), "9.9.9").expect_err("refused").contains("not me"));

        // Will not exec at all - a noexec mount or a lost exec bit, which is what
        // this probe is actually for. It must NOT blame the architecture: the copy is
        // current_exe, so it is always this machine's architecture, and an aarch64 VM
        // never reaches a line of this program.
        let cant = d.join("cant");
        fs::write(&cant, b"\x7fELF not really\n").expect("write");
        materialise(&d.join("nx"), &cant, "9.9.9", "t").expect("materialised");
        let e = verify(&d.join("nx"), "9.9.9").expect_err("refused");
        assert!(e.contains("will not run"), "{}", e);
        assert!(e.contains("noexec"), "{}", e);
        assert!(!e.contains("If this machine is not x86_64"), "{}", e);

        let _ = fs::remove_dir_all(&d);
    }

    /// The whole materialise/refresh cycle, including the two things a stale tree
    /// depends on: an old file is pruned, and a changed file is reported REPLACED.
    #[test]
    fn materialise_writes_refreshes_prunes_and_reports_what_it_replaced() {
        let d = scratch("mat");
        let tree = d.join("tree");
        // Any readable file will do as the "binary": verify() is not called here.
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, b"#!/bin/sh\nexit 0\n").expect("write");

        let lines = materialise(&tree, &exe, "0.1.0", "t").expect("materialised");
        assert!(lines.iter().any(|l| l.contains("created")), "{:?}", lines);
        for (rel, text) in crate::embedded::MANIFESTS {
            assert_eq!(fs::read(tree.join(rel)).expect("written"), text.as_bytes());
        }
        assert_eq!(fs::metadata(tree.join(BIN)).expect("copied").permissions().mode() & 0o777, 0o755);

        // A file an "older version" generated, recorded in the marker.
        fs::write(tree.join("hooks/extra.json"), b"{}").expect("write");
        fs::write(
            marker_path(&tree),
            marker_text("0.0.1", "t", &[".claude-plugin/plugin.json", "hooks/hooks.json", BIN, "hooks/extra.json"]),
        )
        .expect("write");
        // ...and a manifest the user edited under us.
        fs::write(tree.join(crate::embedded::HOOKS_JSON_PATH), b"edited\n").expect("write");

        let lines = materialise(&tree, &exe, "0.1.0", "t").expect("refreshed");
        assert!(lines.iter().any(|l| l.contains("refreshed")), "{:?}", lines);
        assert!(lines.iter().any(|l| l.contains("REPLACED, was 7 bytes")), "{:?}", lines);
        assert!(lines.iter().any(|l| l.contains("pruned:   hooks/extra.json")), "{:?}", lines);
        assert!(!tree.join("hooks/extra.json").exists());
        assert_eq!(
            fs::read(tree.join(crate::embedded::HOOKS_JSON_PATH)).expect("rewritten"),
            crate::embedded::HOOKS_JSON.as_bytes()
        );
        let m = read_marker(&tree).expect("present").expect("parses");
        assert_eq!(m.version, b"0.1.0");
        assert_eq!(m.files.len(), 3);

        let _ = fs::remove_dir_all(&d);
    }
}
