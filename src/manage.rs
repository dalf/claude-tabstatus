//! `install`, `uninstall`, `doctor`, `version` - the management half of the
//! binary.
//!
//! It is in the SAME binary as the hook, and that is a measured choice rather
//! than a convenience: merging the installer into the hook cost -9us on the hot
//! edge and 90KB of file size. Code that never runs is never paged in, so a
//! second executable would have bought nothing and cost a second thing to keep
//! in step. One file to copy, one path in hooks.json, one `--version`.
//!
//! What it changes is exactly what the shell installer changed:
//!
//!   1. one key   <config>/settings.json -> env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"
//!   2. a symlink <config>/skills/claude-tabstatus -> this repo
//!   3. a record  <config>/claude-tabstatus.state  -> what was there before
//!
//! settings.json is written FIRST and the symlink LAST, so a failure while
//! editing settings cannot leave the plugin loaded with Claude Code's built-in
//! title still repainting over it - which looks broken rather than uninstalled.
//! uninstall is the MIRROR of that, settings first and the link last, for the
//! sharper version of the same reason: the half-state where the env key is still
//! set and the plugin is gone paints NO tab title at all. Both halves preflight
//! every refusal, so the window between the two writes contains nothing that can
//! decide to stop.
//!
//! CLAUDE_CONFIG_DIR overrides the config directory, which is how the tests
//! point all of this at a throwaway tree instead of a real one.

use crate::settings::{self, Outcome};
use crate::{json, location, render, sh};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const KEY: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";
const PLUGIN: &str = "claude-tabstatus";
const STATE_VERSION: i32 = 2;

/// The management names. Checked against argv before anything else, and
/// deliberately disjoint from the edge names - `working`, `waiting`, `idle`,
/// `notify`, `session-start`, `session-end` - so no hook can reach this half and
/// no typo of a subcommand can reach the paint path with a real edge's name.
pub fn is_subcommand(a: &[u8]) -> bool {
    matches!(
        a,
        b"install" | b"uninstall" | b"doctor" | b"version" | b"--version" | b"-V"
            | b"help" | b"--help" | b"-h"
    )
}

pub fn dispatch(a: &[u8], args: &[Vec<u8>]) -> i32 {
    match a {
        b"install" => {
            let mut force = false;
            for arg in args {
                match &arg[..] {
                    b"--force" => force = true,
                    other => {
                        fail(&format!(
                            "unknown option {}",
                            String::from_utf8_lossy(other)
                        ));
                        return 1;
                    }
                }
            }
            run(|c| install(c, force))
        }
        b"uninstall" => {
            let mut force = false;
            let mut restore = false;
            for arg in args {
                match &arg[..] {
                    b"--force" => force = true,
                    b"--restore-backup" => restore = true,
                    other => {
                        fail(&format!(
                            "unknown option {}",
                            String::from_utf8_lossy(other)
                        ));
                        return 1;
                    }
                }
            }
            run(|c| uninstall(c, force, restore))
        }
        b"doctor" => run(doctor),
        b"version" | b"--version" | b"-V" => {
            version();
            0
        }
        _ => {
            usage();
            0
        }
    }
}

fn run<F: FnOnce(&Ctx) -> Result<(), String>>(f: F) -> i32 {
    let ctx = match Ctx::new() {
        Ok(c) => c,
        Err(e) => {
            fail(&e);
            return 1;
        }
    };
    match f(&ctx) {
        Ok(()) => 0,
        Err(e) => {
            fail(&e);
            1
        }
    }
}

fn say(s: &str) {
    let out = std::io::stdout();
    let mut l = out.lock();
    let _ = l.write_all(s.as_bytes());
    let _ = l.write_all(b"\n");
}

fn fail(s: &str) {
    let e = std::io::stderr();
    let mut l = e.lock();
    let _ = l.write_all(b"error: ");
    let _ = l.write_all(s.as_bytes());
    let _ = l.write_all(b"\n");
}

fn usage() {
    say(concat!(
        "tabstatus - the Claude Code session state in the terminal tab title\n",
        "\n",
        "  tabstatus install            link the plugin and disable the built-in title\n",
        "  tabstatus uninstall          undo exactly that\n",
        "      --force                  remove the env key even with no state record\n",
        "      --restore-backup         roll settings.json back to the pre-install copy\n",
        "  tabstatus doctor             report what is installed and what would paint\n",
        "  tabstatus version\n",
        "\n",
        "Hook edges, invoked from hooks/hooks.json rather than by hand:\n",
        "  working | waiting | idle | notify | session-start | session-end\n",
        "\n",
        "  CCTAB_DRY_RUN=1 tabstatus working    print the title, emit nothing",
    ));
}

fn version() {
    say(&format!(
        "tabstatus {} ({})",
        env!("CARGO_PKG_VERSION"),
        target_triple()
    ));
}

/// std has no target-triple constant and this crate has no build script, so the
/// triple is composed from the cfg the build actually used. Reported by
/// `version` and `doctor` because the repo ships prebuilt binaries under bin/
/// and "which one of those am I running" is then a real question.
fn target_triple() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "x86_64", target_os = "linux", target_env = "musl")) {
        "x86_64-unknown-linux-musl"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_arch = "aarch64", target_os = "linux", target_env = "musl")) {
        "aarch64-unknown-linux-musl"
    } else if cfg!(all(target_arch = "x86_64", target_os = "macos")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_arch = "aarch64", target_os = "macos")) {
        "aarch64-apple-darwin"
    } else {
        "unknown-target"
    }
}

