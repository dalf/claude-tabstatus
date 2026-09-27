//! `install`, `uninstall`, `doctor`, `version` - the management half of the
//! binary.
//!
//! It shares the hook's binary, which was measured rather than assumed: merging it
//! cost -9us on the hot edge and 90KB of file size, because code that never runs is
//! never paged in. One file to copy, one path in hooks.json, one `--version`.
//!
//! What it changes:
//!
//!   1. one key   <config>/settings.json -> env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"
//!   2. a symlink <config>/skills/claude-tabstatus -> this repo
//!   3. a record  <config>/claude-tabstatus.state  -> what was there before
//!
//! settings.json is written FIRST and the symlink LAST, so a failure mid-edit
//! cannot leave the plugin loaded with Claude Code's built-in title repainting over
//! it - which looks broken rather than uninstalled. uninstall mirrors that for the
//! sharper version of the same reason: env key still set and plugin gone paints NO
//! title at all. Both halves preflight every refusal, so the window between the two
//! writes holds nothing that can decide to stop.
//!
//! `standalone` is a FOURTH thing, and it changes none of the three: it writes a
//! self-contained plugin tree from the manifests compiled into this binary (see
//! `src/embedded.rs`) and then runs the ordinary install against it, so a remote
//! machine needs one scp and one run instead of a tree. The embedded bytes are
//! authoritative only inside a tree this tool generated - `install` never reads
//! them and never writes a manifest, so a checkout's tracked files are not
//! reachable from any install path. `src/standalone.rs` carries why.
//!
//! CLAUDE_CONFIG_DIR overrides the config directory, which is how the tests
//! point all of this at a throwaway tree instead of a real one.

use crate::embedded::{self, Verdict};
use crate::settings::{self, Outcome};
use crate::standalone;
use crate::config::{self, Config, Terminal};
use crate::edge::{Glyph, Paint};
use crate::{json, render, state, tmux};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const KEY: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";
const PLUGIN: &str = "claude-tabstatus";
const STATE_VERSION: i32 = 2;

/// One of this half's options. Spelled here and nowhere else; which subcommand
/// accepts which is [`Subcommand::parse`]'s business.
enum Flag {
    Force,
    RestoreBackup,
    PurgeTree,
}

impl Flag {
    fn parse(a: &OsStr) -> Option<Flag> {
        match a.as_bytes() {
            b"--force" => Some(Flag::Force),
            b"--restore-backup" => Some(Flag::RestoreBackup),
            b"--purge-tree" => Some(Flag::PurgeTree),
            _ => None,
        }
    }
}

/// A management subcommand and its options, decided once from argv. Nothing below
/// this type looks at an argument again.
pub enum Subcommand {
    Install {
        force: bool,
    },
    Uninstall {
        force: bool,
        restore_backup: bool,
        purge_tree: bool,
    },
    Doctor,
    /// `standalone [<dir>]` - write a self-contained plugin tree from the copies
    /// compiled into this binary, then install it. The remote-install verb: scp
    /// one file, run it once.
    Standalone(Option<OsString>),
    /// `print-embedded <plugin|hooks>` - the embedded bytes on stdout, verbatim.
    /// Not in `usage`, because it exists for the test suite's byte-for-byte diff
    /// against the files in the repo and for a spot-check on a machine that has no
    /// source tree; it is the drift guard that works where a digest of `src/`
    /// cannot.
    PrintEmbedded(Option<OsString>),
    /// Print the two tmux format strings SessionStart would install. The test
    /// suite drives a private tmux server with EXACTLY the product's string
    /// rather than a copy of it, and a user who would rather pin the format in
    /// `~/.tmux.conf` than have it set at runtime can paste these two lines.
    TmuxFormat,
    /// `tmux-arm <tty>` - write Konsole's per-tab arming to ONE terminal. Invoked
    /// by the `client-attached` hook SessionStart installs, never by a hook event
    /// and never by hand in normal use: tmux substitutes the attaching client's
    /// pty, so a reattach - closing the laptop and coming back - lands in an armed
    /// tab instead of one governed by Konsole's `RemoteTabTitleFormat`.
    ///
    /// The argument is `None` only when a human typed the verb with nothing after
    /// it; the guard on the path itself is `emit::tty_write`.
    TmuxArm(Option<OsString>),
    Version,
    Help,
    /// A verb this half owns, given an option that verb does not accept. It
    /// carries the offending argument so the message can name it, and it is a
    /// variant rather than an early `Err` because parsing must not need a `Ctx`:
    /// `install --nope` is refused even where there is no config directory to
    /// find.
    BadOption(OsString),
}

impl Subcommand {
    /// `None` when argv[1] is not one of this half's names, which is what sends
    /// the run on to the paint path.
    ///
    /// The verbs are spelled ONCE, here, and are deliberately disjoint from the
    /// edge names - `working`, `waiting`, `idle`, `notify`, `session-start`,
    /// `session-end` - so no hook can reach this half, and no typo of a subcommand
    /// can reach the paint path carrying a real edge's name.
    pub fn parse(first: Option<&OsStr>, rest: &[OsString]) -> Option<Subcommand> {
        Some(match first?.as_bytes() {
            b"install" => install_options(rest),
            b"uninstall" => uninstall_options(rest),
            // These three take no options and IGNORE any argument given, which is
            // what they have always done: `doctor --force` runs doctor.
            b"doctor" => Subcommand::Doctor,
            b"standalone" => standalone_options(rest),
            b"print-embedded" => Subcommand::PrintEmbedded(rest.first().cloned()),
            b"tmux-format" => Subcommand::TmuxFormat,
            b"tmux-arm" => Subcommand::TmuxArm(rest.first().cloned()),
            b"version" | b"--version" | b"-V" => Subcommand::Version,
            b"help" | b"--help" | b"-h" => Subcommand::Help,
            _ => return None,
        })
    }

    /// The exit code. This half keeps its own: `install` exits non-zero on a
    /// refusal so a wrapper can see it. "Always exit 0" is the PAINT path's
    /// contract, not this one's.
    pub fn run(self) -> i32 {
        match self {
            Subcommand::Install { force } => with_ctx(|c| install(c, force)),
            Subcommand::Uninstall {
                force,
                restore_backup,
                purge_tree,
            } => with_ctx(|c| uninstall(c, force, restore_backup, purge_tree)),
            Subcommand::Doctor => with_ctx(doctor),
            // NOT `with_ctx`: the whole point is that there is no tree above the
            // running binary yet, so the repo cannot be resolved before one is
            // written.
            Subcommand::Standalone(dir) => match standalone_cmd(dir) {
                Ok(()) => 0,
                Err(e) => {
                    fail(&e);
                    1
                }
            },
            Subcommand::PrintEmbedded(which) => print_embedded(which.as_deref()),
            Subcommand::TmuxFormat => {
                for line in tmux::format_lines(&Config::from_env()) {
                    say(&line);
                }
                0
            }
            Subcommand::TmuxArm(tty) => match tty {
                Some(t) => {
                    tmux::arm_tty(&t);
                    0
                }
                None => {
                    fail("tmux-arm needs the pty to arm, e.g. /dev/pts/3");
                    1
                }
            },
            Subcommand::Version => {
                version();
                0
            }
            Subcommand::Help => {
                usage();
                0
            }
            Subcommand::BadOption(arg) => {
                fail(&format!(
                    "unknown option {}",
                    String::from_utf8_lossy(arg.as_bytes())
                ));
                1
            }
        }
    }
}

