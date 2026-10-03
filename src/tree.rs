//! The generated plugin tree: the directory Claude Code actually loads.
//!
//! `hooks/hooks.json` and `.claude-plugin/plugin.json` are SOURCE. They are tracked
//! files you read, diff and edit, and they reach a running session the same way
//! `src/main.rs` does - through a build. `scripts/build.sh` compiles them into the
//! binary with `include_str!` (see `src/embedded.rs`) and `install` writes them back
//! out HERE, into a directory this module owns. The plugin directory is build
//! output, exactly like `bin/`.
//!
//! WHY, and it is not a preference. The checkout used to BE the plugin directory:
//! `<config>/skills/claude-tabstatus` was a symlink to the clone. So `git checkout`
//! of a branch with a broken `hooks.json` broke every prompt in every running
//! session, instantly, with no deploy step anywhere in the story - and `git pull`
//! did something subtler and worse, because `bin/` is gitignored: you got the NEW
//! hooks.json live at once while `bin/` still held the OLD binary, so new hook edges
//! pointed at a binary that had never heard of them. A generated tree makes both
//! states unreachable: one rebuild plus one `install` moves the manifests and the
//! binary together, and nothing reaches a session until you ask for it.
//!
//! Ownership is asserted by POSITIVE EVIDENCE, never inferred: a tree we generated
//! carries `.tabstatus-generated`, written by this module, and a directory without
//! it is refused rather than written into. The marker has exactly ONE job now - do
//! not clobber a directory that is not ours - and it is no longer a mode
//! discriminator, because there is only one shape of plugin directory left. A `.git`
//! in or above the target is refused a second time even if a marker somehow appears
//! there, which is what keeps a checkout from becoming a plugin directory again.
//!
//! Inside a tree we do own, the embedded bytes WIN unconditionally. "On-disk wins
//! when present" would make an upgrade silently do nothing: a newer binary installed
//! over an older tree would keep the old hooks.json, so a release that adds a
//! twelfth hook edge would install cleanly and that edge would never fire, with
//! nothing anywhere saying why. Every generated file is therefore rewritten, files a
//! newer version dropped are pruned from the marker's list, and a file whose bytes
//! CHANGED is named - overwriting is right here, doing it silently is not.
//!
//! EVERY WRITE IS A RENAME, because the tree is LIVE WIRING: hooks fire constantly,
//! and `bin/tabstatus` is the file eleven of them exec. `remove_file` then `copy`
//! would leave that path absent for the length of a 680 KB copy, and a hook that
//! execs a missing file exits 127 - which for `PreToolUse` BLOCKS A TOOL. A
//! same-directory temp file plus `rename(2)` means every path always resolves to the
//! whole old file or the whole new one. It also removes the reason the unlink was
//! there: `fs::copy` onto a running executable is ETXTBSY, where a rename leaves
//! every running process on its own inode.
//!
//! Windows will not rename over a file something is running, so there the binary's
//! last step is `sys::replace_running`: the running file renamed aside, the new one
//! renamed in, the old one deleted once nothing runs it. That leaves a window of two
//! renames with no `bin\tabstatus.exe` at all, and the seam says so where it is done.
//!
use crate::json;
use crate::sys;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// The proof of ownership. A dotfile so it does not read as plugin content;
/// `claude plugin validate --strict` is happy with it at the tree root.
pub const MARKER: &str = ".tabstatus-generated";

const MARKER_VERSION: i32 = 1;

/// The binary `hooks/hooks.json` invokes, relative to the plugin root. A real copy
/// of the binary that installed the tree - not a link back into `bin/` - so the tree
/// is self-contained and a `git checkout` cannot change what a session executes.
///
/// `bin/tabstatus.exe` on Windows, where `CreateProcess` wants the suffix; the
/// `.../bin/tabstatus` that hooks.json names still reaches it, because Git Bash
/// resolves a missing `.exe` the way Windows programs expect.
pub const BIN: &str =
    if std::env::consts::EXE_SUFFIX.is_empty() { "bin/tabstatus" } else { "bin/tabstatus.exe" };

/// `<tree>/bin/tabstatus` as a path to open or print. [`BIN`] is the marker's
/// spelling, always with `/`, and joined whole onto a Windows tree it printed
/// `...\claude-tabstatus\bin/tabstatus.exe`. Joined a component at a time the
/// separator is the platform's own; on Unix it is the same bytes as `tree.join(BIN)`.
pub fn bin_path(tree: &Path) -> PathBuf {
    BIN.split('/').fold(tree.to_path_buf(), |p, c| p.join(c))
}

/// A marker-spelled relative path (always `/`) as a report line prints it: with the
/// platform's own separator, so `bin\tabstatus.exe` on Windows sits beside the
/// absolute paths around it. The same bytes on Unix.
pub fn shown(rel: &str) -> String {
    rel.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// Every path this module writes, in the order it writes them. `bin/tabstatus`
/// FIRST, which is the opposite of what it was and for a reason that only applies
/// now that the tree is live wiring.
///
/// When the tree was never loaded by anything, "manifests first, binary last" kept
/// it from being loadable-but-unrunnable. A refresh of the LIVE tree has no such
/// quiet moment: whatever order we pick, one hook event somewhere may see a
/// half-updated pair. Of the two possible in-between states only one is harmless. A
/// NEW binary with OLD manifests paints every edge the old hooks.json can name, and
/// it is the newer half that has to understand the older words. An OLD binary with
/// NEW manifests is the broken one: a new edge word reaches `edge.rs`, which maps
/// what it does not recognise to `Edge::Unknown` and paints the IDLE glyph, so the
/// tab goes quietly wrong rather than loudly. Binary first, therefore - and on a
/// first install into an empty directory the order is indifferent, so it is right in
/// both cases.
fn generated_paths() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![BIN];
    v.extend(crate::embedded::MANIFESTS.iter().map(|(p, _)| *p));
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
    out.push_str("  \"written_by\": \"tabstatus install\",\n");
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
/// read as ours, which `doctor` reports and `install` treats as "prune nothing".
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
    // Joined a component at a time, so the separator is the platform's own.
    match crate::config::home_var() {
        Some(h) => Ok(PathBuf::from(h).join(".local").join("share").join("claude-tabstatus")),
        None => Err("neither XDG_DATA_HOME nor HOME is set, so there is no default \
                     place for the plugin tree. Give one: tabstatus install --tree <dir>"
            .to_string()),
    }
}

/// Why this directory must not be the target. `None` means go ahead.
///
/// Four refusals, and each one exists because the alternative is a confusing failure
/// LATER: under `<config>/skills` the installer would be asked to link a directory to
/// itself, a checkout in or above the target means tracked files that are not ours to
/// rewrite, and a non-empty directory with no marker is somebody else's.
///
/// The upward walk is what keeps the door this whole change closes from reopening:
/// `install --tree <somewhere inside the checkout>` would otherwise drop an untracked
/// plugin tree into the working tree, and the checkout would be the live plugin
/// directory again. It looks for a `.git` beside a `.claude-plugin/plugin.json`
/// specifically, not for any `.git` at all: plenty of people keep `$HOME` itself in
/// git, and the default tree lives three levels under it.
///
/// "Under `<config>/skills`" is asked through `sys::is_within`: a component prefix on
/// Unix, and on Windows where the directory would really land - NTFS takes
/// `<config>\SKILLS`, a short name and a `\\?\` prefix to the same place, and the
/// install that walked past this refusal wrote the tree AT the link path, set the env
/// key, and only then found it could not make the link.
pub fn refuse_target(tree: &Path, skills: &Path) -> Option<String> {
    if sys::is_within(tree, skills) {
        return Some(format!(
            "{} is under {}, which is where install puts the {} TO the tree. \
             A tree there would make install {} a directory to itself. Pick \
             somewhere else, or pass no directory at all for the default.",
            tree.display(),
            skills.display(),
            sys::DIR_LINK,
            sys::DIR_LINK
        ));
    }
    if tree.join(".git").exists() {
        return Some(format!(
            "{} has a .git in it, so it is a checkout and its files are not this \
             command's to rewrite. The plugin tree is build output; pick a directory \
             outside your checkout, or pass none at all for the default. Nothing has \
             been changed.",
            tree.display()
        ));
    }
    if let Some(co) = checkout_above(tree) {
        return Some(format!(
            "{} is inside the claude-tabstatus checkout at {}. The plugin tree is \
             BUILD OUTPUT and must not live in the source tree - that is the wiring \
             this design exists to undo. Pick a directory outside it, or pass none at \
             all for the default. Nothing has been changed.",
            tree.display(),
            co.display()
        ));
    }
    // A SYMLINK, asked about before `fs::metadata` follows it. Two reasons, and the
    // first is the errno this whole block exists to replace: a DANGLING link answers
    // NotFound to `fs::metadata`, reads as "absent, go ahead", and then
    // `create_dir_all` fails EEXIST - after the header has already announced the
    // repoint, so the operator is told the live link "will ->" somewhere it never
    // went. The second is a link to an EMPTY directory, which the old code accepted:
    // the tree lands in the link's target, the plugin link points at the link, and
    // `in_tree` then refuses to remove anything under it, so `uninstall` can never
    // take it down. Neither is wanted; both are one refusal.
    if let Ok(md) = fs::symlink_metadata(tree) {
        if md.file_type().is_symlink() {
            let t = fs::read_link(tree).unwrap_or_else(|_| PathBuf::from("?"));
            return Some(format!(
                "{} is a symlink (-> {}), not a directory. The tree is written and \
                 removed file by file, and nothing reached through a link is provably \
                 inside it - so pass the directory itself. Nothing has been changed.",
                tree.display(),
                t.display()
            ));
        }
    }
    // Absent is exactly what we want. Anything else that is not a READABLE
    // DIRECTORY is a named refusal here rather than an errno from create_dir_all
    // three lines later: `cannot create <path>/.claude-plugin: File exists` reads
    // like a bug in this program, prints after the first report line as though work
    // had begun, and lacks the "Nothing has been changed." every real refusal ends
    // with.
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
                         not written by `tabstatus install` and is not ours to \
                         overwrite.{} Pass a different directory with `--tree`. Nothing \
                         has been changed.",
                        tree.display(),
                        MARKER,
                        if safe_to_suggest_removing(tree) {
                            " If it is an old plugin tree that lost its marker, nothing here can \
                             tell its files from yours: look at what it holds, and empty it \
                             yourself before re-running."
                        } else {
                            ""
                        }
                    ))
                }
            }
        },
    }
}