// --- context ----------------------------------------------------------------

struct Ctx {
    repo: PathBuf,
    config: PathBuf,
    skills: PathBuf,
    link: PathBuf,
    /// The file actually written: a settings.json that is a symlink resolves to
    /// its target here, so the temp-file rename lands on the managed file
    /// instead of replacing the link with a regular file.
    settings: PathBuf,
    settings_link: Option<PathBuf>,
    /// The target of a settings.json symlink that does NOT resolve, as readlink
    /// gives it. `install` and `uninstall` refuse on it - writing through a
    /// dangling link would replace the link with a regular file - but `doctor`
    /// reports it and carries on, because the one command whose job is to explain
    /// a broken config has to be able to run on the config that is broken.
    settings_unresolved: Option<PathBuf>,
    state: PathBuf,
    backup: PathBuf,
    safety: PathBuf,
}

impl Ctx {
    fn new() -> Result<Ctx, String> {
        let repo = repo_root()?;
        let config = match sh::env_raw("CLAUDE_CONFIG_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(OsString::from_vec(v)),
            _ => {
                let home = sh::env_str("HOME");
                if home.is_empty() {
                    return Err("neither CLAUDE_CONFIG_DIR nor HOME is set, so there \
                                is no config directory to work on"
                        .to_string());
                }
                let mut p = PathBuf::from(OsString::from_vec(home));
                p.push(".claude");
                p
            }
        };
        let skills = config.join("skills");
        let link = skills.join(PLUGIN);
        let mut settings = config.join("settings.json");
        let state = config.join(format!("{}.state", PLUGIN));
        let mut settings_link = None;
        let mut settings_unresolved = None;
        if let Ok(md) = fs::symlink_metadata(&settings) {
            if md.file_type().is_symlink() {
                match fs::canonicalize(&settings) {
                    Ok(real) => {
                        settings_link = Some(settings.clone());
                        settings = real;
                    }
                    Err(_) => {
                        settings_unresolved =
                            Some(fs::read_link(&settings).unwrap_or_else(|_| PathBuf::from("?")));
                    }
                }
            }
        }
        let backup = with_suffix(&settings, ".cctab-preinstall");
        let safety = with_suffix(&settings, ".cctab-preuninstall");
        Ok(Ctx {
            repo,
            config,
            skills,
            link,
            settings,
            settings_link,
            settings_unresolved,
            state,
            backup,
            safety,
        })
    }

    /// The binary hooks.json invokes. Kept separate from the running executable:
    /// `cargo run` and a copy in a scratchpad are both legitimate ways to run
    /// `install`, and what must exist afterwards is the committed one.
    fn hook_binary(&self) -> PathBuf {
        self.repo.join("bin").join("tabstatus")
    }
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Walk up from the running executable looking for the plugin manifest. That
/// finds the repo whether the binary is `bin/tabstatus`, a `bin/tabstatus-<triple>`
/// it points at, or `target/release/tabstatus` straight out of a build - and
/// /proc/self/exe is already symlink-resolved, so installing through the
/// installed link records the real clone rather than a link to a link.
fn repo_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find my own path ({}), so I cannot find the repo", e))?;
    let mut dir = exe.parent().map(|p| p.to_path_buf());
    for _ in 0..5 {
        let d = match dir {
            Some(d) => d,
            None => break,
        };
        if d.join(".claude-plugin/plugin.json").is_file() && d.join("hooks/hooks.json").is_file() {
            return Ok(d);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    Err(format!(
        "{} is not inside a claude-tabstatus checkout (no .claude-plugin/plugin.json \
         and hooks/hooks.json above it)",
        exe.display()
    ))
}

// --- atomic writes ----------------------------------------------------------

fn mode_of(p: &Path) -> Option<u32> {
    fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o7777)
}

/// Write through a temp file in the same directory and rename, with an explicit
/// mode: the settings.json we replace holds the user's `env` block, so a mode of
/// 0600 has to stay 0600. The shell installer widened it to 0644 through a
/// redirection, which is the bug this signature exists to make impossible.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
    let mut tmp_name = b".".to_vec();
    tmp_name.extend_from_slice(&name);
    tmp_name.extend_from_slice(format!(".cctab-tmp.{}", std::process::id()).as_bytes());
    let tmp = dir.join(OsString::from_vec(tmp_name));
    let _ = fs::remove_file(&tmp);
    let res = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // create_new honours `mode` only through the open(2) mode argument,
        // which umask narrows. Set it explicitly so a umask of 022 cannot
        // widen - or narrow - what we asked for.
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        fs::rename(&tmp, path)
    })();
    match res {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("could not write {}: {}", path.display(), e))
        }
    }
}

fn read_file(p: &Path) -> Result<Vec<u8>, String> {
    fs::read(p).map_err(|e| format!("cannot read {}: {}", p.display(), e))
}

fn writable(p: &Path) -> bool {
    // Asked the only way that is not a lie: open it.
    fs::OpenOptions::new().write(true).open(p).is_ok()
}