fn install_options(rest: &[OsString]) -> Subcommand {
    let mut force = false;
    for arg in rest {
        match Flag::parse(arg) {
            Some(Flag::Force) => force = true,
            // `--restore-backup` is a real flag, but not one install accepts.
            _ => return Subcommand::BadOption(arg.clone()),
        }
    }
    Subcommand::Install { force }
}

fn uninstall_options(rest: &[OsString]) -> Subcommand {
    let mut force = false;
    let mut restore_backup = false;
    let mut purge_tree = false;
    for arg in rest {
        match Flag::parse(arg) {
            Some(Flag::Force) => force = true,
            Some(Flag::RestoreBackup) => restore_backup = true,
            Some(Flag::PurgeTree) => purge_tree = true,
            None => return Subcommand::BadOption(arg.clone()),
        }
    }
    Subcommand::Uninstall {
        force,
        restore_backup,
        purge_tree,
    }
}

/// `standalone` takes a DIRECTORY, not options - but a mistyped flag must not be
/// silently treated as a directory name and materialised into `./--force`.
fn standalone_options(rest: &[OsString]) -> Subcommand {
    match rest.first() {
        Some(a) if a.as_bytes().starts_with(b"-") => Subcommand::BadOption(a.clone()),
        other => Subcommand::Standalone(other.cloned()),
    }
}