/// Is emptying this directory by hand a thing to suggest at all?
///
/// The refusal above is correct and writes nothing, but it is the one message in this
/// program a hurried operator acts on, and its subject is whatever they typed after
/// `--tree`. It used to end with `rm -rf <that>`: `--tree /` printed `rm -rf /`,
/// `--tree ~` printed it on the home directory, and once bounded to "looks like ours"
/// it still printed it for a directory that by definition carries no marker - nothing
/// here can tell its files from the operator's - and, through a "sits beside the
/// default tree" clause, for `~/.local/share/claude` and every other sibling in there.
///
/// So no refusal carries a command any more. This decides only whether the refusal
/// adds "if it is an old plugin tree, look and empty it yourself": a directory named
/// `claude-tabstatus`, never `$HOME`, never anything less than three levels down.
fn safe_to_suggest_removing(tree: &Path) -> bool {
    if tree.components().filter(|c| matches!(c, Component::Normal(_))).count() < 3 {
        return false;
    }
    if crate::config::home_var().map(|h| Path::new(&h) == tree).unwrap_or(false) {
        return false;
    }
    tree.file_name() == Some(OsStr::new("claude-tabstatus"))
}

/// A claude-tabstatus checkout at or above `from`: a `.git` beside a
/// `.claude-plugin/plugin.json`. Bounded, and it never answers for `from` itself -
/// `refuse_target` checks that case separately and with its own wording.
fn checkout_above(from: &Path) -> Option<PathBuf> {
    let mut dir = from.parent().map(|p| p.to_path_buf());
    for _ in 0..24 {
        let d = dir?;
        if d.join(".git").exists() && d.join(".claude-plugin/plugin.json").is_file() {
            return Some(d);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

// --- materialising -----------------------------------------------------------

/// Write the running binary and the embedded manifests into `tree`, prove the copy
/// runs BEFORE it becomes the live one, prune what a previous run generated and this
/// one does not, and record what was written.
///
/// Returns the report lines rather than printing them, so the one place that
/// writes to stdout stays `manage::say`.
pub fn materialise(tree: &Path, exe: &Path, version: &str, target: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let existed = tree.exists();
    let prior = read_marker(tree);

    fs::create_dir_all(tree).map_err(|e| format!("cannot create {}: {}", tree.display(), e))?;
    out.push(format!(
        "tree:     {} ({}) - build output, like bin/",
        tree.display(),
        if existed { "refreshed" } else { "created" }
    ));

    // The marker FIRST, listing what this run intends to write, and never rewritten.
    // Ownership has to be claimed BEFORE the files, because the marker is the only
    // evidence `refuse_target` accepts: a run killed anywhere in the window below -
    // ENOSPC during the 680 KB binary copy on a small VM, a dropped ssh, an OOM, a
    // Ctrl-C - would otherwise leave both manifests and no marker, which the next run
    // classifies as somebody else's directory and refuses FOREVER. Marker first makes
    // every partial state re-enterable by construction, which is the property the
    // marker exists to provide, and it costs nothing: `read_marker` already tolerates
    // a marker it cannot parse and a stale file list.
    //
    // ONE write, not a claim and a later correction: the list cannot change between
    // here and the end, because every step below returns Err rather than carrying
    // on. The marker already exists if the following binary copy fails; copied
    // file timestamps need not reflect this operation order.
    let now = generated_paths();
    // `in_tree` even for a root-level file: it is the one place that proves the tree
    // itself is a real directory rather than a link to one, and it runs before the
    // first write.
    let mp = in_tree(tree, MARKER.as_bytes(), false)?;
    crate::manage::write_atomic(&mp, &marker_text(version, target, &now), 0o644)?;
    out.push(format!(
        "marker:   {} ({} files, written first so an interrupted run is resumable)",
        MARKER,
        now.len()
    ));

    // The BINARY first of the generated files, and verified before it is renamed
    // into place - see `generated_paths` for the ordering and the module header for
    // why every write here is a rename.
    out.extend(write_binary(tree, exe, version)?);

    for (rel, text) in crate::embedded::MANIFESTS {
        // Resolves the path AND creates its directories, one component at a time, so
        // the write cannot land outside the tree through a symlinked component.
        let dst = in_tree(tree, rel.as_bytes(), true)?;
        let before = fs::read(&dst).ok();
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
        out.push(format!("wrote:    {} ({} bytes, {})", shown(rel), text.len(), what));
    }

    // Prune what an OLDER version generated and this one no longer does, so an
    // upgrade cannot leave a file behind that nothing rewrites.
    if let Some(Ok(m)) = &prior {
        for f in &m.files {
            let name = String::from_utf8_lossy(f).to_string();
            if now.contains(&name.as_str()) {
                continue;
            }
            // A path we cannot prove is inside the tree is named and skipped, never
            // unlinked: prune is one of the two places here that removes anything.
            let p = match in_tree(tree, f, false) {
                Ok(p) => p,
                Err(why) => {
                    out.push(format!("pruned:   {} NOT taken - {}", shown(&name), why));
                    continue;
                }
            };
            if p.is_file() && fs::remove_file(&p).is_ok() {
                out.push(format!("pruned:   {} (generated by an older version)", shown(&name)));
                // And the directory it was the last thing in. Without this an older
                // version's `old/legacy.json` leaves an empty `old/` behind that no
                // later marker lists, so `remove` never prunes it and `uninstall`
                // cannot take the tree down - it reported "which is now empty" over a
                // directory that survived.
                prune_empty_parents(tree, &p);
            }
        }
    }

    Ok(out)
}

/// `<tree>/bin/tabstatus`: the one file in here that a live session EXECS, so it is
/// the one write that has to be got exactly right.
///
/// Three answers, and two of them touch nothing. Running FROM the tree is the
/// ordinary re-install through the installed copy. Bytes already IDENTICAL is the
/// commonest case of all - `install` run twice, or run after a build that changed
/// nothing - and short-circuiting it means a no-op install genuinely does not
/// disturb live wiring, instead of renaming a fresh inode over a file eleven hooks
/// are executing for no reason at all.
///
/// Otherwise: copy to a same-directory temp file, chmod it, EXEC IT, and only then
/// rename it onto `bin/tabstatus`. Verifying the temp copy rather than the installed
/// one is what makes the rename safe to be the point of no return: nothing becomes
/// live wiring here without having been run first.
fn write_binary(tree: &Path, exe: &Path, version: &str) -> Result<Vec<String>, String> {
    let dst = in_tree(tree, BIN.as_bytes(), true)?;
    let same_file = fs::canonicalize(&dst).ok() == fs::canonicalize(exe).ok() && dst.exists();
    if same_file {
        return Ok(vec![
            format!("wrote:    {} (unchanged - it IS the running binary)", shown(BIN)),
            format!("verify:   {}", verify(&dst, version)?),
        ]);
    }
    if let (Ok(a), Ok(b)) = (fs::read(exe), fs::read(&dst)) {
        if a == b {
            return Ok(vec![
                format!("wrote:    {} ({} bytes, unchanged - identical bytes)", shown(BIN), b.len()),
                format!("verify:   {}", verify(&dst, version)?),
            ]);
        }
    }
    let existed = dst.exists();
    let tmp = scratch_beside(&dst);
    let _ = fs::remove_file(&tmp);
    let n = fs::copy(exe, &tmp)
        .map_err(|e| format!("cannot copy {} to {}: {}", exe.display(), tmp.display(), e))?;
    if let Err(e) = sys::set_mode(&tmp, 0o755) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot chmod {}: {}", tmp.display(), e));
    }
    // Run it BEFORE the rename: a copy that will not exec must never become the file
    // the hooks invoke, and the temp file is removed rather than left behind.
    let said = match verify(&tmp, version) {
        Ok(said) => said,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // rename(2), not remove-then-copy: the old path resolves to the whole old binary
    // until this instant and to the whole new one afterwards, so no hook event can
    // ever exec a file that is not there. Every process already running the old
    // inode keeps it, which is also why this is not ETXTBSY. Windows refuses to
    // replace a file something is running, so there the seam renames the running one
    // aside first - see `sys::replace_running` for that sequence and its window.
    let aside = sys::replace_running(&tmp, &dst).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot move {} into place at {}: {}", tmp.display(), dst.display(), e)
    })?;
    let mut out = vec![
        format!(
            "wrote:    {} ({} bytes, {} from {})",
            shown(BIN),
            n,
            if existed { "REPLACED, copied" } else { "copied" },
            exe.display()
        ),
        format!("verify:   {}", said),
    ];
    // Something this install leaves on the disk, so it is named: the old binary was
    // running, and could only be renamed out of the way.
    if let Some(a) = aside {
        out.push(format!(
            "aside:    the old {} was running, so it was renamed to {}",
            shown(BIN),
            a.display()
        ));
        out.push("          a later install or uninstall removes it once nothing runs it".to_string());
    }
    Ok(out)
}

/// Every directory between `p` and `tree` that `p` was the last entry of, deepest
/// first. `remove_dir`, never `remove_dir_all`, so a directory holding anything else
/// survives with its contents; ENOTEMPTY just stops the walk.
///
/// It exists because an emptied directory is invisible to everything downstream: a
/// later marker does not list it, so `remove` never prunes it, and `uninstall` then
/// reports a tree as empty while leaving the directory on disk.
fn prune_empty_parents(tree: &Path, p: &Path) {
    let mut at = p.parent().map(Path::to_path_buf);
    while let Some(d) = at {
        if d == tree || !d.starts_with(tree) {
            return;
        }
        if fs::remove_dir(&d).is_err() {
            return;
        }
        at = d.parent().map(Path::to_path_buf);
    }
}

/// A temp name beside `p`, in the same directory so the rename is atomic, and pid
/// suffixed so two concurrent installs cannot collide. Deliberately the same shape
/// `manage::scratch_pid` recognises, so a killed run's litter is swept.
pub fn scratch_beside(p: &Path) -> PathBuf {
    let dir = p.parent().unwrap_or(Path::new("."));
    let name = p.file_name().map(|n| n.as_encoded_bytes().to_vec()).unwrap_or_default();
    let mut tmp = b".".to_vec();
    tmp.extend_from_slice(&name);
    tmp.extend_from_slice(format!(".cctab-tmp.{}", std::process::id()).as_bytes());
    dir.join(sys::os_string_from_vec(tmp))
}

/// A marker path we are willing to unlink: relative, no `..`, no leading `/`. The
/// marker is ours, but it is still a file on disk that something could have
/// edited, and prune is the one place here that REMOVES anything.
///
/// Each Normal component must also parse as exactly itself when it stands alone,
/// because that is how [`in_tree`] uses it: `PathBuf::push` one component at a
/// time. Windows reads a drive prefix only at the START of a path, so
/// `hooks/C:evil` splits into two Normal components - and pushing `C:evil` then
/// REPLACES the buffer with a path on drive C:, outside the tree. On Unix a Normal
/// component always parses as itself, so this refuses nothing there.
///
/// TEXT only, and that is why it is not enough on its own - see [`in_tree`].
fn safe_relative(p: &[u8]) -> bool {
    let s = sys::os_str_from_bytes(p);
    !p.is_empty()
        && !p.starts_with(b"/")
        && Path::new(&*s).components().all(|c| match c {
            Component::Normal(n) => Path::new(n).components().eq([c]),
            _ => false,
        })
}

/// A marker-listed relative path resolved to a place we are CERTAIN is inside
/// `tree`. The only way those paths are constructed: every write and every unlink in
/// this module goes through here.
///
/// `safe_relative` checks the string, and a string made entirely of Normal
/// components still resolves through whatever is on disk. If `<tree>/hooks` is a
/// symlink to somebody's directory, `tree.join("hooks/hooks.json")` names a file in
/// THAT directory - and both operations this module performs would reach it:
/// `remove_file` unlinks it, and `write_atomic`, whose temp file is created in
/// `path.parent()`, creates and renames inside it. Outside the tree, silently, and
/// reported as inside it. [`walk`] already refuses to wander through such a link
/// (`symlink_metadata`, so `bin` is one entry rather than a directory); this is the
/// same rule applied where it matters.
///
/// So the tree root and every DIRECTORY component of `rel` must be a real directory.
/// The leaf is not checked and need not be: `remove_file` on a symlink takes the link
/// itself, a rename onto one replaces it, and the temp file sits in a parent this
/// function has just proved real.
///
/// `create` builds the missing components with `fs::create_dir` one at a time, never
/// `create_dir_all`, which accepts an existing symlink-to-directory as "already
/// there" and would reopen the hole from the other side.
///
/// The tree's own ANCESTORS are deliberately NOT checked. `~/.local` is a symlink on
/// any machine with a dotfile manager, and the tree is where the operator pointed it;
/// what this defends is the boundary of the tree, not the route to it.
fn in_tree(tree: &Path, rel: &[u8], create: bool) -> Result<PathBuf, String> {
    let name = String::from_utf8_lossy(rel).into_owned();
    if !safe_relative(rel) {
        return Err(format!(
            "{} lists {:?}, which is not a path inside the tree. Refusing to touch it.",
            MARKER, name
        ));
    }
    if fs::symlink_metadata(tree).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(format!(
            "{} is a symlink, not a directory, so nothing under it is provably inside \
             the plugin tree. Pass the directory itself with --tree.",
            tree.display()
        ));
    }
    let relp = PathBuf::from(sys::os_string_from_vec(rel.to_vec()));
    let last = relp.components().count().saturating_sub(1);
    let mut at = tree.to_path_buf();
    for (i, comp) in relp.components().enumerate() {
        at.push(comp);
        if i == last {
            break;
        }
        match fs::symlink_metadata(&at) {
            Ok(md) if md.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is not a directory, so {} is not provably inside the plugin tree \
                     {} - following it would write to, or unlink, a file outside. \
                     Refusing.",
                    at.display(),
                    name,
                    tree.display()
                ))
            }
            // Absent: `create` makes it, and a removal has nothing to take down there.
            Err(_) if !create => continue,
            Err(_) => fs::create_dir(&at)
                .map_err(|e| format!("cannot create {}: {}", at.display(), e))?,
        }
    }
    Ok(at)
}