fn dir_writable(p: &Path) -> bool {
    let probe = p.join(format!(".cctab-wtest.{}", std::process::id()));
    match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// --- the state record -------------------------------------------------------

struct State {
    env_had: bool,
    env_raw: Option<Vec<u8>>,
    /// Whether the `env` OBJECT was already in the file. Without this, uninstall
    /// cannot tell an `env` the installer created from an empty one the user had,
    /// and removes theirs.
    env_object_had: bool,
    link_had: bool,
    link_target: Option<Vec<u8>>,
}

fn state_text(c: &Ctx, s: &State) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"state_version\": {},\n", STATE_VERSION));
    out.push_str("  \"written_by\": \"tabstatus install\",\n");
    out.push_str(&format!("  \"repo\": {},\n", json::quote(c.repo.as_os_str().as_bytes())));
    out.push_str(&format!(
        "  \"settings_path\": {},\n",
        json::quote(c.settings.as_os_str().as_bytes())
    ));
    out.push_str(&format!("  \"env_key\": {},\n", json::quote(KEY.as_bytes())));
    out.push_str(&format!(
        "  \"env_object_before\": {{\"had\": {}}},\n",
        s.env_object_had
    ));
    out.push_str("  \"env_key_before\": {");
    out.push_str(&format!("\"had\": {}, ", s.env_had));
    match &s.env_raw {
        // The value's original TEXT, carried as a JSON string so that restoring
        // it is a byte-for-byte splice rather than a re-encoding.
        Some(raw) => out.push_str(&format!("\"raw\": {}", json::quote(raw))),
        None => out.push_str("\"raw\": null"),
    }
    out.push_str("},\n");
    out.push_str("  \"symlink_before\": {");
    out.push_str(&format!("\"had\": {}, ", s.link_had));
    match &s.link_target {
        Some(t) => out.push_str(&format!("\"target\": {}", json::quote(t))),
        None => out.push_str("\"target\": null"),
    }
    out.push_str("}\n}\n");
    out.into_bytes()
}

fn read_state(c: &Ctx) -> Result<Option<State>, String> {
    if !c.state.exists() {
        return Ok(None);
    }
    let raw = read_file(&c.state)?;
    let v = json::parse(&raw).map_err(|e| {
        format!(
            "{} is not valid JSON ({}). Delete it and re-run with --force to \
             uninstall anyway.",
            c.state.display(),
            e
        )
    })?;
    let root = v
        .as_obj()
        .ok_or_else(|| format!("{} is not a JSON object", c.state.display()))?;
    let (mut env_had, mut env_raw) = (false, None);
    if let Some(m) = root.get("env_key_before") {
        if let Some(o) = m.val.as_obj() {
            env_had = o.get("had").and_then(|x| x.val.as_bool()).unwrap_or(false);
            env_raw = o.get("raw").and_then(|x| x.val.as_str()).map(|s| s.to_vec());
            if env_raw.is_none() {
                // state_version 1, written by the shell installer, recorded the
                // value as a JSON VALUE under a different name. Its source text
                // is exactly what we want to splice back, and the parser has the
                // span, so a state file from before this binary existed still
                // restores a value the user had set themselves.
                if let Some(v) = o.get("value") {
                    if !matches!(v.val, json::J::Null) {
                        env_raw = Some(raw[v.val_start..v.end].to_vec());
                    }
                }
            }
        }
    }
    // A state file written before this field existed (state_version 1, or a
    // hand-edited one) says nothing about it. Default false, which is what the
    // shell installer did unconditionally: remove an env that ends up empty.
    let mut env_object_had = false;
    if let Some(m) = root.get("env_object_before") {
        if let Some(o) = m.val.as_obj() {
            env_object_had = o.get("had").and_then(|x| x.val.as_bool()).unwrap_or(false);
        }
    }
    let (mut link_had, mut link_target) = (false, None);
    if let Some(m) = root.get("symlink_before") {
        if let Some(o) = m.val.as_obj() {
            link_had = o.get("had").and_then(|x| x.val.as_bool()).unwrap_or(false);
            link_target = o.get("target").and_then(|x| x.val.as_str()).map(|s| s.to_vec());
        }
    }
    Ok(Some(State { env_had, env_raw, env_object_had, link_had, link_target }))
}

// --- the skills symlink -----------------------------------------------------

enum LinkState {
    Absent,
    /// A symlink, its target, and whether that target resolves.
    Link(PathBuf, bool),
    Dir,
    Other,
}

fn link_state(p: &Path) -> LinkState {
    match fs::symlink_metadata(p) {
        Err(_) => LinkState::Absent,
        Ok(md) => {
            if md.file_type().is_symlink() {
                let t = fs::read_link(p).unwrap_or_default();
                LinkState::Link(t, fs::metadata(p).is_ok())
            } else if md.is_dir() {
                LinkState::Dir
            } else {
                LinkState::Other
            }
        }
    }
}

/// The refusal both writers share for a settings.json symlink that does not
/// resolve. Writing through it would replace the LINK with a regular file.
fn refuse_unresolved(c: &Ctx) -> Option<String> {
    c.settings_unresolved.as_ref().map(|t| {
        format!(
            "{} is a symlink to {}, which could not be resolved. Refusing to \
             touch it; nothing has been changed.",
            c.settings.display(),
            t.display()
        )
    })
}