/// Resolve the config directory once, then run one command against it. The only
/// place a refusal from either half becomes a message on stderr and an exit code.
fn with_ctx<F: FnOnce(&Ctx) -> Result<(), String>>(f: F) -> i32 {
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
        "  tabstatus standalone [<dir>] write a self-contained plugin tree from the\n",
        "                               copies compiled into this binary, then install\n",
        "                               it - for a machine with no checkout on it\n",
        "  tabstatus uninstall          undo exactly that\n",
        "      --force                  remove the env key even with no state record\n",
        "      --restore-backup         roll settings.json back to the pre-install copy\n",
        "      --purge-tree             also remove a tree `standalone` generated\n",
        "  tabstatus doctor             report what is installed and what would paint\n",
        "  tabstatus tmux-format        print the two tmux format strings we install\n",
        "  tabstatus tmux-arm <tty>     re-arm one Konsole tab; tmux's client-attached\n",
        "                               hook runs this, so a reattach is armed again\n",
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

/// The config directory, resolved on its own because `standalone` needs it BEFORE
/// there is a tree to make a [`Ctx`] out of - it refuses a target under
/// `<config>/skills`.
///
/// Stays OsString: a HOME or CLAUDE_CONFIG_DIR that is not valid UTF-8 still names
/// a real directory, and repairing it here would edit a different one.
fn config_dir() -> Result<PathBuf, String> {
    match config::var_nonempty("CLAUDE_CONFIG_DIR") {
        Some(v) => Ok(PathBuf::from(v)),
        None => match config::var_nonempty("HOME") {
            Some(home) => Ok(PathBuf::from(home).join(".claude")),
            None => Err("neither CLAUDE_CONFIG_DIR nor HOME is set, so there \
                         is no config directory to work on"
                .to_string()),
        },
    }
}

impl Ctx {
    fn new() -> Result<Ctx, String> {
        Ctx::at(repo_root()?)
    }

    /// Everything in `new` except finding the plugin directory, so `standalone` can
    /// install the tree it has just written instead of one it had to discover.
    fn at(repo: PathBuf) -> Result<Ctx, String> {
        let config = config_dir()?;
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

    /// Which of the two shapes this plugin directory is. Asserted by the marker
    /// file `standalone` writes, never guessed: the same embedded-vs-on-disk
    /// comparison gets OPPOSITE remedies in the two modes, so a mode nobody can
    /// state is a report that gives the wrong advice.
    fn mode(&self) -> Mode {
        if standalone::is_generated(&self.repo) {
            Mode::Standalone
        } else {
            Mode::Checkout
        }
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
    if let Some(d) = plugin_dir_above(&exe) {
        return Ok(d);
    }
    // No tree above us, which is the normal shape of a binary scp'd to a VM. Two
    // fallbacks, in this order.
    //
    // FIRST the symlink `install` itself wrote. `<config>/skills/claude-tabstatus`
    // is the AUTHORITATIVE record of where the plugin directory is, written by the
    // same run that created the tree - so reading it is what makes `standalone
    // <dir>` a real option. Without it these two commands only ever worked for the
    // DEFAULT path: a tree anywhere else, or a default path that moved because
    // XDG_DATA_HOME is set in an interactive shell and not in `ssh vm '...'`, left a
    // live install that neither `doctor` nor `uninstall` could touch, under advice
    // whose only effect was to materialise a SECOND tree and orphan the first.
    //
    // The target is taken even when it does not exist or is incomplete. A dangling
    // link with a live env key and a state file behind it is exactly the broken
    // config `doctor` is for and `uninstall` has to clean up, and nothing here
    // writes into the repo path on that basis: `install` re-checks both manifests,
    // `--purge-tree` re-checks the marker, and a directory with no marker is never
    // removed.
    let recorded = config_dir().map(|c| c.join("skills").join(PLUGIN));
    if let Ok(link) = &recorded {
        if let Some(t) = link_target(link) {
            return Ok(t);
        }
    }
    // THEN the default location, for the one shape the link cannot cover: a
    // `standalone` that wrote its tree and then could not install it.
    if let Ok(t) = standalone::default_tree() {
        let complete = embedded::MANIFESTS.iter().all(|(rel, _)| t.join(rel).is_file());
        if standalone::is_generated(&t) && complete {
            return Ok(t);
        }
    }
    Err(format!(
        "{} is not inside a claude-tabstatus checkout (no .claude-plugin/plugin.json \
         and hooks/hooks.json above it), {} records no plugin directory, and there is \
         no generated tree at the default place to fall back on. Write one: \
         tabstatus standalone",
        exe.display(),
        match &recorded {
            Ok(l) => l.display().to_string(),
            Err(_) => "and with no config directory there is nothing that".to_string(),
        }
    ))
}

/// The plugin directory `<config>/skills/claude-tabstatus` points at, resolved
/// against the link's own directory when the link is relative.
///
/// A symlink, specifically: a real directory in that place is somebody else's
/// arrangement rather than a record this installer wrote, and `install` would have
/// refused to replace it.
fn link_target(link: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(link).ok()?.file_type().is_symlink() {
        return None;
    }
    let t = fs::read_link(link).ok()?;
    if t.is_absolute() {
        Some(t)
    } else {
        Some(link.parent()?.join(t))
    }
}

/// The upward walk itself: the plugin directory above `from`, or `None`.
fn plugin_dir_above(from: &Path) -> Option<PathBuf> {
    let mut dir = from.parent().map(|p| p.to_path_buf());
    for _ in 0..5 {
        let d = dir?;
        if d.join(".claude-plugin/plugin.json").is_file() && d.join("hooks/hooks.json").is_file() {
            return Some(d);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

// --- atomic writes ----------------------------------------------------------

fn mode_of(p: &Path) -> Option<u32> {
    fs::metadata(p).ok().map(|m| m.permissions().mode() & 0o7777)
}

/// Write through a temp file in the same directory and rename, with an explicit
/// mode: the settings.json we replace holds the user's `env` block, so a mode of
/// 0600 has to stay 0600. The shell installer widened it to 0644 through a
/// redirection, which is the bug this signature exists to make impossible.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
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
/// `dir_writable`'s probe and `write_atomic`'s temp file are named with the pid that
/// created them, so nothing collides with a stale one - and nothing used to clean
/// one up either. Both are per-run scratch, so a name whose pid is no longer in
/// /proc is litter by definition. A LIVE pid is left alone: a concurrent install's
/// temp file is the one thing here that must not be removed under it.
fn sweep_litter(dir: &Path) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let raw = e.file_name();
        let Some(name) = raw.to_str() else { continue };
        let Some(pid) = scratch_pid(name) else { continue };
        if !Path::new(&format!("/proc/{}", pid)).exists() {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// The pid inside one of this tool's two scratch names, or `None` for a name that
/// is not ours to remove.
///
/// `.cctab-wtest.<pid>` and `.<file>.cctab-tmp.<pid>` are the only two shapes, and
/// the pid must be all digits: nothing else in either directory ends in
/// `.<digits>` after one of those markers.
fn scratch_pid(name: &str) -> Option<&str> {
    let pid = name
        .strip_prefix(".cctab-wtest.")
        .or_else(|| name.rsplit_once(".cctab-tmp.").map(|(_, p)| p))?;
    let numeric = !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
    numeric.then_some(pid)
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

/// Three writes, in the order the module header explains, with every refusal
/// decided before the first of them.
fn install(c: &Ctx, force: bool) -> Result<(), String> {
    let hook = c.hook_binary();
    let existing = install_preflight(c, force, &hook)?;
    install_header(c, &hook);
    // Clear this tool's own scratch files left by a killed run, in the two
    // directories it is about to write.
    sweep_all(c);
    fs::create_dir_all(&c.config)
        .map_err(|e| format!("cannot create {}: {}", c.config.display(), e))?;

    record_prior_state(c, existing.as_deref())?;
    write_env_key(c, existing)?;
    link_the_plugin(c)?;

    say("");
    say("Done. Nothing else on this machine was modified.");
    say("Start a NEW Claude Code session for the plugin and the env key to take effect.");
    say("To undo: tabstatus uninstall");
    Ok(())
}

/// Every reason to refuse, and the settings document to edit if there is none.
///
/// `Ok(None)` means "write settings.json fresh": either it does not exist, or it
/// exists and holds nothing but whitespace, which Claude Code reads as no settings
/// at all.
fn install_preflight(c: &Ctx, force: bool, hook: &Path) -> Result<Option<Vec<u8>>, String> {
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
    Ok(existing)
}

/// What is about to be touched, before anything is.
fn install_header(c: &Ctx, hook: &Path) {
    say(&format!("repo:     {}", c.repo.display()));
    say(&format!("config:   {}", c.config.display()));
    if let Some(l) = &c.settings_link {
        say(&format!("settings: following the symlink {} -> {}", l.display(), c.settings.display()));
    }
    if hook.is_file() {
        say(&format!("binary:   {}", hook.display()));
    } else {
        // Only reachable with --force, which the preflight demands.
        say(&format!(
            "binary:   WARNING - {} is missing, so NOTHING will paint until you \
             build it.\n          Installing anyway because --force was given.\n\
                       Build it: sh scripts/build.sh",
            hook.display()
        ));
    }
    say("");
}

/// Step 1: what was here before we touched it, written once and never rewritten -
/// a second install must not record the state the FIRST one left.
fn record_prior_state(c: &Ctx, existing: Option<&[u8]>) -> Result<(), String> {
    if c.state.exists() {
        say(&format!("state:    {} exists - keeping the original record", c.state.display()));
    } else {
        let (link_had, link_target) = match link_state(&c.link) {
            LinkState::Link(t, _) => (true, Some(t.as_os_str().as_bytes().to_vec())),
            _ => (false, None),
        };
        let env_raw = match existing {
            Some(doc) => settings::env_raw_text(doc, KEY)?,
            None => None,
        };
        let env_object_had = match existing {
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
    Ok(())
}

/// Step 2: the one key, spliced into the document we parsed in the preflight.
fn write_env_key(c: &Ctx, existing: Option<Vec<u8>>) -> Result<(), String> {
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
    Ok(())
}

/// Step 3: the skills symlink. Everything here is preflighted, so a failure is a
/// surprise - a race, a full disk - and it has to say what state it leaves,
/// because settings.json has already been written by the time it runs.
fn link_the_plugin(c: &Ctx) -> Result<(), String> {
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

// --- standalone -------------------------------------------------------------

/// Which shape the plugin directory is, and therefore which of the two copies of a
/// manifest is the truth.
#[derive(PartialEq)]
enum Mode {
    /// A checkout: the FILE is the truth and a disagreeing binary is the stale
    /// thing, so the remedy is a rebuild.
    Checkout,
    /// A tree `standalone` generated: the BINARY is the truth and a disagreeing
    /// file is the stale thing, so the remedy is another `standalone`.
    Standalone,
}

/// Materialise a self-contained plugin tree, prove the copy runs, then install it.
///
/// Nothing about `install` changes: it still requires both manifests on disk and
/// still refuses without them, so this verb is the only thing in the binary that
/// can turn the embedded bytes into files - and it can only do it in a directory
/// that is absent, empty, or already carries our marker.
fn standalone_cmd(dir: Option<OsString>) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find my own path ({}), so there is nothing to copy", e))?;
    let config = config_dir()?;
    let skills = config.join("skills");
    let tree = match dir {
        Some(d) if d.is_empty() => return Err("the directory to materialise into is empty".into()),
        Some(d) => PathBuf::from(d),
        None => standalone::default_tree()?,
    };
    if let Some(why) = standalone::refuse_target(&tree, &skills) {
        return Err(why);
    }
    let above = plugin_dir_above(&exe);
    say(&format!("mode:     standalone - {}", standalone_mode_line(above.as_deref(), &tree)));
    // A checkout above us is the one place the two copies of a manifest can
    // disagree, and this is the one write path that turns the embedded bytes into
    // LIVE WIRING. `doctor` WARNs about exactly this drift in exactly this checkout;
    // staying silent here - at the moment a stale prebuilt binary's copy becomes the
    // hooks a session will run - was the last doorway the guard left open.
    if let Some(d) = above.as_deref() {
        if !standalone::is_generated(d) {
            warn_checkout_drift(d);
        }
    }
    for line in standalone::materialise(&tree, &exe, env!("CARGO_PKG_VERSION"), target_triple())? {
        say(&line);
    }
    // Before the install, not after: a tree whose binary cannot exec would set the
    // env key and leave a tab nothing paints, which is the half-state the whole
    // write ordering exists to prevent.
    say(&format!("verify:   {}", standalone::verify(&tree, env!("CARGO_PKG_VERSION"))?));
    say("");
    // The tree EXISTS by now, so install's refusals cannot be passed through as
    // they stand: they are worded for the install-only path, where nothing had been
    // written yet, and "Nothing has been changed." as the last line after five lines
    // reporting a complete tree contradicts itself - and it is the last line the
    // operator reads. Leaving the tree is right: it is inert, and a re-run reuses
    // it. Saying so is the part that was missing.
    Ctx::at(tree.clone()).and_then(|c| install(&c, false)).map_err(|e| {
        format!(
            "the tree at {} was written and verified, but the install did not happen: \
             {}\nFix that and re-run `tabstatus standalone`, or remove the tree.",
            tree.display(),
            without_nothing_changed(&e)
        )
    })
}

/// Which directory this run is writing, in terms of what is above the running
/// binary - and never calling a generated tree a checkout, which is the one
/// distinction this verb exists to keep straight. Refreshing the tree you are
/// running from is a normal thing to do, and it is not "a SEPARATE tree".
fn standalone_mode_line(above: Option<&Path>, tree: &Path) -> String {
    let Some(d) = above else {
        return "no checkout above the running binary, so the tree is written from \
                the copies compiled in"
            .to_string();
    };
    if !standalone::is_generated(d) {
        return format!(
            "running from the checkout at {}, materialising a SEPARATE tree (that \
             checkout is not touched)",
            d.display()
        );
    }
    let same = fs::canonicalize(d)
        .ok()
        .zip(fs::canonicalize(tree).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if same {
        format!("refreshing the generated tree this binary is running from, at {}", d.display())
    } else {
        format!(
            "running from the generated tree at {}, materialising a SEPARATE tree \
             (that tree is not touched)",
            d.display()
        )
    }
}

/// The copies compiled in, against the checkout the running binary came out of.
/// Only ever a WARN: the bytes that are about to land in the tree are the embedded
/// ones either way, and which of the two the operator MEANT is not ours to decide.
fn warn_checkout_drift(repo: &Path) {
    for (rel, text) in embedded::MANIFESTS {
        if let Verdict::Differs { on_disk, embedded: emb } = embedded::compare(repo, rel, text) {
            say(&format!(
                "          WARN {} in that checkout differs from the copy compiled in ({})",
                rel,
                embedded::differs_phrase(on_disk, emb)
            ));
            say(
                "          the tree carries the COMPILED-IN copy; rebuild with \
                 `sh scripts/build.sh` first if you meant to ship that edit",
            );
        }
    }
}

/// Install's refusals end "Nothing has been changed.", which is true when install is
/// the whole command and false once `standalone` has written a tree. Dropped rather
/// than contradicted.
fn without_nothing_changed(e: &str) -> &str {
    let t = e.trim_end();
    t.strip_suffix("Nothing has been changed.").unwrap_or(t).trim_end()
}

/// The embedded bytes, verbatim, so `tests/run.sh` can diff them against the files
/// and `doctor` has something to point at on a machine with no source tree.
fn print_embedded(which: Option<&OsStr>) -> i32 {
    let Some(name) = which else {
        fail(&format!("print-embedded needs a name: {}", embedded::NAMES));
        return 1;
    };
    match embedded::by_name(name.as_bytes()) {
        Some((_, text)) => {
            let out = std::io::stdout();
            let mut l = out.lock();
            // write_all, and NOT a trailing newline of our own: the whole value of
            // this verb is that its stdout is byte-for-byte the file.
            let _ = l.write_all(text.as_bytes());
            0
        }
        None => {
            fail(&format!(
                "no embedded file called {}; the names are: {}",
                String::from_utf8_lossy(name.as_bytes()),
                embedded::NAMES
            ));
            1
        }
    }
}

// --- uninstall --------------------------------------------------------------

/// What uninstall's preflight found, so that no decision below it has to re-read
/// a file or re-parse a document.
struct Prior {
    /// settings.json holds nothing but whitespace, which counts as "no settings"
    /// exactly as it does in install.
    blank_settings: bool,
    state: Option<State>,
}

/// The mirror of install: settings first and the link last. See the module header
/// for why that order is the sharper of the two.
fn uninstall(c: &Ctx, force: bool, restore_backup: bool, purge_tree: bool) -> Result<(), String> {
    let prior = match uninstall_preflight(c, force, restore_backup, purge_tree)? {
        Preflight::Go(prior) => prior,
        Preflight::Refused => return Ok(()),
    };
    uninstall_header(c, &prior);
    sweep_all(c);

    remove_env_key(c, &prior, force, restore_backup)?;
    unlink_the_plugin(c, &prior)?;
    remove_state(c)?;
    remove_records();
    // The tmux server's own set-titles pair, which SessionStart saved aside. Only
    // uninstall restores it: the options are server-wide, so a SessionEnd doing it
    // would unpaint the other claude windows still running.
    for line in tmux::uninstall() {
        say(&line);
    }

    // LAST, because it holds the binary this process is running. Unlinking a
    // running executable is fine on Linux; unlinking it before the writes above
    // would leave the hooks pointing at nothing if one of them failed.
    let purged = remove_tree(c, purge_tree)?;

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
    // Mode-specific, as remove_tree's own line already is: a generated tree is not
    // "the repo", and the `tree:` line above has already named it and said it was
    // kept. A repo path that is not there at all - a tree removed by hand, which
    // these commands can now still run on - gets no line either.
    if !purged && c.mode() == Mode::Checkout && c.repo.is_dir() {
        say(&format!("The repo itself at {} was not touched.", c.repo.display()));
    }
    Ok(())
}

/// The generated tree, which is the one thing an uninstall can be asked to remove
/// and by default does not.
///
/// A checkout is never removable here whatever flags are given - that is the
/// user's source - and a tree without our marker is not ours either. The default
/// NAMES the tree rather than staying silent, because the report ends "Done." and
/// a whole plugin directory left in `~/.local/share` is not something to find by
/// accident.
fn remove_tree(c: &Ctx, purge: bool) -> Result<bool, String> {
    let generated = c.mode() == Mode::Standalone;
    if !purge {
        if generated {
            say(&format!(
                "tree:     {} was NOT removed (it holds the binary). Remove it with \
                 `tabstatus uninstall --purge-tree`.",
                c.repo.display()
            ));
        }
        return Ok(false);
    }
    // What goes with it that we never wrote. `standalone` is scrupulous - prune only
    // ever touches the marker's list - so a few refresh runs teach the operator by
    // behaviour that their own files are safe in that directory. remove_dir_all takes
    // them anyway. Refusing would be over-cautious for a flag with `purge` in its
    // name; taking them SILENTLY is the part that is wrong.
    let known = match standalone::read_marker(&c.repo) {
        Some(Ok(m)) => m.files,
        _ => Vec::new(),
    };
    let extra = standalone::extra_files(&c.repo, &known);
    fs::remove_dir_all(&c.repo)
        .map_err(|e| format!("cannot remove {}: {}", c.repo.display(), e))?;
    say(&format!(
        "tree:     removed the generated tree {}{}",
        c.repo.display(),
        describe_extra(&extra)
    ));
    Ok(true)
}

/// The parenthesis `--purge-tree` adds when the tree held files nothing generated.
/// A few names, then a count: the point is that the operator can see what they lost,
/// not that the report reproduces a `find`.
fn describe_extra(extra: &[String]) -> String {
    if extra.is_empty() {
        return String::new();
    }
    const SHOW: usize = 5;
    let named: Vec<&str> = extra.iter().take(SHOW).map(String::as_str).collect();
    let more = extra.len() - named.len();
    format!(
        " ({} file{} it did not generate went with it: {}{})",
        extra.len(),
        if extra.len() == 1 { "" } else { "s" },
        named.join(", "),
        if more > 0 { format!(", and {} more", more) } else { String::new() }
    )
}

/// Either what uninstall needs, or the one refusal that is not a failure.
enum Preflight {
    Go(Prior),
    /// `env.KEY` is set with no record that we set it. Already reported on stderr,
    /// and the process exits 0: declining to remove a key we cannot prove we
    /// created is the correct outcome, not an error.
    Refused,
}

/// Every refusal, decided before anything is removed.
fn uninstall_preflight(
    c: &Ctx,
    force: bool,
    restore_backup: bool,
    purge_tree: bool,
) -> Result<Preflight, String> {
    if let Some(e) = refuse_unresolved(c) {
        return Err(e);
    }
    // Decided HERE, before anything is removed: `--purge-tree` against a checkout
    // must not first undo the install and then refuse.
    if purge_tree && c.mode() != Mode::Standalone {
        return Err(format!(
            "--purge-tree only removes a tree `tabstatus standalone` generated, and \
             {} carries no {} - it is a checkout, so its files are yours, not this \
             uninstaller's. Re-run without --purge-tree. Nothing has been changed.",
            c.repo.display(),
            standalone::MARKER
        ));
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
            return Ok(Preflight::Refused);
        }
    }
    Ok(Preflight::Go(Prior { blank_settings, state }))
}

/// What is about to be undone, before anything is.
fn uninstall_header(c: &Ctx, prior: &Prior) {
    say(&format!("config:   {}", c.config.display()));
    if let Some(l) = &c.settings_link {
        say(&format!("settings: following the symlink {} -> {}", l.display(), c.settings.display()));
    }
    match &prior.state {
        Some(_) => say(&format!("state:    {}", c.state.display())),
        None => say(&format!("state:    no record at {}", c.state.display())),
    }
    say("");
}

/// Step 1: put settings.json back - to the value install found, or to the
/// pre-install copy when `--restore-backup` asks for the whole file.
fn remove_env_key(c: &Ctx, prior: &Prior, force: bool, restore_backup: bool) -> Result<(), String> {
    let (blank_settings, state) = (prior.blank_settings, &prior.state);
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
    Ok(())
}

/// Step 2, AFTER settings.json: the mirror image of install's ordering and for the
/// same reason. The env key switches Claude Code's own title painting off and the
/// plugin paints the replacement, so the moment where only one of the two is undone
/// must be the moment where the KEY is already back. A failure between them then
/// leaves a working install rather than a blank tab.
fn unlink_the_plugin(c: &Ctx, prior: &Prior) -> Result<(), String> {
    let state = &prior.state;
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
    Ok(())
}

/// Step 3: the record itself, which has served its purpose by now.
fn remove_state(c: &Ctx) -> Result<(), String> {
    if c.state.exists() {
        fs::remove_file(&c.state)
            .map_err(|e| format!("cannot remove {}: {}", c.state.display(), e))?;
        say(&format!("state:    removed {}", c.state.display()));
    }
    Ok(())
}

/// The wait-ownership records, which are the OTHER thing an install leaves on the
/// disk. Best effort and unannounced when there is nowhere they could be: the
/// directory is created by the first edge that has something to record, so its
/// absence is the normal case.
///
/// Removing a still-LIVE session's record is harmless, because a session with no
/// record degrades to the stateless answer, which is exactly what it painted before
/// this plugin existed; and by this point the plugin is unlinked, so no hook of any
/// session will run again to write another. Without this the files sat in
/// `$XDG_RUNTIME_DIR` until logout, unmentioned by a report that ends "Done." and
/// "The repo itself was not touched" - which reads as a complete account of what is
/// left behind.
fn remove_records() {
    let Some((d, gone, dir_gone)) = state::purge() else {
        return;
    };
    if dir_gone {
        say(&format!("records:  removed {} ({gone} record(s))", d.display()));
    } else if gone > 0 {
        say(&format!(
            "records:  removed {gone} record(s) from {}, which holds other files",
            d.display()
        ));
    }
}

// --- doctor -----------------------------------------------------------------

/// What is installed, and what the runtime half would do right now.
///
/// Read-only by construction: the one command whose job is to explain a broken
/// config has to be able to run on the config that is broken.
fn doctor(c: &Ctx) -> Result<(), String> {
    say(&format!("tabstatus {} ({})", env!("CARGO_PKG_VERSION"), target_triple()));
    say(&format!("repo:      {}", c.repo.display()));
    report_mode(c);
    say(&format!("config:    {}", c.config.display()));
    report_binary(c);
    report_plugin(c);
    report_embedded(c);
    report_env_key(c)?;
    report_state(c);
    report_record();
    report_runtime();
    report_tmux();
    report_title();
    Ok(())
}

/// Which shape the plugin directory is, where it is, and - in a generated tree -
/// whether it still matches the binary that is running.
///
/// The mode has to be STATED, not left for the reader to infer, because every
/// remedy below it differs between the two: in a checkout the file on disk is the
/// truth and a stale binary is rebuilt, and in a generated tree the binary is the
/// truth and a stale tree is re-materialised.
fn report_mode(c: &Ctx) {
    // A plugin directory that is NOT THERE gets neither mode: the mode is read off a
    // marker inside it, and calling a path that does not exist "a checkout" is the
    // same mis-wording this slice removed everywhere else. Reachable because
    // `repo_root` now takes the location from the symlink install wrote, so a tree
    // deleted by hand leaves a config these commands can still explain and clean up.
    if !c.repo.is_dir() {
        say(&format!(
            "mode:      FAIL the plugin directory {} is not there, so nothing can load \
             and neither mode can be read off it. `tabstatus uninstall` clears what is \
             left of the install; `tabstatus standalone` writes a fresh tree.",
            c.repo.display()
        ));
        return;
    }
    match c.mode() {
        Mode::Checkout => {
            say(&format!(
                "mode:      checkout - no {} above, so the manifests on disk are the \
                 truth and the copies compiled in are a projection",
                standalone::MARKER
            ));
        }
        Mode::Standalone => {
            say(&format!(
                "mode:      standalone - {} says this tree was materialised from a \
                 binary, so the BINARY is the truth here",
                standalone::MARKER
            ));
            match standalone::read_marker(&c.repo) {
                Some(Ok(m)) => {
                    say(&format!(
                        "           generated by tabstatus {} ({})",
                        String::from_utf8_lossy(&m.version),
                        String::from_utf8_lossy(&m.target)
                    ));
                    // A tree materialised by one architecture and later run by
                    // another: the hooks would exec a binary this machine cannot.
                    if m.target != target_triple().as_bytes() {
                        say(&format!(
                            "           WARN the tree was written by a {} binary and this \
                             one is {}",
                            String::from_utf8_lossy(&m.target),
                            target_triple()
                        ));
                    }
                }
                Some(Err(e)) => say(&format!("           WARN {} is unreadable: {}", standalone::MARKER, e)),
                // Unreachable: the mode IS the marker's presence.
                None => say(&format!("           WARN {} vanished while reading it", standalone::MARKER)),
            }
            report_tree_binary(c);
        }
    }
}

/// Is the tree's binary the one running? The one way to get a stale tree is to scp
/// a newer binary straight over `<tree>/bin/tabstatus` and skip `standalone`, and
/// this is the line that catches it.
fn report_tree_binary(c: &Ctx) {
    let tree_bin = c.repo.join(standalone::BIN);
    let exe = std::env::current_exe().ok();
    let same_path = exe
        .as_ref()
        .and_then(|e| fs::canonicalize(e).ok())
        .zip(fs::canonicalize(&tree_bin).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if same_path {
        say("           tree binary: the one running this report");
        return;
    }
    let both = exe.as_ref().and_then(|e| fs::read(e).ok()).zip(fs::read(&tree_bin).ok());
    match both {
        Some((a, b)) if a == b => say("           tree binary: identical to the one running"),
        Some((a, b)) => {
            // Same size and different bytes is the LIKELY shape of this, because a
            // version bump rarely changes the length, so "635440 vs 635440" would
            // read as a bug in the report rather than as the answer.
            say(&format!(
                "           WARN tree binary DIFFERS from the one running ({})",
                if a.len() == b.len() {
                    format!("same {} bytes, different content", a.len())
                } else {
                    format!("{} in the tree vs {} running", b.len(), a.len())
                }
            ));
            say("           refresh the tree from this binary: tabstatus standalone");
        }
        None => say(&format!("           WARN cannot compare {} with the running binary", tree_bin.display())),
    }
}

/// The two manifests, against the copies compiled into this binary.
///
/// WARN, never FAIL, in a checkout: a mid-edit `hooks/hooks.json` is a normal
/// working-tree state and the one command people run daily must not cry wolf over
/// it. In a generated tree it is still WARN, but the remedy is the opposite one.
fn report_embedded(c: &Ctx) {
    let standalone_mode = c.mode() == Mode::Standalone;
    let remedy = if standalone_mode {
        "this tree was generated, so the BINARY is the truth: refresh it with \
         `tabstatus standalone`"
    } else {
        "this is a checkout, so the FILE is the truth: rebuild with `sh scripts/build.sh`"
    };
    for (rel, text) in embedded::MANIFESTS {
        match embedded::compare(&c.repo, rel, text) {
            Verdict::Same => say(&format!("embedded:  OK   {} matches the copy compiled in", rel)),
            Verdict::Differs { on_disk, embedded: emb } => {
                say(&format!(
                    "embedded:  WARN {} differs from the copy compiled in ({})",
                    rel,
                    embedded::differs_phrase(on_disk, emb)
                ));
                say(&format!("           {}", remedy));
            }
            Verdict::Missing => say(&format!(
                "embedded:  FAIL {} is missing, so Claude Code cannot load the plugin. \
                 The copy compiled in is intact: {}",
                rel,
                if standalone_mode { "re-run `tabstatus standalone`" } else { "restore it from git" }
            )),
            Verdict::Unreadable(e) => {
                say(&format!("embedded:  WARN {} cannot be read: {}", rel, e))
            }
        }
    }
}

/// The binary hooks.json invokes, which is the one thing whose absence makes every
/// hook exit 127.
fn report_binary(c: &Ctx) {
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
}

fn report_plugin(c: &Ctx) {
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
}

/// The env key, and the file it lives in.
///
/// This is the one branch of the report that can end it: an I/O error reading
/// settings.json propagates, so a settings.json that is a DIRECTORY aborts the
/// report at this line where every other broken shape is described and exits 0.
/// That is the reference implementation's behaviour, reproduced deliberately;
/// changing it belongs to a slice that is allowed to change behaviour.
fn report_env_key(c: &Ctx) -> Result<(), String> {
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
    Ok(())
}

fn report_state(c: &Ctx) {
    say(&format!(
        "state:     {}",
        if c.state.exists() {
            format!("{}", c.state.display())
        } else {
            format!("absent ({}), so uninstall will not remove the env key without --force", c.state.display())
        }
    ));
}

/// The wait-ownership records: where they live, whether the layer is on at all,
/// what each record holds, and which of them the next session-start will reap.
///
/// "disabled" is the silent answer to almost every question this layer can raise -
/// a tab behaving exactly as it did before the layer existed is indistinguishable
/// from a layer that is working correctly - so the reason is printed, not implied.
///
/// Read-only, deliberately: it NAMES the stale files rather than deleting them,
/// because the command you run when something is already wrong must not be the one
/// that removes the evidence. The verdicts come from the same function the reaper
/// calls, so the report cannot drift from the behaviour.
fn report_record() {
    let s = state::survey();
    let dir = match &s.dir {
        Err(why) => {
            say(&format!("record:    disabled - {why}"));
            say(
                "           so every edge falls back to the stateless answer, which is \
                 the behaviour from before wait ownership existed",
            );
            return;
        }
        Ok(d) => d,
    };
    say(&format!(
        "record:    {} ({} record{}, {} stale)",
        dir.display(),
        s.records.len(),
        if s.records.len() == 1 { "" } else { "s" },
        s.stale
    ));
    // Before anything about what is in there, because a directory nothing can be
    // written to is the one failure mode every OTHER line of this report renders as
    // healthy - "nothing recorded" included - while the tab is stuck orange.
    if let Some(why) = s.writable {
        say(&format!("           FAIL {why}"));
    }
    if s.records.is_empty() {
        say(
            "           nothing recorded, which is also what a session that has raised \
             no dialog leaves behind",
        );
    }
    for (name, what) in &s.records {
        say(&format!("           {name}: {what}"));
    }
}

/// What the runtime half would decide from this environment: which terminal, which
/// glyph position, and whether there is a pty to write to.
fn report_runtime() {
    let konsole_vars = config::flag("KONSOLE_VERSION") || config::flag("KONSOLE_DBUS_SESSION");
    let mux = if config::flag("TMUX") {
        "tmux"
    } else if config::flag("STY") {
        "screen"
    } else {
        ""
    };
    let konsole = Terminal::detect() == Terminal::Konsole;
    // The REASON matters more than the answer, because there are now three of
    // them and they disagree: an explicit CCTAB_TERMINAL, inherited KONSOLE_*,
    // and a multiplexer that makes the inherited kind meaningless.
    let override_ = config::var_nonempty("CCTAB_TERMINAL");
    say(&format!(
        "terminal:  {}",
        match (&override_, konsole, konsole_vars, mux.is_empty()) {
            (Some(v), true, _, _) => format!(
                "Konsole, from CCTAB_TERMINAL={} - the only signal that survives ssh",
                String::from_utf8_lossy(v.as_bytes())
            ),
            (Some(v), false, _, _) => format!(
                "not Konsole: CCTAB_TERMINAL={} says so explicitly",
                String::from_utf8_lossy(v.as_bytes())
            ),
            (None, true, _, _) => "Konsole (KONSOLE_* in the environment, no multiplexer)".to_owned(),
            (None, false, true, false) =>
                "inside a multiplexer, so the inherited KONSOLE_* is ignored - set \
                 CCTAB_TERMINAL=konsole if the outer terminal really is Konsole"
                    .to_owned(),
            (None, false, true, true) => "KONSOLE_* set".to_owned(),
            (None, false, false, _) =>
                "not Konsole (no CCTAB_TERMINAL, no KONSOLE_VERSION, no \
                 KONSOLE_DBUS_SESSION)"
                    .to_owned(),
        }
    ));
    if !mux.is_empty() {
        say(&format!(
            "           multiplexer: {}, so KONSOLE_* says nothing about the outer \
             terminal",
            mux
        ));
        if !konsole {
            say(
                "           If the outer terminal IS Konsole, set \
                 CCTAB_TERMINAL=konsole: it moves the strip to the end",
            );
            say("           Konsole does not elide, and arms the tab so the title \
                 shows there at all.");
        }
    }
    let pos = config::var_nonempty("CCTAB_GLYPH_POS");
    let implied: &str = if pos.is_some() {
        "from CCTAB_GLYPH_POS"
    } else if konsole {
        "suffix - Konsole elides the tab label from the left"
    } else {
        "prefix - the safe default; Windows Terminal truncates from the right"
    };
    say(&format!(
        "glyph:     {}{}",
        match &pos {
            None => String::new(),
            Some(p) => format!("{} ", String::from_utf8_lossy(p.as_bytes())),
        },
        implied
    ));
    match config::var_nonempty("CLAUDE_PID") {
        Some(p) => say(&format!(
            "pty:       CLAUDE_PID={} - session-start and session-end write it directly",
            String::from_utf8_lossy(p.as_bytes())
        )),
        None => say("pty:       CLAUDE_PID is not set, so this is not a hook subprocess \
                  (session-start and session-end would do nothing)"),
    }
}

/// Inside tmux or not, the socket, the pane, whether the decay's clock is running,
/// whether the outer terminal can be given a title at all, and whose
/// set-titles-string is installed.
///
/// Two tmux invocations, both read-only, both on a cold path. Nothing here is a
/// copy of what the runtime half decides: [`tmux::report`] is in the module that
/// decides it.
fn report_tmux() {
    for line in tmux::report(&Config::from_env()) {
        say(&line);
    }
}

/// The runtime half's own pipeline, CALLED rather than copied, so this line cannot
/// drift from what actually paints.
fn report_title() {
    let title = render::compose(Paint::Line(Glyph::Idle), &Config::from_env()).title;
    let mut line = b"title:     ".to_vec();
    line.extend_from_slice(title.as_bytes());
    line.push(b'\n');
    let out = std::io::stdout();
    let mut l = out.lock();
    let _ = l.write_all(&line);
    let _ = l.write_all(b"           (what this directory would paint on an idle tab)\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Subcommand {
        let all: Vec<OsString> = words.iter().map(OsString::from).collect();
        let (first, rest) = all.split_first().expect("at least a verb");
        Subcommand::parse(Some(first), rest).expect("a management verb")
    }

    fn not_a_subcommand(word: Option<&str>) -> bool {
        let first = word.map(OsString::from);
        Subcommand::parse(first.as_deref(), &[]).is_none()
    }

    #[test]
    fn every_verb_and_every_spelling_of_one_parses() {
        assert!(matches!(parse(&["install"]), Subcommand::Install { force: false }));
        assert!(matches!(
            parse(&["uninstall"]),
            Subcommand::Uninstall { force: false, restore_backup: false, purge_tree: false }
        ));
        assert!(matches!(parse(&["standalone"]), Subcommand::Standalone(None)));
        assert!(matches!(parse(&["doctor"]), Subcommand::Doctor));
        for w in ["version", "--version", "-V"] {
            assert!(matches!(parse(&[w]), Subcommand::Version), "{}", w);
        }
        for w in ["help", "--help", "-h"] {
            assert!(matches!(parse(&[w]), Subcommand::Help), "{}", w);
        }
    }

    #[test]
    fn no_edge_and_no_typo_of_a_verb_reaches_this_half() {
        // The disjointness the module header rests on: every edge name, and the
        // near-misses, fall through to the paint path.
        for w in [
            "working", "waiting", "idle", "notify", "session-start", "session-end",
            "", "Install", "instal", "installx", "--force", "doctor ", "-v",
        ] {
            assert!(not_a_subcommand(Some(w)), "{:?} must not be a subcommand", w);
        }
        assert!(not_a_subcommand(None), "no argv at all is not a subcommand");
    }

    #[test]
    fn a_flag_attaches_only_to_the_verb_that_accepts_it() {
        assert!(matches!(parse(&["install", "--force"]), Subcommand::Install { force: true }));
        assert!(matches!(
            parse(&["uninstall", "--force"]),
            Subcommand::Uninstall { force: true, restore_backup: false, purge_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--restore-backup"]),
            Subcommand::Uninstall { force: false, restore_backup: true, purge_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--restore-backup", "--force"]),
            Subcommand::Uninstall { force: true, restore_backup: true, purge_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--purge-tree"]),
            Subcommand::Uninstall { force: false, restore_backup: false, purge_tree: true }
        ));
        // Repeats are idempotent, as they were.
        assert!(matches!(parse(&["install", "--force", "--force"]), Subcommand::Install { force: true }));
    }

    #[test]
    fn an_option_the_verb_does_not_accept_is_named_rather_than_ignored() {
        // `--restore-backup` is a real flag, but install has never taken it.
        for words in [
            &["install", "--restore-backup"][..],
            &["install", "--bogus"][..],
            &["install", "--force", "--bogus"][..],
            &["uninstall", "--bogus"][..],
            // `--purge-tree` is a real flag, but only uninstall takes it.
            &["install", "--purge-tree"][..],
            // `standalone` takes a DIRECTORY; a mistyped flag must not become one.
            &["standalone", "--force"][..],
            &["standalone", "-d"][..],
        ] {
            match parse(words) {
                Subcommand::BadOption(a) => assert_eq!(a, OsString::from(words[words.len() - 1])),
                _ => panic!("{:?} should be a BadOption", words),
            }
        }
    }

    #[test]
    fn the_three_read_only_verbs_ignore_their_arguments() {
        // Reproduced, not improved: `doctor --force` has always run doctor.
        assert!(matches!(parse(&["doctor", "--force", "--bogus"]), Subcommand::Doctor));
        assert!(matches!(parse(&["version", "--bogus"]), Subcommand::Version));
        assert!(matches!(parse(&["help", "--bogus"]), Subcommand::Help));
    }

    /// `standalone <dir>` has to carry the directory through, and the two new verbs
    /// must be as disjoint from the edge names as the old ones.
    #[test]
    fn standalone_carries_its_directory_and_print_embedded_its_name() {
        match parse(&["standalone", "/tmp/somewhere"]) {
            Subcommand::Standalone(Some(d)) => assert_eq!(d, OsString::from("/tmp/somewhere")),
            _ => panic!("standalone must carry the directory"),
        }
        match parse(&["print-embedded", "hooks"]) {
            Subcommand::PrintEmbedded(Some(n)) => assert_eq!(n, OsString::from("hooks")),
            _ => panic!("print-embedded must carry the name"),
        }
        assert!(matches!(parse(&["print-embedded"]), Subcommand::PrintEmbedded(None)));
        // Near-misses of the new verbs stay on the paint path, exactly as the old
        // ones do.
        for w in ["standalone ", "Standalone", "stand-alone", "print-embed", "embedded"] {
            assert!(not_a_subcommand(Some(w)), "{:?} must not be a subcommand", w);
        }
    }

    /// `print-embedded` and `doctor` both rest on the embedded copies being reachable
    /// from this module, and the paths they name are the ones a generated tree gets.
    #[test]
    fn the_embedded_manifests_are_the_two_files_a_plugin_tree_needs() {
        let paths: Vec<&str> = embedded::MANIFESTS.iter().map(|(p, _)| *p).collect();
        assert_eq!(paths, vec![".claude-plugin/plugin.json", "hooks/hooks.json"]);
        // install_preflight demands exactly these two on disk, so the embedded set
        // and the required set cannot drift apart.
        for (rel, _) in embedded::MANIFESTS {
            assert!(
                [".claude-plugin/plugin.json", "hooks/hooks.json"].contains(&rel),
                "{} is embedded but not required by install_preflight",
                rel
            );
        }
    }

    #[test]
    fn only_this_tools_own_scratch_names_with_a_numeric_pid_are_litter() {
        assert_eq!(scratch_pid(".cctab-wtest.123"), Some("123"));
        assert_eq!(scratch_pid(".settings.json.cctab-tmp.4567"), Some("4567"));
        assert_eq!(scratch_pid(".cctab-tmp.9"), Some("9"));
        // Ours in shape but not in pid.
        assert_eq!(scratch_pid(".cctab-wtest.notanumber"), None);
        assert_eq!(scratch_pid(".cctab-wtest."), None);
        assert_eq!(scratch_pid(".settings.json.cctab-tmp."), None);
        assert_eq!(scratch_pid(".cctab-wtest.12a"), None);
        // Not ours at all.
        assert_eq!(scratch_pid("settings.json"), None);
        assert_eq!(scratch_pid(".settings.json.cctab-preinstall"), None);
        assert_eq!(scratch_pid(".bashrc"), None);
        assert_eq!(scratch_pid("cctab-wtest.123"), None);
    }

    fn ctx() -> Ctx {
        let config = PathBuf::from("/cfg");
        Ctx {
            repo: PathBuf::from("/repo"),
            skills: config.join("skills"),
            link: config.join("skills").join(PLUGIN),
            settings: config.join("settings.json"),
            settings_link: None,
            settings_unresolved: None,
            state: config.join(format!("{}.state", PLUGIN)),
            backup: config.join("settings.json.cctab-preinstall"),
            safety: config.join("settings.json.cctab-preuninstall"),
            config,
        }
    }

    #[test]
    fn with_suffix_appends_to_the_whole_name_not_to_the_stem() {
        let p = with_suffix(Path::new("/a/settings.json"), ".cctab-preinstall");
        assert_eq!(p, PathBuf::from("/a/settings.json.cctab-preinstall"));
    }

    /// The record `uninstall` reads back, so its writer has to produce something
    /// this crate's own parser accepts.
    #[test]
    fn the_state_record_is_json_and_round_trips_what_uninstall_reads() {
        let s = State {
            env_had: true,
            // The value's original SOURCE text, quotes included.
            env_raw: Some(b"\"0\"".to_vec()),
            env_object_had: true,
            link_had: true,
            link_target: Some(b"/elsewhere".to_vec()),
        };
        let raw = state_text(&ctx(), &s);
        let v = json::parse(&raw).expect("the record is valid JSON");
        let root = v.as_obj().expect("an object at the top");
        let field = |m: &str, k: &str| -> Option<Vec<u8>> {
            root.get(m)?.val.as_obj()?.get(k)?.val.as_str().map(<[u8]>::to_vec)
        };
        let flag = |m: &str, k: &str| -> Option<bool> {
            root.get(m)?.val.as_obj()?.get(k)?.val.as_bool()
        };
        assert!(root.get("state_version").is_some());
        assert_eq!(flag("env_key_before", "had"), Some(true));
        assert_eq!(field("env_key_before", "raw").as_deref(), Some(&b"\"0\""[..]));
        assert_eq!(flag("env_object_before", "had"), Some(true));
        assert_eq!(flag("symlink_before", "had"), Some(true));
        assert_eq!(field("symlink_before", "target").as_deref(), Some(&b"/elsewhere"[..]));
    }

    #[test]
    fn a_record_with_nothing_recorded_writes_explicit_nulls() {
        let s = State {
            env_had: false,
            env_raw: None,
            env_object_had: false,
            link_had: false,
            link_target: None,
        };
        let raw = state_text(&ctx(), &s);
        json::parse(&raw).expect("the record is valid JSON");
        let text = String::from_utf8(raw).expect("ascii paths give ascii output");
        assert!(text.contains("\"raw\": null"), "{}", text);
        assert!(text.contains("\"target\": null"), "{}", text);
    }

    /// A config directory whose name is not valid UTF-8 still has to produce a
    /// record the parser accepts - and the per-byte replacement rule the length cap
    /// depends on elsewhere is the same one `json::quote` applies here.
    #[test]
    fn an_invalid_utf8_path_is_quoted_one_replacement_per_byte() {
        let mut c = ctx();
        c.repo = PathBuf::from(OsString::from_vec(b"/repo/tr\xf0\x9f\x98x".to_vec()));
        let raw = state_text(&c, &State {
            env_had: false,
            env_raw: None,
            env_object_had: false,
            link_had: false,
            link_target: None,
        });
        let v = json::parse(&raw).expect("still valid JSON");
        let repo = v.as_obj().and_then(|o| o.get("repo")).and_then(|m| m.val.as_str());
        // Three bytes of a truncated four-byte sequence: three U+FFFD, not one.
        assert_eq!(repo, Some("/repo/tr\u{fffd}\u{fffd}\u{fffd}x".as_bytes()));
    }

    #[test]
    fn the_target_triple_names_the_build_that_is_running() {
        let t = target_triple();
        assert!(t.contains('-'), "{}", t);
        assert_ne!(t, "unknown-target", "this crate is built for a named target");
    }
}