/// What `remove` leaves behind in `tree` given the marker's list `known`.
#[cfg(test)]
fn extra_files(tree: &Path, known: &[Vec<u8>]) -> Vec<String> {
    walk(tree, known).left()
}

/// Why a walk of a tree did not see all of it - and so cannot vouch for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// It stopped at its bound.
    TooMany,
    /// A directory, or an entry in one, could not be read.
    Unreadable,
}

impl Unread {
    /// The reason, as the start of a sentence the caller ends.
    pub fn why(self) -> &'static str {
        match self {
            Unread::TooMany => "It holds more entries than a generated tree ever does",
            Unread::Unreadable => "Some of what it holds could not be read",
        }
    }
}

/// What a non-directory entry is, as far as anything here cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Link,
    Other,
}

/// What one bounded, link-blind walk of a tree saw: the single reading behind
/// [`remove`]'s `left` and [`survey`] (whether the directory command may be offered),
/// so the two cannot disagree about what is in there.
struct Walk {
    /// Non-directory entries the marker does not list and this tool did not leave, as
    /// tree-relative `/`-joined paths, a link marked as one.
    foreign: Vec<String>,
    /// Plain files bearing one of this tool's own scratch names - a killed run's temp
    /// copy, or on Windows a binary set aside while it ran - tree-relative.
    litter: Vec<PathBuf>,
    /// Marker-listed non-directory entries, in the exact spelling read from disk.
    listed: Vec<(Vec<u8>, Kind)>,
    unread: Option<Unread>,
}