/// Unlink this tool's own scratch files left behind by a run that was killed.
///
/// `dir_writable`'s probe and `write_atomic`'s temp file are both named with the
/// pid that created them, so nothing ever collides with a stale one and nothing
/// ever cleaned one up: 400 SIGKILLed runs left a permanent `.cctab-wtest.<pid>`
/// and a permanent `.settings.json.cctab-tmp.<pid>` in the config directory. Both
/// are per-run scratch, so a name whose pid is no longer in /proc is litter by
/// definition. A LIVE pid is left alone, because a concurrent install's temp file
/// is the one thing here that must not be removed under it.
fn sweep_litter(dir: &Path) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let raw = e.file_name();
        let name = match raw.to_str() {
            Some(n) => n,
            None => continue,
        };
        // `.cctab-wtest.<pid>` and `.<file>.cctab-tmp.<pid>` - our two scratch
        // shapes, and nothing else in either directory ends in `.<digits>` after
        // one of those markers.
        let pid = match name
            .strip_prefix(".cctab-wtest.")
            .or_else(|| name.rsplit_once(".cctab-tmp.").map(|(_, p)| p))
        {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p,
            _ => continue,
        };
        if !Path::new(&format!("/proc/{}", pid)).exists() {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Every directory this tool writes scratch files into.
fn sweep_all(c: &Ctx) {
    sweep_litter(&c.config);
    let dir = c.settings.parent().unwrap_or(Path::new("."));
    if dir != c.config {
        sweep_litter(dir);
    }
}

fn refuse_link(p: &Path, what: &str) -> String {
    format!(
        "{} exists and is {}. This installer did not create it, so it is not \
         ours to replace. Move or remove it yourself, then re-run. Nothing has \
         been changed.",
        p.display(),
        what
    )
}

// --- install ----------------------------------------------------------------

fn install(c: &Ctx, force: bool) -> Result<(), String> {
    // ---- preflight: every refusal happens before anything is written -------
    if let Some(e) = refuse_unresolved(c) {
        return Err(e);
    }
    for f in [".claude-plugin/plugin.json", "hooks/hooks.json"] {
        if !c.repo.join(f).is_file() {
            return Err(format!("{} does not look like the {} repo (missing {})", c.repo.display(), PLUGIN, f));
        }
    }
    // The binary hooks.json invokes. Without it the install is strictly WORSE
    // than no install: the env key switches Claude Code's own title painting off
    // and all ten hooks then resolve to a command that exits 127, so nothing
    // paints the tab at all. Refused rather than warned about, because the
    // warning exited 0 and was invisible to any wrapper script.
    let hook = c.hook_binary();
    if !hook.is_file() && !force {
        return Err(format!(
            "{} is missing, so none of the ten hooks could run - and installing \
             anyway would set env.{}, which switches Claude Code's OWN title \
             painting off. That leaves a tab nothing paints at all, so this is \
             refused rather than warned about. Nothing has been changed.\n\
             \n\
             Build it:   sh scripts/build.sh\n\
             Or install anyway, if you are about to build it: tabstatus install --force",
            hook.display(),
            KEY
        ));
    }
    match link_state(&c.link) {
        LinkState::Dir => return Err(refuse_link(&c.link, "a real directory, not a symlink")),
        LinkState::Other => return Err(refuse_link(&c.link, "not a symlink")),
        _ => {}
    }
    // `<config>/skills` itself, which step 3 has to create or write into. Checked
    // HERE because step 3 runs after settings.json has already been edited: a
    // refusal there would leave the env key set with no plugin to honour it,
    // which is the one half-state this ordering exists to prevent.
    if c.skills.exists() {
        if !c.skills.is_dir() {
            return Err(refuse_link(&c.skills, "not a directory"));
        }
        if !dir_writable(&c.skills) {
            return Err(format!(
                "{} is not writable, and the plugin symlink goes in it. Nothing \
                 has been changed.",
                c.skills.display()
            ));
        }
    }
    if c.config.exists() {
        if !c.config.is_dir() {
            return Err(format!("{} exists and is not a directory", c.config.display()));
        }
        if !dir_writable(&c.config) {
            return Err(format!("{} is not writable. Nothing has been changed.", c.config.display()));
        }
    }
    let existing = if c.settings.exists() {
        let doc = read_file(&c.settings)?;
        // A zero-byte or whitespace-only file is a real state - a truncated write,
        // or an editor that creates the file before it saves - and Claude Code
        // reads it as no settings at all. Treated as the `None` branch, i.e.
        // written fresh, rather than dead-ended on with a byte-offset parser
        // message. Its mode is still preserved, below.
        if doc.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
            if !writable(&c.settings) {
                return Err(format!(
                    "{} is read-only. Refusing to overwrite it; chmod it yourself if \
                     that was not deliberate.",
                    c.settings.display()
                ));
            }
            None
        } else {
        // Parse before anything is touched, so a document we cannot round-trip
        // is refused rather than half-edited.
        settings::env_raw_text(&doc, KEY).map_err(|e| {
            format!(
                "{} is not the expected shape ({}). Refusing to touch it, and \
                 nothing else has been changed.",
                c.settings.display(),
                e
            )
        })?;
        if !writable(&c.settings) {
            return Err(format!(
                "{} is read-only. Refusing to overwrite it; chmod it yourself if \
                 that was not deliberate.",
                c.settings.display()
            ));
        }
        let dir = c.settings.parent().unwrap_or(Path::new("."));
        if !dir_writable(dir) {
            return Err(format!(
                "{} is not writable, and settings.json is replaced through a \
                 temp file in its own directory. Nothing was changed.",
                dir.display()
            ));
        }
        Some(doc)
        }
    } else {
        None
    };

    say(&format!("repo:     {}", c.repo.display()));
    say(&format!("config:   {}", c.config.display()));
    if let Some(l) = &c.settings_link {
        say(&format!("settings: following the symlink {} -> {}", l.display(), c.settings.display()));
    }
    if hook.is_file() {
        say(&format!("binary:   {}", hook.display()));
    } else {
        // Only reachable with --force, which the preflight above demands.
        say(&format!(
            "binary:   WARNING - {} is missing, so NOTHING will paint until you \
             build it.\n          Installing anyway because --force was given.\n\
                       Build it: sh scripts/build.sh",
            hook.display()
        ));
    }
    say("");
    // Clear this tool's own scratch files left by a killed run, in the two
    // directories it is about to write.
    sweep_all(c);

    fs::create_dir_all(&c.config)
        .map_err(|e| format!("cannot create {}: {}", c.config.display(), e))?;

    // ---- 1. record what was here, once ------------------------------------
    if c.state.exists() {
        say(&format!("state:    {} exists - keeping the original record", c.state.display()));
    } else {
        let (link_had, link_target) = match link_state(&c.link) {
            LinkState::Link(t, _) => (true, Some(t.as_os_str().as_bytes().to_vec())),
            _ => (false, None),
        };
        let env_raw = match &existing {
            Some(doc) => settings::env_raw_text(doc, KEY)?,
            None => None,
        };
        let env_object_had = match &existing {
            Some(doc) => settings::has_env_object(doc)?,
            None => false,
        };
        let s = State {
            env_had: env_raw.is_some(),
            env_raw,
            env_object_had,
            link_had,
            link_target,
        };
        write_atomic(&c.state, &state_text(c, &s), 0o600)?;
        say(&format!("state:    recorded the prior state in {}", c.state.display()));
    }

    // ---- 2. the settings key ----------------------------------------------
    match existing {
        None => {
            // 0600 from birth: this file is where people keep API keys. A blank
            // file that already existed keeps the mode it had.
            let mode = mode_of(&c.settings).unwrap_or(0o600);
            let text = format!("{{\n  \"env\": {{\n    {}: \"1\"\n  }}\n}}\n", json::quote(KEY.as_bytes()));
            write_atomic(&c.settings, text.as_bytes(), mode)?;
            say(&format!("settings: created {} (mode {:04o})", c.settings.display(), mode));
            say(&format!("          env.{} = \"1\"", KEY));
        }
        Some(doc) => match settings::set_env_key(&doc, KEY, "1")? {
            Outcome::Unchanged => {
                say(&format!("settings: env.{} is already \"1\" - unchanged", KEY));
                say("          (no backup was written, because nothing was changed)");
            }
            Outcome::Changed { text, before } => {
                let mode = mode_of(&c.settings).unwrap_or(0o600);
                fs::copy(&c.settings, &c.backup)
                    .map_err(|e| format!("cannot write the backup {}: {}", c.backup.display(), e))?;
                say(&format!("settings: backed up to {}", c.backup.display()));
                // Re-read and compare: another live Claude Code session writing
                // this file between the read above and the rename here would
                // otherwise have its write silently dropped.
                let now = read_file(&c.settings)?;
                if now != doc {
                    return Err(format!(
                        "{} changed while this installer was running (another \
                         Claude Code session writing it, most likely). Nothing \
                         was changed; the backup is at {}.",
                        c.settings.display(),
                        c.backup.display()
                    ));
                }
                write_atomic(&c.settings, &text, mode)?;
                say(&format!("settings: set env.{} = \"1\" (mode {:04o} kept)", KEY, mode));
                if before.had {
                    say(&format!(
                        "          the previous value {} is recorded in the state file",
                        String::from_utf8_lossy(&before.raw.unwrap_or_default())
                    ));
                }
                say("          every other byte of the file is untouched");
            }
        },
    }

    // ---- 3. the symlink ---------------------------------------------------
    // Everything below is preflighted above, so a failure here is a surprise
    // (a race, a full disk). Say what state it leaves, because settings.json has
    // already been written by now.
    fs::create_dir_all(&c.skills).map_err(|e| late_failure(c, &format!("cannot create {}: {}", c.skills.display(), e)))?;
    match link_state(&c.link) {
        LinkState::Absent => {
            symlink(&c.repo, &c.link).map_err(|e| late_failure(c, &e))?;
            say("symlink:  created");
            say(&format!("          {} -> {}", c.link.display(), c.repo.display()));
        }
        LinkState::Link(t, resolves) if t == c.repo => {
            if resolves {
                say("symlink:  already correct");
                say(&format!("          {} -> {}", c.link.display(), c.repo.display()));
            } else {
                say("symlink:  points here but does not resolve - recreated");
                say(&format!("          {} -> {}", c.link.display(), c.repo.display()));
            }
        }
        LinkState::Link(t, resolves) => {
            fs::remove_file(&c.link)
                .map_err(|e| late_failure(c, &format!("cannot replace {}: {}", c.link.display(), e)))?;
            symlink(&c.repo, &c.link).map_err(|e| late_failure(c, &e))?;
            say(if resolves {
                "symlink:  WARNING - repointed a symlink this installer did not create"
            } else {
                "symlink:  WARNING - replaced a BROKEN symlink this installer did not create"
            });
            say(&format!("          {}", c.link.display()));
            say(&format!("          was  {}", t.display()));
            say(&format!("          now  {}", c.repo.display()));
            say("          uninstall puts the old target back.");
        }
        LinkState::Dir => return Err(refuse_link(&c.link, "a real directory, not a symlink")),
        LinkState::Other => return Err(refuse_link(&c.link, "not a symlink")),
    }

    say("");
    say("Done. Nothing else on this machine was modified.");
    say("Start a NEW Claude Code session for the plugin and the env key to take effect.");
    say("To undo: tabstatus uninstall");
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("cannot create the symlink {}: {}", link.display(), e))
}

/// A failure AFTER settings.json has been written. Everything reachable here is
/// preflighted, so this is a race or a full disk - but the user still has to be
/// told that the env key is set and how to put it back, or the tab paints nothing
/// and the reason is invisible.
fn late_failure(c: &Ctx, what: &str) -> String {
    format!(
        "{}\n\
         \n\
         settings.json was ALREADY changed (env.{} = \"1\"), so Claude Code's own \
         title painting is off while the plugin is not linked - a tab nothing \
         paints. Run `tabstatus uninstall` to put it back, or fix the path above \
         and re-run `tabstatus install`.\n\
         The state record at {} is what uninstall reads.",
        what,
        KEY,
        c.state.display()
    )
}

// --- uninstall --------------------------------------------------------------

fn uninstall(c: &Ctx, force: bool, restore_backup: bool) -> Result<(), String> {
    // ---- preflight: every refusal happens before anything is removed -------
    if let Some(e) = refuse_unresolved(c) {
        return Err(e);
    }
    match link_state(&c.link) {
        LinkState::Dir => return Err(refuse_link(&c.link, "a real directory, not a symlink")),
        LinkState::Other => return Err(refuse_link(&c.link, "not a symlink")),
        _ => {}
    }
    let mut blank_settings = false;
    if c.settings.exists() {
        let doc = read_file(&c.settings)?;
        // Blank counts as "no settings", as it does in install.
        blank_settings = doc.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
        if !blank_settings {
            settings::env_raw_text(&doc, KEY).map_err(|e| {
                format!(
                    "{} is not the expected shape ({}). Refusing to touch it, and \
                     nothing else has been changed.",
                    c.settings.display(),
                    e
                )
            })?;
        }
        if !writable(&c.settings) {
            return Err(format!("{} is read-only. Refusing to overwrite it.", c.settings.display()));
        }
    }
    let state = read_state(c)?;

    // The "not ours to delete" decision is a PURE function of data already in
    // hand, so it belongs here rather than in step 2. It used to run after the
    // symlink had been removed, which left the one combination that paints no tab
    // title at all: the env key still off, and the plugin that painted the
    // replacement gone.
    if !restore_backup && !blank_settings && c.settings.exists() && state.is_none() && !force {
        let doc = read_file(&c.settings)?;
        if settings::env_raw_text(&doc, KEY)?.is_some() {
            fail(&format!("env.{} IS set, but there is no record that we set it.", KEY));
            fail("Not removing a key this uninstaller cannot prove it created, and");
            fail("not removing the plugin symlink either - the two together are what");
            fail("paints the tab, and removing only one leaves a tab nothing paints.");
            fail("Re-run with --force to remove both anyway: tabstatus uninstall --force");
            return Ok(());
        }
    }

    say(&format!("config:   {}", c.config.display()));
    if let Some(l) = &c.settings_link {
        say(&format!("settings: following the symlink {} -> {}", l.display(), c.settings.display()));
    }
    match &state {
        Some(_) => say(&format!("state:    {}", c.state.display())),
        None => say(&format!("state:    no record at {}", c.state.display())),
    }
    say("");
    sweep_all(c);

    // ---- 1. settings.json -------------------------------------------------
    if restore_backup {
        if !c.backup.exists() {
            return Err(format!(
                "no pre-install backup at {}. install only writes one when it \
                 actually changes settings.json.",
                c.backup.display()
            ));
        }
        let raw = read_file(&c.backup)?;
        json::parse(&raw).map_err(|e| {
            format!("{} is not valid JSON ({}). Refusing to restore it.", c.backup.display(), e)
        })?;
        let mode = mode_of(&c.backup).or_else(|| mode_of(&c.settings)).unwrap_or(0o600);
        if c.settings.exists() {
            fs::copy(&c.settings, &c.safety)
                .map_err(|e| format!("cannot write {}: {}", c.safety.display(), e))?;
            say(&format!("settings: current file saved to {}", c.safety.display()));
        }
        write_atomic(&c.settings, &raw, mode)?;
        say(&format!("settings: restored from {}", c.backup.display()));
    } else if !c.settings.exists() {
        say(&format!("settings: {} does not exist - nothing to do", c.settings.display()));
    } else if blank_settings {
        say(&format!("settings: {} is empty - nothing to do", c.settings.display()));
    } else {
        let doc = read_file(&c.settings)?;
        let present = settings::env_raw_text(&doc, KEY)?.is_some();
        let had = state.as_ref().map(|s| s.env_had).unwrap_or(false);
        if !present && !had {
            say(&format!("settings: env.{} is not set - unchanged", KEY));
        } else if state.is_none() && !force {
            // Unreachable: the preflight above makes this decision before
            // anything is removed. Kept as a belt, and it must not remove the key
            // - with no record there is no way to tell our key apart from one the
            // user set themselves.
            say(&format!("settings: env.{} is set with no record - left alone", KEY));
        } else {
            let want = state.as_ref().and_then(|s| if s.env_had { s.env_raw.clone() } else { None });
            let env_was_there = state.as_ref().map(|s| s.env_object_had).unwrap_or(false);
            match settings::restore_env_key(&doc, KEY, want.as_deref(), env_was_there)? {
                Outcome::Unchanged => say(&format!(
                    "settings: env.{} already holds the value install found - unchanged",
                    KEY
                )),
                Outcome::Changed { text, .. } => {
                    let mode = mode_of(&c.settings).unwrap_or(0o600);
                    fs::copy(&c.settings, &c.safety)
                        .map_err(|e| format!("cannot write {}: {}", c.safety.display(), e))?;
                    say(&format!("settings: backed up to {}", c.safety.display()));
                    write_atomic(&c.settings, &text, mode)?;
                    match &want {
                        Some(raw) => say(&format!(
                            "settings: restored env.{} = {} (the value install found here)",
                            KEY,
                            String::from_utf8_lossy(raw)
                        )),
                        None => {
                            say(&format!("settings: removed env.{}", KEY));
                            let after = read_file(&c.settings)?;
                            let still = json::parse(&after)
                                .ok()
                                .and_then(|v| v.as_obj().map(|o| o.get("env").is_some()))
                                .unwrap_or(false);
                            say(if still {
                                "          \"env\" kept (it still has other keys)"
                            } else {
                                "          \"env\" was empty and was removed too"
                            });
                        }
                    }
                    say(&format!("          (mode {:04o} kept)", mode));
                }
            }
        }
    }

    // ---- 2. the symlink ---------------------------------------------------
    // AFTER settings.json, the mirror image of install's ordering and for the
    // same reason: the env key switches Claude Code's own title painting off and
    // the plugin paints the replacement, so the moment where only one of the two
    // is undone must be the moment where the KEY is already back. A failure
    // between them then leaves a working install rather than a blank tab.
    match link_state(&c.link) {
        LinkState::Link(t, _) => {
            let recorded = state.as_ref().and_then(|s| {
                if s.link_had {
                    s.link_target.clone()
                } else {
                    None
                }
            });
            fs::remove_file(&c.link)
                .map_err(|e| format!("cannot remove {}: {}", c.link.display(), e))?;
            match recorded {
                Some(old) if !old.is_empty() => {
                    let old = PathBuf::from(OsString::from_vec(old));
                    symlink(&old, &c.link)?;
                    say("symlink:  put back the target install found here");
                    say(&format!("          {} -> {}", c.link.display(), old.display()));
                }
                _ => {
                    say(&format!("symlink:  removed {}", c.link.display()));
                    say(&format!("          (was -> {})", t.display()));
                }
            }
        }
        LinkState::Absent => say("symlink:  not present - nothing to remove"),
        LinkState::Dir => return Err(refuse_link(&c.link, "a real directory, not a symlink")),
        LinkState::Other => return Err(refuse_link(&c.link, "not a symlink")),
    }

    // ---- 3. the state record ----------------------------------------------
    if c.state.exists() {
        fs::remove_file(&c.state)
            .map_err(|e| format!("cannot remove {}: {}", c.state.display(), e))?;
        say(&format!("state:    removed {}", c.state.display()));
    }

    say("");
    say("Done. Start a NEW Claude Code session for the change to take effect.");
    for b in [&c.backup, &c.safety] {
        if b.exists() {
            say(&format!(
                "A copy of settings.json is left at {} - delete it when you are happy.",
                b.display()
            ));
        }
    }
    say(&format!("The repo itself at {} was not touched.", c.repo.display()));
    Ok(())
}