impl Walk {
    /// Files inside a tree we own that the marker does NOT list, as tree-relative
    /// paths, so `uninstall` can name what it is LEAVING BEHIND: the foreign entries
    /// and this tool's litter, which `remove` does not take either - `install`'s sweep
    /// does.
    ///
    /// `install` is scrupulous - prune only ever touches the marker's list, so a few
    /// refresh runs teach the operator by behaviour that their own files are safe in
    /// that directory. `uninstall` keeps that promise rather than breaking it at the
    /// last moment: these paths stay, and the report names them so a directory left in
    /// `~/.local/share` is not something to find by accident.
    fn left(&self) -> Vec<String> {
        let mut out = self.foreign.clone();
        out.extend(self.litter.iter().map(|p| String::from_utf8_lossy(&slash_joined(p)).into_owned()));
        out.sort();
        out
    }
}

/// `name (a link)` for a link, so a report never calls one a file somebody wrote.
fn entry_name(rel: &[u8], kind: Kind) -> String {
    let name = String::from_utf8_lossy(rel);
    if kind == Kind::Link {
        format!("{} (a link)", name)
    } else {
        name.into_owned()
    }
}

/// A name only this tool gives a file: `manage::scratch_pid`'s two shapes, which every
/// `install` sweeps, and `sys::is_set_aside`'s, which it sweeps on Windows.
fn is_litter(name: &OsStr) -> bool {
    name.to_str().is_some_and(|n| crate::manage::scratch_pid(n).is_some() || sys::is_set_aside(n))
}

fn walk(tree: &Path, known: &[Vec<u8>]) -> Walk {
    let mut w = Walk { foreign: Vec::new(), litter: Vec::new(), listed: Vec::new(), unread: None };
    let mut stack = vec![PathBuf::new()];
    // Bounded so a pathological tree cannot make `uninstall` hang, and symlink_metadata
    // so a link out of the tree is one entry rather than a directory to wander into.
    // What it could not read, and what lies past the bound, is recorded as such: a walk
    // that did not see everything must never read as "nothing else is in there".
    let mut budget = 4096usize;
    'walk: while let Some(rel) = stack.pop() {
        let rd = match fs::read_dir(tree.join(&rel)) {
            Ok(rd) => rd,
            Err(_) => {
                w.unread.get_or_insert(Unread::Unreadable);
                continue;
            }
        };
        for e in rd {
            if budget == 0 {
                w.unread = Some(Unread::TooMany);
                break 'walk;
            }
            budget -= 1;
            let Ok(e) = e else {
                w.unread.get_or_insert(Unread::Unreadable);
                continue;
            };
            let child = rel.join(e.file_name());
            let Ok(md) = fs::symlink_metadata(tree.join(&child)) else {
                w.unread.get_or_insert(Unread::Unreadable);
                continue;
            };
            if md.is_dir() {
                stack.push(child);
                continue;
            }
            let kind = if md.file_type().is_symlink() {
                Kind::Link
            } else if md.is_file() {
                Kind::File
            } else {
                Kind::Other
            };
            let bytes = slash_joined(&child);
            // The marker is ours whether or not it lists itself.
            if bytes == MARKER.as_bytes() {
                continue;
            }
            if known.contains(&bytes) {
                w.listed.push((bytes, kind));
            } else if kind == Kind::File && is_litter(&e.file_name()) {
                w.litter.push(child);
            } else {
                w.foreign.push(entry_name(&bytes, kind));
            }
        }
    }
    w.foreign.sort();
    w.litter.sort();
    w
}

/// What a generated tree holds, read and never changed, for the one question asked
/// before printing the delete-the-directory command about a tree this run LEAVES in
/// place: does it hold only what its marker lists?
///
/// The notion of "ours" is [`remove`]'s, built from the same pieces - the marker's list,
/// [`walk`] for everything else, [`in_tree`] for whether a
/// listed path is provably inside - plus two rules `remove` does not need. A
/// marker-listed path that is a LINK is not ours, because install writes only plain
/// files and the directory command must never be handed a link to follow. And a plain
/// file with one of this tool's scratch names IS ours: `install` sweeps those.
pub struct Survey {
    /// What "delete only these" may name: the marker-listed paths that are provably
    /// inside the tree and plain files at exactly that spelling, in the marker's order,
    /// then this tool's litter. Never the marker, which goes last and is the caller's.
    pub ours: Vec<PathBuf>,
    /// Every other non-directory entry, tree-relative, a link marked as one.
    pub foreign: Vec<String>,
    /// Marker-listed paths not provably inside the tree, each as [`in_tree`] says why.
    pub blocked: Vec<String>,
    /// Why the walk did not see everything, when it did not.
    pub unread: Option<Unread>,
}

impl Survey {
    /// The go-ahead for the delete-the-directory command.
    pub fn only_ours(&self) -> bool {
        self.unread.is_none() && self.foreign.is_empty() && self.blocked.is_empty()
    }
}

/// [`Survey`] a tree, or say why it cannot be: no marker, a marker that is not a plain
/// file or cannot be read, or a tree that is itself a link. Each is a tree whose files
/// nothing here can tell from anybody else's.
///
/// Cheap and bounded: [`walk`]'s capped walk, which never follows a link, and a
/// few `stat`s per marker entry. Install, uninstall and doctor run it; no hook does.
pub fn survey(tree: &Path) -> Result<Survey, String> {
    if fs::symlink_metadata(tree).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("It is a link, not a directory".to_string());
    }
    match fs::symlink_metadata(marker_path(tree)) {
        Ok(m) if m.is_file() => {}
        Ok(m) if m.file_type().is_symlink() => return Err(format!("Its {} is a link, not a file", MARKER)),
        Ok(_) => return Err(format!("Its {} is not a file", MARKER)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("It carries no {} any more", MARKER))
        }
        Err(_) => return Err(format!("Its {} cannot be read", MARKER)),
    }
    let known = match read_marker(tree) {
        Some(Ok(m)) => m.files,
        _ => return Err(format!("Its {} cannot be read", MARKER)),
    };
    let w = walk(tree, &known);
    let mut foreign = w.foreign.clone();
    let mut ours: Vec<PathBuf> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    for f in &known {
        let p = match in_tree(tree, f, false) {
            Ok(p) => p,
            Err(why) => {
                blocked.push(why);
                continue;
            }
        };
        // What the walk saw at exactly this spelling. Not a `stat` of `p`: on Windows
        // that answers for `NOTES.txt` when the marker says `notes.txt`, and the
        // operator's own file would be listed as ours to delete while being named as
        // theirs one line up.
        let kind = match w.listed.iter().find(|(b, _)| b == f) {
            Some((_, k)) => Some(*k),
            // Absent, a directory (whatever it holds was walked), or another spelling
            // of an entry the walk already named under its own.
            None if w.unread.is_none() => None,
            // The walk did not get there: ask the disk, so the list of ours is whole.
            None => match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => None,
                Ok(m) if m.is_file() => Some(Kind::File),
                Ok(m) if m.file_type().is_symlink() => Some(Kind::Link),
                Ok(_) => Some(Kind::Other),
                Err(_) => None,
            },
        };
        match kind {
            Some(Kind::File) => ours.push(p),
            // A link, or anything else install never writes.
            Some(k) => foreign.push(entry_name(f, k)),
            None => {}
        }
    }
    ours.extend(w.litter.iter().map(|rel| tree.join(rel)));
    foreign.sort();
    Ok(Survey { ours, foreign, blocked, unread: w.unread })
}

/// A tree-relative path in the marker's spelling: components joined by `/`, which
/// is what `Path::join` already produces on Unix. On Windows `join` uses `\`, and
/// comparing that against the marker's `bin/tabstatus.exe` would name every file we
/// generated as somebody else's.
fn slash_joined(rel: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for c in rel.components() {
        if !out.is_empty() {
            out.push(b'/');
        }
        out.extend_from_slice(c.as_os_str().as_encoded_bytes());
    }
    out
}

/// Run the copy we just made: ONE named refusal at install time instead of eleven
/// hooks silently failing in every future session. It is a probe, not a prediction,
/// so it needs to know nothing about the machine.
///
/// Takes the BINARY, not the tree, because `write_binary` points it at the temp copy:
/// the whole protection is that nothing is renamed into live wiring without having
/// been exec'd first.
///
/// It does NOT cover a wrong architecture, and must not claim to: the copy is
/// `current_exe`, so it is by construction the same architecture as the process
/// running this check, and on an aarch64 machine the kernel refuses the scp'd
/// binary with its own *Exec format error* before a line of this program runs. What
/// is left is real: a filesystem mounted `noexec`, a lost exec bit, a copy that is
/// not what we think it is.
pub fn verify(bin: &Path, version: &str) -> Result<String, String> {
    let out = std::process::Command::new(bin).arg("version").output().map_err(|e| {
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

// --- removing a tree we own ---------------------------------------------------

/// What `remove` did: the generated files it took, the paths it left behind, and
/// whether the directory itself is gone.
pub struct Removal {
    pub removed: usize,
    pub left: Vec<String>,
    /// Marker-listed paths that were NOT taken because they could not be proved to
    /// be inside the tree - a symlinked component, or a marker somebody edited.
    /// Named by the report rather than swallowed: they are still on disk.
    pub blocked: Vec<String>,
    /// Marker-listed files that are inside the tree and still there because removing
    /// them FAILED, each as its full path and the error - on Windows, the binary a hook
    /// is running right now. While any is left the marker stays too.
    pub failed: Vec<(PathBuf, String)>,
    /// Why `left` may not be everything, when the walk behind it did not see the whole
    /// tree: an empty `left` then proves nothing, and the report must not say it does.
    pub unread: Option<Unread>,
    pub dir_gone: bool,
}

/// Remove a tree we generated, CONSERVATIVELY: only the files the marker lists, then
/// the marker, then any directory those left empty.
///
/// The marker goes only when every file it lists did. A tree that still holds one of
/// ours but no marker is the shape `refuse_target` refuses forever - somebody else's
/// non-empty directory - so a single file that could not be removed (a hook running
/// the binary, on Windows) would turn into an install that needs `rm -rf` to proceed.
/// Kept, the marker leaves it a tree `install` reuses and a later removal finishes.
///
/// `remove_dir_all` on a path this program derived from a symlink is the one thing an
/// uninstaller of a personal tool has no business doing, and now that the generated
/// tree is where the plugin always lives, `uninstall` removes it by DEFAULT - so the
/// blast radius has to be the marker's list and nothing else. Anything else in there
/// is named and left; `rm -rf <path>` is the honest instruction for somebody who
/// wants the whole directory gone.
pub fn remove(tree: &Path) -> Result<Removal, String> {
    let known: Vec<Vec<u8>> = match read_marker(tree) {
        Some(Ok(m)) => m.files,
        // An unparseable marker means we cannot prove which files are ours, so none
        // are removed and everything is named. The marker itself still goes: it is
        // ours whatever it says.
        _ => Vec::new(),
    };
    let w = walk(tree, &known);
    let (left, unread) = (w.left(), w.unread);
    let mut removed = 0usize;
    let mut blocked: Vec<String> = Vec::new();
    let mut failed: Vec<(PathBuf, String)> = Vec::new();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for f in &known {
        // The ONE way a path to unlink is built here. A textual check is not enough:
        // `<tree>/bin` replaced by a symlink would have had this loop unlink a file in
        // somebody else's directory and call it a generated file of ours.
        let p = match in_tree(tree, f, false) {
            Ok(p) => p,
            Err(why) => {
                blocked.push(why);
                continue;
            }
        };
        if fs::symlink_metadata(&p).is_ok() {
            match fs::remove_file(&p) {
                Ok(()) => removed += 1,
                Err(e) => failed.push((p.clone(), e.to_string())),
            }
        }
        if let Some(d) = p.parent() {
            if d != tree && d.starts_with(tree) && !dirs.contains(&d.to_path_buf()) {
                dirs.push(d.to_path_buf());
            }
        }
    }
    if failed.is_empty() {
        let _ = fs::remove_file(marker_path(tree));
    }
    // Deepest first, and `remove_dir` not `remove_dir_all`, so a directory holding
    // anything we did not write survives with its contents.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in &dirs {
        let _ = fs::remove_dir(d);
    }
    let dir_gone = fs::remove_dir(tree).is_ok();
    Ok(Removal { removed, left, blocked, failed, unread, dir_gone })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test below that calls `materialise` or `verify` holds this, and the reason
    /// is ETXTBSY rather than shared state.
    ///
    /// `write_binary` copies the binary and then EXECS the copy, which is the whole
    /// safety property. `execve` returns ETXTBSY while any process holds the file open
    /// for writing - and `fork` duplicates open descriptors, so in a MULTITHREADED
    /// process a `Command` spawned by thread B, between its fork and its exec, is
    /// briefly holding thread A's write descriptor on A's temp copy. Cargo's harness
    /// runs these tests as threads of one process, so two of them materialising at once
    /// hit that window; the product never can, because `install` is one process doing
    /// one copy and one exec with no other thread in it. Serialising the tests is
    /// therefore the honest fix - the alternative is a retry loop in the product for a
    /// race the product does not have.
    #[cfg(unix)]
    static FORKING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn forking() -> std::sync::MutexGuard<'static, ()> {
        FORKING.lock().unwrap_or_else(|e| e.into_inner())
    }

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
        assert_eq!(files, vec![".claude-plugin/plugin.json", BIN, "hooks/hooks.json"]);
        // The WRITE order is the opposite of the marker's sorted list, and it is
        // load-bearing: the binary lands first, so the only in-between state a live
        // hook can see is a new binary with old manifests - never an old binary
        // painting a newer hooks.json's edge word as idle.
        assert_eq!(generated_paths()[0], BIN);
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
            assert!(why.contains(&format!("{} a directory to itself", sys::DIR_LINK)), "{}", why);
        }
        // A checkout, even with a marker dropped in it: tracked files are not ours.
        let co = d.join("checkout");
        fs::create_dir_all(co.join(".git")).expect("mkdir");
        fs::write(marker_path(&co), b"{}").expect("write");
        assert!(refuse_target(&co, &skills).expect("refused").contains(".git"));
        // ...and ANYWHERE INSIDE it, which is the door that let the checkout become
        // the plugin directory again. A `.git` alone is not enough - `$HOME` itself is
        // in git on plenty of machines and the default tree is three levels under it -
        // so the walk looks for a claude-tabstatus checkout specifically.
        fs::create_dir_all(co.join(".claude-plugin")).expect("mkdir");
        fs::write(co.join(".claude-plugin/plugin.json"), b"{}").expect("write");
        let inside = co.join("build/tree");
        let why = refuse_target(&inside, &skills).expect("refused");
        assert!(why.contains("is inside the claude-tabstatus checkout"), "{}", why);
        assert!(why.contains("BUILD OUTPUT"), "{}", why);
        // A plain git repo that is not this one does not block a tree beneath it.
        let dots = d.join("dotfiles");
        fs::create_dir_all(dots.join(".git")).expect("mkdir");
        assert!(refuse_target(&dots.join(".local/share/claude-tabstatus"), &skills).is_none());
        // Non-empty and unmarked.
        let other = d.join("other");
        fs::create_dir_all(&other).expect("mkdir");
        fs::write(other.join("something"), b"x").expect("write");
        let why = refuse_target(&other, &skills).expect("refused");
        assert!(why.contains(MARKER), "{}", why);
        assert!(why.contains("not written by `tabstatus install`"), "{}", why);
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
    #[cfg(unix)]
    fn a_tree_a_killed_run_left_behind_is_still_ours_to_finish() {
        let _serial = forking();
        let d = scratch("partial");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, fake_exe("0.1.0")).expect("write");
        sys::set_mode(&exe, 0o755).expect("chmod");
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

        // Force failure at the binary copy and observe the marker already there.
        // File mtimes cannot prove this order: a copy can preserve its source's
        // timestamp. The partial tree must remain owned and resumable.
        let fresh = d.join("fresh");
        let missing = d.join("missing-exe");
        let error = materialise(&fresh, &missing, "0.1.0", "t").expect_err("copy fails");
        assert!(error.contains("cannot copy"), "{}", error);
        assert!(read_marker(&fresh).expect("marker before copy").is_ok());
        assert!(!fresh.join(BIN).exists());
        for (rel, _) in crate::embedded::MANIFESTS {
            assert!(!fresh.join(rel).exists());
        }
        assert!(refuse_target(&fresh, &skills).is_none());
        materialise(&fresh, &exe, "0.1.0", "t").expect("resumed after copy failure");
        verify(&fresh.join(BIN), "0.1.0").expect("resumed binary runs");

        let _ = fs::remove_dir_all(&d);
    }

    /// What `uninstall` names as left behind. prune never touches these, so
    /// nothing else in the program would ever mention them.
    #[test]
    #[cfg(unix)]
    fn extra_files_names_what_the_marker_does_not_list() {
        let _serial = forking();
        let d = scratch("extra");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, fake_exe("0.1.0")).expect("write");
        sys::set_mode(&exe, 0o755).expect("chmod");
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
        assert!(all.contains(&BIN.to_string()), "{:?}", all);
        assert!(!all.iter().any(|f| f == MARKER), "{:?}", all);

        let _ = fs::remove_dir_all(&d);
    }

    /// Removal is the marker's list and nothing else, which is what makes it safe for
    /// `uninstall` to do it by DEFAULT now that the tree is where the plugin always
    /// lives. `remove_dir_all` on a path derived from a symlink is exactly what this
    /// does not do.
    #[test]
    #[cfg(unix)]
    fn remove_takes_only_what_the_marker_lists_and_names_the_rest() {
        let _serial = forking();
        let d = scratch("remove");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, fake_exe("0.1.0")).expect("write");
        sys::set_mode(&exe, 0o755).expect("chmod");
        materialise(&tree, &exe, "0.1.0", "t").expect("materialised");

        // A tree holding nothing but what we generated goes completely.
        let r = remove(&tree).expect("removed");
        assert_eq!(r.removed, 3);
        assert!(r.left.is_empty(), "{:?}", r.left);
        assert!(r.dir_gone);
        assert!(!tree.exists());

        // The same tree with somebody's own files in it: ours go, theirs stay, the
        // directory survives, and the report NAMES what it left.
        materialise(&tree, &exe, "0.1.0", "t").expect("materialised");
        fs::write(tree.join("NOTES.txt"), b"mine\n").expect("write");
        fs::create_dir_all(tree.join("hooks/keep")).expect("mkdir");
        fs::write(tree.join("hooks/keep/mine.md"), b"mine\n").expect("write");
        let r = remove(&tree).expect("removed");
        assert_eq!(r.removed, 3);
        assert_eq!(r.left, vec!["NOTES.txt", "hooks/keep/mine.md"]);
        assert!(!r.dir_gone);
        assert!(tree.join("NOTES.txt").is_file());
        assert!(tree.join("hooks/keep/mine.md").is_file());
        // The marker goes whatever happens - it is ours - and so does an emptied
        // directory, but never one still holding something.
        assert!(!marker_path(&tree).exists());
        assert!(!tree.join(".claude-plugin").exists());
        assert!(tree.join("hooks").is_dir());

        // An UNPARSEABLE marker proves ownership but names no files, so nothing is
        // removed and everything is named.
        let _ = fs::remove_dir_all(&tree);
        materialise(&tree, &exe, "0.1.0", "t").expect("materialised");
        fs::write(marker_path(&tree), b"not json").expect("write");
        let r = remove(&tree).expect("removed");
        assert_eq!(r.removed, 0);
        assert!(r.left.contains(&BIN.to_string()), "{:?}", r.left);
        assert!(tree.join(BIN).is_file());

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

    /// Whatever a marker spells, a path `in_tree` hands back is under the tree. On
    /// Windows the drive-relative spellings are the escape - pushing `C:evil`
    /// replaces the buffer - and `safe_relative` refuses them; on Unix they are
    /// ordinary file names inside the tree. The invariant is the same on both.
    #[test]
    fn a_marker_path_never_resolves_outside_the_tree() {
        let d = scratch("escape");
        let tree = d.join("tree");
        fs::create_dir_all(tree.join("hooks")).expect("mkdir");
        for rel in [&b"hooks/C:evil"[..], b"C:evil", b"hooks/C:/Windows/x", b"a/C:\\x", b"hooks/x"] {
            if let Ok(at) = in_tree(&tree, rel, false) {
                assert!(at.starts_with(&tree), "{:?} resolved to {}", rel, at.display());
            }
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// The exec probe, both ways, and the fact that `materialise` will not rename a
    /// copy that fails it into place. This is the check that turns a wrong
    /// architecture, a noexec mount and a lost exec bit into ONE named refusal
    /// instead of eleven hooks failing silently in every future session - and because
    /// what is verified is a 0755 temp copy, a shell script stands in for the binary
    /// and the probe can be driven without a second architecture to hand.
    #[test]
    #[cfg(unix)]
    fn verify_runs_the_copy_and_refuses_one_that_answers_wrong() {
        let _serial = forking();
        let d = scratch("verify");
        fs::create_dir_all(&d).expect("mkdir");

        let good = d.join("good");
        fs::write(&good, fake_exe("9.9.9")).expect("write");
        sys::set_mode(&good, 0o755).expect("chmod");
        let lines = materialise(&d.join("ok"), &good, "9.9.9", "t").expect("materialised");
        assert!(
            lines.iter().any(|l| l == "verify:   tabstatus 9.9.9 (some-triple)"),
            "{:?}",
            lines
        );
        assert_eq!(
            verify(&d.join("ok").join(BIN), "9.9.9").expect("runs"),
            "tabstatus 9.9.9 (some-triple)"
        );

        // Right shape, wrong version: a copy that is not the binary this run
        // embedded.
        assert!(verify(&d.join("ok").join(BIN), "0.1.0").expect_err("refused").contains("instead of"));

        // Runs, says something else entirely - and materialise REFUSES rather than
        // leaving it as the file the hooks invoke. The whole point of verifying the
        // temp copy is that this tree never gains a bin/tabstatus at all.
        let bad = d.join("bad");
        fs::write(&bad, "#!/bin/sh\nprintf 'not me\\n'\n").expect("write");
        sys::set_mode(&bad, 0o755).expect("chmod");
        let e = materialise(&d.join("no"), &bad, "9.9.9", "t").expect_err("refused");
        assert!(e.contains("not me"), "{}", e);
        assert!(!d.join("no").join(BIN).exists(), "the failed copy must not be renamed into place");
        // ...and no temp file is left behind either.
        assert!(extra_files(&d.join("no"), &[]).iter().all(|f| !f.contains("cctab-tmp")));

        // Will not exec at all - a noexec mount or a lost exec bit, which is what
        // this probe is actually for. It must NOT blame the architecture: the copy is
        // current_exe, so it is always this machine's architecture, and an aarch64 VM
        // never reaches a line of this program.
        let cant = d.join("cant");
        // An invalid executable format can fall back to a shell on some Unixes.
        // A shebang naming a missing interpreter must fail at process launch.
        let missing_interpreter = d.join("missing-interpreter");
        fs::write(&cant, format!("#!{}\n", missing_interpreter.display())).expect("write");
        let e = materialise(&d.join("nx"), &cant, "9.9.9", "t").expect_err("refused");
        assert!(e.contains("will not run"), "{}", e);
        assert!(e.contains("noexec"), "{}", e);
        assert!(!e.contains("If this machine is not x86_64"), "{}", e);

        let _ = fs::remove_dir_all(&d);
    }

    /// A stand-in for the binary: `materialise` execs `<copy> version` before it
    /// renames the copy into place, so every fixture that plays the binary has to
    /// answer the way the real one does.
    #[cfg(unix)]
    fn fake_exe(version: &str) -> String {
        format!("#!/bin/sh\nprintf 'tabstatus {} (some-triple)\\n'\n", version)
    }

    /// The whole materialise/refresh cycle, including the two things a stale tree
    /// depends on: an old file is pruned, and a changed file is reported REPLACED.
    #[test]
    #[cfg(unix)]
    fn materialise_writes_refreshes_prunes_and_reports_what_it_replaced() {
        let _serial = forking();
        let d = scratch("mat");
        let tree = d.join("tree");
        // materialise EXECS what it copies before renaming it into place, so the
        // stand-in has to answer `version` the way the real binary does.
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, fake_exe("0.1.0")).expect("write");
        sys::set_mode(&exe, 0o755).expect("chmod");

        let lines = materialise(&tree, &exe, "0.1.0", "t").expect("materialised");
        assert!(lines.iter().any(|l| l.contains("created")), "{:?}", lines);
        for (rel, text) in crate::embedded::MANIFESTS {
            assert_eq!(fs::read(tree.join(rel)).expect("written"), text.as_bytes());
        }
        assert_eq!(sys::mode(&fs::metadata(tree.join(BIN)).expect("copied")).map(|m| m & 0o777), Some(0o755));

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

    /// The text check and the ON-DISK check are different questions, and only one of
    /// them was being asked. `safe_relative` is happy with `bin/tabstatus` forever;
    /// whether `<tree>/bin` is a real directory decides whether that string names a
    /// file inside the tree or one in somebody else's.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_component_is_not_inside_the_tree() {
        let d = scratch("intree");
        let tree = d.join("tree");
        let victim = d.join("victim");
        fs::create_dir_all(tree.join("real")).expect("mkdir");
        fs::create_dir_all(&victim).expect("mkdir");
        fs::write(victim.join("precious"), b"theirs").expect("write");

        // The ordinary case, and the leaf need not exist.
        assert_eq!(in_tree(&tree, b"real/x", false).expect("in"), tree.join("real/x"));
        // An absent DIRECTORY component is fine for a removal and made for a write.
        assert!(in_tree(&tree, b"fresh/x", false).is_ok());
        assert!(in_tree(&tree, b"made/x", true).is_ok());
        assert!(tree.join("made").is_dir());

        // The hole: a directory component that is a link out of the tree.
        std::os::unix::fs::symlink(&victim, tree.join("bin")).expect("symlink");
        assert!(safe_relative(b"bin/precious"), "the STRING is fine, which is the point");
        let why = in_tree(&tree, b"bin/precious", false).expect_err("refused");
        assert!(why.contains("is not a directory"), "{}", why);
        assert!(why.contains("not provably inside the plugin tree"), "{}", why);
        // ...and `create` must not reopen it from the other side: create_dir_all would
        // accept the existing link as "already there".
        assert!(in_tree(&tree, b"bin/precious", true).is_err());
        // A file where a directory has to be is refused rather than left to errno.
        fs::write(tree.join("afile"), b"x").expect("write");
        assert!(in_tree(&tree, b"afile/under", true).is_err());
        // The textual refusals still hold, and now say which file they are about.
        for bad in [&b".."[..], b"/etc/hostname", b"a/../../x", b""] {
            assert!(in_tree(&tree, bad, false).is_err(), "{:?}", bad);
        }
        // And the tree itself being a link is the same question one level up.
        let linked = d.join("linked");
        std::os::unix::fs::symlink(&victim, &linked).expect("symlink");
        let why = in_tree(&linked, b"precious", false).expect_err("refused");
        assert!(why.contains("is a symlink"), "{}", why);

        let _ = fs::remove_dir_all(&d);
    }

    /// `remove` is the one path here that unlinks, so it gets the same proof from the
    /// outside: the victim's file survives and the marker-listed path is reported as
    /// left behind rather than counted as removed.
    #[test]
    #[cfg(unix)]
    fn remove_never_reaches_outside_the_tree() {
        let d = scratch("rmout");
        let tree = d.join("tree");
        let victim = d.join("victim");
        fs::create_dir_all(tree.join(".claude-plugin")).expect("mkdir");
        fs::create_dir_all(&victim).expect("mkdir");
        fs::write(victim.join("tabstatus"), b"theirs").expect("write");
        fs::write(tree.join(".claude-plugin/plugin.json"), b"{}").expect("write");
        std::os::unix::fs::symlink(&victim, tree.join("bin")).expect("symlink");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &generated_paths())).expect("write");

        let r = remove(&tree).expect("removed");
        assert_eq!(fs::read(victim.join("tabstatus")).expect("survived"), b"theirs");
        assert!(r.blocked.iter().any(|w| w.contains("bin")), "{:?}", r.blocked);
        // One file was genuinely ours and went; the link is named as left behind.
        assert_eq!(r.removed, 1);
        assert!(r.left.contains(&"bin (a link)".to_string()), "{:?}", r.left);
        assert!(!r.dir_gone);
        // The link itself is not removed either - it is not a file this tool wrote.
        assert!(fs::symlink_metadata(tree.join("bin")).is_ok());

        let _ = fs::remove_dir_all(&d);
    }

    /// A generated file that will not go - on Windows, the binary a hook is running,
    /// played here by a handle that shares nothing - is NAMED, and the marker stays, so
    /// the tree is still one `install` reuses rather than a stranger's directory it
    /// refuses. Once the file is free, removal finishes the job.
    #[test]
    #[cfg(windows)]
    fn a_file_that_will_not_go_keeps_the_marker_and_is_named() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = scratch("held");
        let tree = d.join("tree");
        let skills = d.join("cfg").join("skills");
        for rel in generated_paths() {
            let p = bin_path_like(&tree, rel);
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(&p, b"x").expect("write");
        }
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &generated_paths())).expect("write");
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(bin_path(&tree)).expect("open");

        let r = remove(&tree).expect("removed");
        assert_eq!(r.removed, 2);
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert_eq!(r.failed[0].0, bin_path(&tree), "{:?}", r.failed);
        assert!(!r.dir_gone);
        assert!(is_generated(&tree), "the marker stays while a file it lists does");
        assert!(refuse_target(&tree, &skills).is_none(), "so install still reuses it");

        drop(held);
        let r = remove(&tree).expect("removed");
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(r.dir_gone);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    fn bin_path_like(tree: &Path, rel: &str) -> PathBuf {
        rel.split('/').fold(tree.to_path_buf(), |p, c| p.join(c))
    }

    /// Every spelling NTFS takes to `<config>\skills` is refused as under it - the one
    /// that walked past this check wrote the tree AT the link path.
    #[test]
    #[cfg(windows)]
    fn a_tree_under_skills_is_refused_in_any_spelling() {
        let d = scratch("skills-case");
        let skills = d.join("cfg").join("skills");
        fs::create_dir_all(&skills).expect("mkdir");
        let upper = PathBuf::from(skills.to_string_lossy().to_uppercase());
        let mut verbatim = std::ffi::OsString::from(r"\\?\");
        verbatim.push(skills.join("foo"));
        for p in [upper.join("claude-tabstatus"), upper.join("foo"), PathBuf::from(&verbatim)] {
            let why = refuse_target(&p, &skills).expect("refused");
            assert!(why.contains("a directory to itself"), "{}", why);
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// A refusal is the one message here a hurried operator acts on. It carries no
    /// command at all any more (next test); this bounds even the advice to empty a
    /// directory by hand to one that looks like a stale tree of ours.
    #[test]
    fn rm_rf_is_only_ever_offered_for_something_that_looks_like_ours() {
        for no in ["/", "/usr", "/home/someone", "/a/b"] {
            assert!(!safe_to_suggest_removing(Path::new(no)), "{}", no);
        }
        assert!(safe_to_suggest_removing(Path::new("/home/someone/.local/share/claude-tabstatus")));
        assert!(safe_to_suggest_removing(Path::new("/srv/state/claude-tabstatus")));
        // Deep enough and named something else: no hint.
        assert!(!safe_to_suggest_removing(Path::new("/home/someone/code/work")));
    }

    /// No refusal hands over a command, whatever the directory is called: it carries no
    /// marker, so nothing here can tell its files from the operator's. One named like
    /// ours is told to look and empty it by hand; a sibling of the default tree - the
    /// slip that once printed `rm -rf ~/.local/share/claude` - gets nothing extra.
    #[test]
    fn a_refusal_never_hands_over_a_command_for_a_directory_without_a_marker() {
        let d = scratch("nocmd");
        let skills = d.join("cfg/skills");
        fs::create_dir_all(&skills).expect("mkdir");
        for (name, advice) in [("claude-tabstatus", true), ("claude", false), ("nvim", false)] {
            let p = d.join("share").join(name);
            fs::create_dir_all(&p).expect("mkdir");
            fs::write(p.join("theirs"), b"x").expect("write");
            let why = refuse_target(&p, &skills).expect("refused");
            assert!(!why.contains(&sys::remove_dir_command(&p)), "{}", why);
            assert!(!why.contains("rm -rf") && !why.contains("Remove-Item"), "{}", why);
            assert_eq!(why.contains("look at what it holds, and empty it yourself before re-running."), advice, "{}", why);
            assert!(why.ends_with("Nothing has been changed."), "{}", why);
            assert_eq!(fs::read(p.join("theirs")).expect("untouched"), b"x");
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// The check behind every delete-the-directory hint about a tree left in place:
    /// only the marker's plain files, and never a link - followed or not.
    #[test]
    fn a_survey_vouches_only_for_a_tree_holding_nothing_but_its_marker_list() {
        let d = scratch("survey");
        let tree = d.join("tree");
        let victim = d.join("victim");
        fs::create_dir_all(&victim).expect("mkdir");
        fs::write(victim.join("precious"), b"theirs").expect("write");
        let hooks = tree.join("hooks").join("hooks.json");
        for p in [bin_path(&tree), hooks.clone()] {
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(&p, b"x").expect("write");
        }
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN, "hooks/hooks.json"])).expect("write");

        let s = survey(&tree).expect("marked");
        assert!(s.only_ours());
        assert_eq!(s.ours, vec![bin_path(&tree), hooks.clone()]);

        // A link inside the tree is somebody's, and the walk does not go through it.
        sys::link_dir(&victim, &tree.join("linked")).expect("link");
        let s = survey(&tree).expect("marked");
        assert!(!s.only_ours());
        assert_eq!(s.foreign, vec!["linked (a link)"]);
        sys::remove_dir_link(&tree.join("linked")).expect("unlink");

        // ...and so is one at a path the marker lists: install writes only plain files.
        fs::remove_file(&hooks).expect("rm");
        sys::link_dir(&victim, &hooks).expect("link");
        let s = survey(&tree).expect("marked");
        assert!(!s.only_ours());
        assert_eq!(s.foreign, vec!["hooks/hooks.json (a link)"]);
        assert_eq!(s.ours, vec![bin_path(&tree)]);
        sys::remove_dir_link(&hooks).expect("unlink");

        // A file the marker lists under another spelling is not ours by that spelling:
        // on Windows `notes.txt` opens `NOTES.txt`, and the operator's file must not be
        // listed for deletion while being named as theirs.
        fs::write(tree.join("NOTES.txt"), b"mine").expect("write");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN, "hooks/hooks.json", "notes.txt"])).expect("write");
        let s = survey(&tree).expect("marked");
        assert!(!s.only_ours());
        assert_eq!(s.foreign, vec!["NOTES.txt"]);
        assert_eq!(s.ours, vec![bin_path(&tree)]);
        fs::remove_file(tree.join("NOTES.txt")).expect("rm");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN, "hooks/hooks.json"])).expect("write");

        // A tree that is itself a link, and one with no marker or an unreadable one,
        // are not surveyed at all - each with its own reason.
        let linked = d.join("linked-tree");
        sys::link_dir(&tree, &linked).expect("link");
        assert_eq!(survey(&linked).err().as_deref(), Some("It is a link, not a directory"));
        sys::remove_dir_link(&linked).expect("unlink");
        fs::write(marker_path(&tree), b"not json").expect("write");
        assert_eq!(survey(&tree).err(), Some(format!("Its {} cannot be read", MARKER)));
        fs::remove_file(marker_path(&tree)).expect("rm");
        assert_eq!(survey(&tree).err(), Some(format!("It carries no {} any more", MARKER)));
        sys::link_dir(&victim, &marker_path(&tree)).expect("link");
        assert_eq!(survey(&tree).err(), Some(format!("Its {} is a link, not a file", MARKER)));
        sys::remove_dir_link(&marker_path(&tree)).expect("unlink");

        assert_eq!(fs::read(victim.join("precious")).expect("untouched"), b"theirs");
        let _ = fs::remove_dir_all(&d);
    }

    /// This tool's own scratch files are ours to name for deletion and do not stand in
    /// the way of the directory command - every `install` sweeps them - but only as
    /// plain files: a link wearing that name is somebody's like any other link.
    #[test]
    fn a_survey_counts_this_tools_own_litter_as_ours_and_a_link_as_nobodys() {
        let d = scratch("litter");
        let tree = d.join("tree");
        fs::create_dir_all(bin_path(&tree).parent().expect("parent")).expect("mkdir");
        fs::write(bin_path(&tree), b"x").expect("write");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN])).expect("write");
        let tmp = tree.join("bin").join(".tabstatus.cctab-tmp.4242");
        fs::write(&tmp, b"half a copy").expect("write");
        let s = survey(&tree).expect("marked");
        assert!(s.only_ours(), "{:?}", s.foreign);
        assert_eq!(s.ours, vec![bin_path(&tree), tmp.clone()]);
        // `remove` does not take it, so it is still named as left there.
        assert_eq!(extra_files(&tree, &[BIN.as_bytes().to_vec()]), vec!["bin/.tabstatus.cctab-tmp.4242"]);
        if !sys::HAS_UNLINK_RUNNING {
            // The binary a running hook held, set aside by an install on Windows.
            let aside = tree.join("bin").join(".tabstatus.exe.cctab-old.4242");
            fs::write(&aside, b"old").expect("write");
            let s = survey(&tree).expect("marked");
            assert!(s.only_ours(), "{:?}", s.foreign);
            assert!(s.ours.contains(&aside), "{:?}", s.ours);
            fs::remove_file(&aside).expect("rm");
        }
        fs::remove_file(&tmp).expect("rm");
        let other = d.join("other");
        fs::create_dir_all(&other).expect("mkdir");
        sys::link_dir(&other, &tmp).expect("link");
        let s = survey(&tree).expect("marked");
        assert!(!s.only_ours());
        assert_eq!(s.foreign, vec!["bin/.tabstatus.cctab-tmp.4242 (a link)"]);
        assert_eq!(s.ours, vec![bin_path(&tree)]);
        sys::remove_dir_link(&tmp).expect("unlink");
        let _ = fs::remove_dir_all(&d);
    }

    /// A walk that stopped at its bound has not seen the tree, so neither the survey
    /// nor the removal may read its silence as "nothing else is in there". Empty
    /// directories, so that nothing it did see is foreign: the bound alone decides.
    #[test]
    fn a_walk_cut_short_vouches_for_nothing() {
        let d = scratch("toomany");
        let tree = d.join("tree");
        fs::create_dir_all(bin_path(&tree).parent().expect("parent")).expect("mkdir");
        fs::write(bin_path(&tree), b"x").expect("write");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN])).expect("write");
        let bulk = tree.join("bulk");
        fs::create_dir_all(&bulk).expect("mkdir");
        for i in 0..4100 {
            fs::create_dir(bulk.join(i.to_string())).expect("mkdir");
        }
        let s = survey(&tree).expect("marked");
        assert_eq!(s.unread, Some(Unread::TooMany));
        assert!(s.foreign.is_empty() && s.blocked.is_empty(), "{:?} {:?}", s.foreign, s.blocked);
        assert!(!s.only_ours());
        // ...and what is ours is still listed, from the disk, wherever the walk stopped.
        assert_eq!(s.ours, vec![bin_path(&tree)]);
        let r = remove(&tree).expect("removed");
        assert_eq!(r.unread, Some(Unread::TooMany));
        assert!(r.left.is_empty() && !r.dir_gone, "{:?}", r.left);
        let _ = fs::remove_dir_all(&d);
    }

    /// A directory the walk cannot read is one it has not seen. Not on Windows, whose
    /// ACLs this test does not edit, and not as root, who reads it anyway.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_directory_vouches_for_nothing() {
        let d = scratch("unreadable");
        let tree = d.join("tree");
        fs::create_dir_all(bin_path(&tree).parent().expect("parent")).expect("mkdir");
        fs::write(bin_path(&tree), b"x").expect("write");
        fs::write(marker_path(&tree), marker_text("0.1.0", "t", &[BIN])).expect("write");
        let shut = tree.join("shut");
        fs::create_dir_all(&shut).expect("mkdir");
        fs::write(shut.join("theirs"), b"x").expect("write");
        sys::set_mode(&shut, 0o000).expect("chmod");
        if fs::read_dir(&shut).is_ok() {
            sys::set_mode(&shut, 0o755).expect("chmod");
            let _ = fs::remove_dir_all(&d);
            return;
        }
        let s = survey(&tree).expect("marked");
        assert_eq!(s.unread, Some(Unread::Unreadable));
        assert!(s.foreign.is_empty(), "{:?}", s.foreign);
        assert!(!s.only_ours());
        let r = remove(&tree).expect("removed");
        assert_eq!(r.unread, Some(Unread::Unreadable));
        assert!(r.left.is_empty() && !r.dir_gone, "{:?}", r.left);
        sys::set_mode(&shut, 0o755).expect("chmod");
        assert_eq!(fs::read(shut.join("theirs")).expect("untouched"), b"x");
        let _ = fs::remove_dir_all(&d);
    }

    /// An emptied directory is invisible to everything downstream - no later marker
    /// lists it, so `remove` never takes it and the tree cannot come down.
    #[test]
    #[cfg(unix)]
    fn pruning_takes_the_directory_it_emptied() {
        let _serial = forking();
        let d = scratch("prunedir");
        let tree = d.join("tree");
        let exe = d.join("fake-exe");
        fs::create_dir_all(&d).expect("mkdir");
        fs::write(&exe, fake_exe("0.1.0")).expect("write");
        sys::set_mode(&exe, 0o755).expect("chmod");
        materialise(&tree, &exe, "0.1.0", "t").expect("materialised");

        fs::create_dir_all(tree.join("old/deeper")).expect("mkdir");
        fs::write(tree.join("old/deeper/legacy.json"), b"{}").expect("write");
        let mut files = generated_paths();
        files.push("old/deeper/legacy.json");
        fs::write(marker_path(&tree), marker_text("0.0.1", "t", &files)).expect("write");

        let lines = materialise(&tree, &exe, "0.1.0", "t").expect("refreshed");
        assert!(lines.iter().any(|l| l.contains("pruned:   old/deeper/legacy.json")), "{:?}", lines);
        assert!(!tree.join("old").exists(), "the emptied directories went too");
        // ...and a directory that is NOT empty is left with its contents.
        fs::create_dir_all(tree.join("theirs")).expect("mkdir");
        fs::write(tree.join("theirs/notes"), b"mine").expect("write");
        let r = remove(&tree).expect("removed");
        assert!(!r.dir_gone);
        assert_eq!(fs::read(tree.join("theirs/notes")).expect("kept"), b"mine");

        let _ = fs::remove_dir_all(&d);
    }
}