// --- doctor -----------------------------------------------------------------

fn doctor(c: &Ctx) -> Result<(), String> {
    say(&format!("tabstatus {} ({})", env!("CARGO_PKG_VERSION"), target_triple()));
    say(&format!("repo:      {}", c.repo.display()));
    say(&format!("config:    {}", c.config.display()));

    let hook = c.hook_binary();
    match fs::metadata(&hook) {
        Ok(m) if m.permissions().mode() & 0o111 != 0 => {
            say(&format!("binary:    OK   {}", hook.display()))
        }
        Ok(_) => say(&format!("binary:    FAIL {} is not executable", hook.display())),
        Err(_) => say(&format!(
            "binary:    FAIL {} is missing - hooks/hooks.json invokes it, so \
             nothing paints",
            hook.display()
        )),
    }

    match link_state(&c.link) {
        LinkState::Absent => say(&format!(
            "plugin:    FAIL not linked. Run `tabstatus install`. ({})",
            c.link.display()
        )),
        LinkState::Link(t, true) if t == c.repo => {
            say(&format!("plugin:    OK   linked, {} -> {}", c.link.display(), t.display()))
        }
        LinkState::Link(t, true) => say(&format!(
            "plugin:    WARN {} points at {}, not at this repo",
            c.link.display(),
            t.display()
        )),
        LinkState::Link(t, false) => say(&format!(
            "plugin:    FAIL {} is a broken symlink to {}",
            c.link.display(),
            t.display()
        )),
        LinkState::Dir => say(&format!(
            "plugin:    WARN {} is a real directory, not a link to this repo",
            c.link.display()
        )),
        LinkState::Other => {
            say(&format!("plugin:    WARN {} exists and is not a symlink", c.link.display()))
        }
    }

    if let Some(t) = &c.settings_unresolved {
        say(&format!(
            "env key:   FAIL {} is a symlink to {}, which does not exist",
            c.settings.display(),
            t.display()
        ));
        say("           install and uninstall refuse to touch it. Fix or remove the link.");
    } else if c.settings.exists() {
        let doc = read_file(&c.settings)?;
        let blank = doc.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
        if blank {
            say(&format!(
                "env key:   FAIL {} is empty, so env.{} is not set. `tabstatus \
                 install` writes it.",
                c.settings.display(),
                KEY
            ));
        } else {
        match settings::env_value(&doc, KEY) {
            Err(e) => say(&format!("env key:   FAIL {} does not parse: {}", c.settings.display(), e)),
            Ok(None) => say(&format!(
                "env key:   FAIL env.{} is not set, so Claude Code repaints its own \
                 title over ours every 960ms",
                KEY
            )),
            Ok(Some(v)) if v == b"1" => say(&format!("env key:   OK   env.{} = \"1\"", KEY)),
            Ok(Some(v)) => say(&format!(
                "env key:   WARN env.{} = \"{}\", which is not \"1\"",
                KEY,
                String::from_utf8_lossy(&v)
            )),
        }
        }
        say(&format!(
            "settings:  {} (mode {:04o}){}",
            c.settings.display(),
            mode_of(&c.settings).unwrap_or(0),
            if c.settings_link.is_some() { ", reached through a symlink" } else { "" }
        ));
    } else {
        say(&format!("env key:   FAIL {} does not exist", c.settings.display()));
    }
    say(&format!(
        "state:     {}",
        if c.state.exists() {
            format!("{}", c.state.display())
        } else {
            format!("absent ({}), so uninstall will not remove the env key without --force", c.state.display())
        }
    ));

    // ---- what the runtime half would do right now -------------------------
    let konsole_vars = sh::env_set("KONSOLE_VERSION") || sh::env_set("KONSOLE_DBUS_SESSION");
    let mux = if sh::env_set("TMUX") {
        "tmux"
    } else if sh::env_set("STY") {
        "screen"
    } else {
        ""
    };
    let konsole = render::is_konsole();
    say(&format!(
        "terminal:  {}",
        if konsole {
            "Konsole (KONSOLE_* in the environment, no multiplexer)"
        } else if konsole_vars && !mux.is_empty() {
            "inside a multiplexer, so KONSOLE_* is ignored"
        } else if konsole_vars {
            "KONSOLE_* set"
        } else {
            "not Konsole (no KONSOLE_VERSION, no KONSOLE_DBUS_SESSION)"
        }
    ));
    if !mux.is_empty() {
        say(&format!("           multiplexer: {}, so no OSC 50 arming is sent", mux));
    }
    let pos = sh::env_str("CCTAB_GLYPH_POS");
    let implied: &str = if !pos.is_empty() {
        "from CCTAB_GLYPH_POS"
    } else if konsole {
        "suffix - Konsole elides the tab label from the left"
    } else {
        "prefix - the safe default; Windows Terminal truncates from the right"
    };
    say(&format!(
        "glyph:     {}{}",
        if pos.is_empty() {
            String::new()
        } else {
            format!("{} ", String::from_utf8_lossy(&pos))
        },
        implied
    ));
    match sh::env_raw("CLAUDE_PID") {
        Some(p) if !p.is_empty() => say(&format!(
            "pty:       CLAUDE_PID={} - session-start and session-end write it directly",
            String::from_utf8_lossy(&p)
        )),
        _ => say("pty:       CLAUDE_PID is not set, so this is not a hook subprocess \
                  (session-start and session-end would do nothing)"),
    }

    let title = current_title(b"idle");
    let mut line = b"title:     ".to_vec();
    line.extend_from_slice(&title);
    line.push(b'\n');
    let out = std::io::stdout();
    let mut l = out.lock();
    let _ = l.write_all(&line);
    let _ = l.write_all(b"           (what this directory would paint on an idle tab)\n");
    Ok(())
}

/// The runtime half's own pipeline, so `doctor` cannot drift from what paints.
fn current_title(edge: &[u8]) -> Vec<u8> {
    let cwd = location::cwd();
    let (place, in_repo) = location::place(&cwd);
    let place = render::apply_length_cap(place, in_repo);
    let place = render::apply_ssh_prefix(place);
    let place = render::sanitize(place);
    render::title(edge, place, render::is_konsole())
}
