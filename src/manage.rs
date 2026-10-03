//! `install`, `uninstall`, `doctor`, `version` - the management half of the
//! binary.
//!
//! It shares the hook's binary, which was measured rather than assumed: merging it
//! cost -9us on the hot edge and 90KB of file size, because code that never runs is
//! never paged in. One file to copy, one path in hooks.json, one `--version`.
//!
//! What `install` changes:
//!
//!   1. a tree     <data>/claude-tabstatus/        <- the plugin directory itself
//!   2. one key    <config>/settings.json -> env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"
//!   3. a symlink  <config>/skills/claude-tabstatus -> that tree
//!   4. a record   <config>/claude-tabstatus.state -> the prior state, and the tree
//!
//! On Windows the link in 3 is a directory JUNCTION (`sys::link_dir`): a directory
//! symlink there needs Developer Mode or elevation, a junction needs neither, and std
//! reads either back as a symlink - so below, "symlink" means whichever the platform
//! made, and every report line says which through `sys::DIR_LINK`.
//!
//! THE PLUGIN DIRECTORY IS BUILD OUTPUT, like `bin/`. It is materialised from the
//! manifests compiled into this binary (`src/embedded.rs`), never symlinked to a
//! checkout, and `src/tree.rs` carries why: when the checkout WAS the plugin
//! directory, `git checkout` of a branch with a broken hooks.json broke every prompt
//! in every running session with no deploy step in sight. There is one install path
//! and it always writes the tree - in a checkout and on a bare VM alike - so nothing
//! here has to ask which shape the plugin directory is.
//!
//! The tree is written FIRST, then settings.json, and the symlink LAST, which is one
//! step in front of the ordering this module always had. The reason for the last two
//! is unchanged: the env key switches Claude Code's own title painting off and the
//! plugin paints the replacement, so "key set, plugin gone" is the one combination
//! that paints NO title at all. The tree goes before both because it is INERT until
//! the symlink points at it - so an abort anywhere before that last step leaves a
//! first-time user exactly as they were. `uninstall` mirrors it: settings first, the
//! link next, the tree last. Both halves preflight every refusal, so the window
//! between the writes holds nothing that can decide to stop.
//!
//! And the symlink is repointed with rename(2), not unlink-then-symlink. Hooks fire
//! constantly; a path that resolves to nothing for even a moment is a hook exec'ing a
//! missing file, and a non-zero PreToolUse hook BLOCKS A TOOL. On Windows the repoint
//! goes through `sys::replace_dir_link`: one rename on NTFS, atomic like this one, and
//! where a filesystem refuses that, a rename-aside sequence with a window of two
//! renames that no ordering here can close - the seam documents it. What Windows CAN
//! always do is fail early: the install preflight makes and removes a probe junction,
//! so a volume that refuses one refuses it before settings.json is touched.
//!
//! CLAUDE_CONFIG_DIR overrides the config directory, which is how the tests
//! point all of this at a throwaway tree instead of a real one.

use crate::embedded::{self, Verdict};
use crate::settings::{self, Outcome};
use crate::tree;
use crate::config::{self, Config};
use crate::edge::{Glyph, Paint};
use crate::surface::{self, Elide, Surface, Terminator};
use crate::support::{Presence, Support, YES};
use crate::mux::{tmux, MuxKind, Stack};
use crate::{json, location, render, state, sys};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

const KEY: &str = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE";
const PLUGIN: &str = "claude-tabstatus";
const STATE_VERSION: i32 = 3;

/// One of this half's options. Spelled here and nowhere else; which subcommand
/// accepts which is [`Subcommand::parse`]'s business.
enum Flag {
    Force,
    RestoreBackup,
    KeepTree,
    /// `--tree <dir>`, install's only option that takes a value.
    Tree,
    /// `--surface <name>`, doctor's only option, and the other one here that takes
    /// a value.
    Surface,
}

impl Flag {
    fn parse(a: &OsStr) -> Option<Flag> {
        match a.as_encoded_bytes() {
            b"--force" => Some(Flag::Force),
            b"--restore-backup" => Some(Flag::RestoreBackup),
            b"--keep-tree" => Some(Flag::KeepTree),
            b"--tree" => Some(Flag::Tree),
            b"--surface" => Some(Flag::Surface),
            _ => None,
        }
    }
}

/// A management subcommand and its options, decided once from argv. Nothing below
/// this type looks at an argument again.
pub enum Subcommand {
    /// `install [--tree <dir>]` - materialise the plugin tree from the copies
    /// compiled into this binary, then link it and set the env key. ONE path, in a
    /// checkout and on a bare VM alike.
    Install {
        tree: Option<OsString>,
        /// Write settings.json even on a filesystem with no Windows ACL to keep.
        force: bool,
    },
    Uninstall {
        force: bool,
        restore_backup: bool,
        keep_tree: bool,
    },
    /// `doctor [--surface <name>]` - what is installed and what would paint, or,
    /// with a surface named, the capability table of a terminal THIS MACHINE CANNOT
    /// RUN. The table is `&'static` data all the way down, which is how the Windows
    /// column gets read before any Windows box exists.
    Doctor {
        surface: Option<OsString>,
    },
    /// The word `standalone` used to be a second install verb, for a machine with no
    /// checkout. `install` now does exactly what it did, everywhere, so the word is
    /// kept only long enough to say so: a name that used to work and now errors
    /// obscurely is worse than one that points at its replacement.
    StandaloneGone,
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
    /// it; the guard on the path itself is `sys::write_tty`.
    TmuxArm(Option<OsString>),
    Version,
    Help,
    /// A verb this half owns, given an option that verb does not accept. It
    /// carries the offending argument so the message can name it, and it is a
    /// variant rather than an early `Err` because parsing must not need a `Ctx`:
    /// `install --nope` is refused even where there is no config directory to
    /// find.
    BadOption(OsString),
    /// Something this half owns, used wrongly in a way `BadOption` cannot say:
    /// `--tree` with nothing after it, or a bare directory where `--tree` is needed.
    BadUsage(String),
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
        Some(match first?.as_encoded_bytes() {
            b"install" => install_options(rest),
            b"uninstall" => uninstall_options(rest),
            b"doctor" => doctor_options(rest),
            // These two take no options and IGNORE any argument given, which is
            // what they have always done.
            b"standalone" => Subcommand::StandaloneGone,
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
            // NOT `with_ctx`: install resolves its OWN tree - it is the command that
            // decides where the plugin directory is, rather than discovering one.
            Subcommand::Install { tree, force } => match install(tree, force) {
                Ok(()) => 0,
                Err(e) => {
                    fail(&e);
                    1
                }
            },
            Subcommand::Uninstall {
                force,
                restore_backup,
                keep_tree,
            } => with_ctx(|c| uninstall(c, force, restore_backup, keep_tree)),
            Subcommand::Doctor { surface: None } => with_ctx(doctor),
            Subcommand::Doctor { surface: Some(name) } => surface_table(&name),
            Subcommand::StandaloneGone => {
                fail("`standalone` is now just `install`.");
                fail("install always writes the plugin tree from the copies compiled into");
                fail("this binary, on a bare machine and in a checkout alike, so there is");
                fail("only one verb: tabstatus install [--tree <dir>]");
                1
            }
            Subcommand::PrintEmbedded(which) => print_embedded(which.as_deref()),
            Subcommand::TmuxFormat => {
                for line in tmux::format_lines(&Config::from_env()) {
                    say(&line);
                }
                0
            }
            Subcommand::TmuxArm(tty) => match tty {
                Some(t) => {
                    // Reattachment is best effort; keep tmux's hook silent.
                    let _ = tmux::arm_tty(&t);
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
                    String::from_utf8_lossy(arg.as_encoded_bytes())
                ));
                1
            }
            Subcommand::BadUsage(why) => {
                fail(&why);
                1
            }
        }
    }
}

/// `install [--tree <dir>] [--force]`.
///
/// `--force` once meant "install anyway, I am about to build `bin/tabstatus`"; install
/// now SUPPLIES the binary it needs, so that meaning is gone for good. It now lifts
/// exactly one refusal: a settings.json on a filesystem that keeps no Windows ACL (a
/// WSL share), whose permissions a rewrite from Windows cannot keep - written anyway,
/// with a warning. `--restore-backup` and `--keep-tree` are uninstall's and refused.
fn install_options(rest: &[OsString]) -> Subcommand {
    let mut tree = None;
    let mut force = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if matches!(Flag::parse(arg), Some(Flag::Force)) {
            force = true;
            continue;
        }
        if matches!(Flag::parse(arg), Some(Flag::Tree)) {
            match it.next() {
                Some(d) if !d.is_empty() && !d.as_encoded_bytes().starts_with(b"-") => {
                    tree = Some(d.clone())
                }
                // A mistyped flag must not be silently taken as a directory name and
                // materialised into `./--force`.
                Some(d) => {
                    return Subcommand::BadUsage(format!(
                        "--tree takes the directory to write the plugin tree into, and \
                         {:?} is not one",
                        String::from_utf8_lossy(d.as_encoded_bytes())
                    ))
                }
                None => {
                    return Subcommand::BadUsage(
                        "--tree needs the directory to write the plugin tree into, \
                         e.g. tabstatus install --tree ~/.local/share/claude-tabstatus"
                            .to_string(),
                    )
                }
            }
            continue;
        }
        // Every other option, real or mistyped, is one install does not accept.
        if arg.as_encoded_bytes().starts_with(b"-") {
            return Subcommand::BadOption(arg.clone());
        }
        // A bare path. `standalone <dir>` took one; install spells it out, so muscle
        // memory cannot make it write a tree somewhere it was not asked to.
        return Subcommand::BadUsage(format!(
            "install takes no bare directory. To choose where the plugin tree goes: \
             tabstatus install --tree {}",
            String::from_utf8_lossy(arg.as_encoded_bytes())
        ));
    }
    Subcommand::Install { tree, force }
}

/// `doctor [--surface <name>]`.
///
/// Every other argument is still passed over rather than refused - a report is the
/// one command that must run on the configuration that is broken, and `doctor
/// --force` has always been a report - so this looks for the single option doctor
/// takes and ignores the rest.
fn doctor_options(rest: &[OsString]) -> Subcommand {
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if !matches!(Flag::parse(arg), Some(Flag::Surface)) {
            continue;
        }
        return match it.next() {
            Some(n) if !n.is_empty() && !n.as_encoded_bytes().starts_with(b"-") => {
                Subcommand::Doctor { surface: Some(n.clone()) }
            }
            _ => Subcommand::BadUsage(format!(
                "--surface needs the terminal to print the table for, one of: {}",
                surface_names()
            )),
        };
    }
    Subcommand::Doctor { surface: None }
}

fn uninstall_options(rest: &[OsString]) -> Subcommand {
    let mut force = false;
    let mut restore_backup = false;
    let mut keep_tree = false;
    for arg in rest {
        match Flag::parse(arg) {
            Some(Flag::Force) => force = true,
            Some(Flag::RestoreBackup) => restore_backup = true,
            Some(Flag::KeepTree) => keep_tree = true,
            // `--tree` and `--surface` are real flags, and neither is uninstall's.
            Some(Flag::Tree) | Some(Flag::Surface) | None => {
                return Subcommand::BadOption(arg.clone())
            }
        }
    }
    Subcommand::Uninstall {
        force,
        restore_backup,
        keep_tree,
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
        "  tabstatus install            write the plugin tree from the copies compiled\n",
        "                               into this binary, link it, and disable the\n",
        "                               built-in title. The plugin directory is BUILD\n",
        "                               OUTPUT, like bin/ - never your checkout\n",
        "      --tree <dir>             put it somewhere other than\n",
        "                               $XDG_DATA_HOME/claude-tabstatus\n",
        "      --force                  write settings.json even where its permissions\n",
        "                               cannot be kept (Windows, on a WSL share)\n",
        "  tabstatus uninstall          undo exactly that, tree included\n",
        "      --force                  remove the env key even with no state record, and\n",
        "                               write settings.json where its permissions cannot\n",
        "                               be kept (Windows, on a WSL share)\n",
        "      --restore-backup         roll settings.json back to the pre-install copy\n",
        "      --keep-tree              leave the generated plugin tree on disk\n",
        "  tabstatus doctor             report what is installed and what would paint,\n",
        "                               including the capability table of the three axes\n",
        "      --surface <name>         print that table for a terminal this machine is\n",
        "                               not running, and nothing else\n",
        "  tabstatus tmux-format        print the two outer-tab tmux format strings\n",
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
/// `version` and `doctor` because releases ship a binary per triple and bin/ holds
/// whichever ones you built, so "which one of those am I running" is a real question -
/// and the tree's marker records it, so `doctor` can spot a tree materialised by one
/// architecture and later run by another.
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
    } else if cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_arch = "aarch64", target_os = "windows", target_env = "msvc")) {
        "aarch64-pc-windows-msvc"
    } else {
        "unknown-target"
    }
}

// --- context ----------------------------------------------------------------

/// How the live plugin tree was found. Printed, not inferred: the checkout used to
/// be the answer to "where is the plugin", and now there are three possible sources,
/// so a report that does not say which one it used cannot be acted on.
#[derive(Clone, Copy, PartialEq)]
enum TreeFrom {
    /// `--tree <dir>`.
    Explicit,
    /// `<config>/skills/claude-tabstatus`, the symlink install itself wrote. The
    /// AUTHORITATIVE record of where the plugin directory is.
    Link,
    /// The state record's `tree` field, which is how a hand-repointed link still
    /// leaves `uninstall` able to find the tree it owns.
    Record,
    /// `$XDG_DATA_HOME/claude-tabstatus`, or `$HOME/.local/share/claude-tabstatus`.
    Default,
}

impl TreeFrom {
    fn why(self) -> String {
        match self {
            TreeFrom::Explicit => "from --tree".to_string(),
            TreeFrom::Link => format!("from the plugin {}", sys::DIR_LINK),
            TreeFrom::Record => "from the state record".to_string(),
            TreeFrom::Default => "the default path".to_string(),
        }
    }
}

struct Ctx {
    /// The plugin directory: generated, never a checkout.
    tree: PathBuf,
    tree_from: TreeFrom,
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

/// The config directory, resolved on its own because `install` needs it BEFORE there
/// is a tree to make a [`Ctx`] out of: it resolves its own tree, and one of the
/// refusals is a target under `<config>/skills`.
///
/// Stays OsString: a HOME or CLAUDE_CONFIG_DIR that is not valid UTF-8 still names
/// a real directory, and repairing it here would edit a different one.
fn config_dir() -> Result<PathBuf, String> {
    match config::var_nonempty("CLAUDE_CONFIG_DIR") {
        // Absolute, so that `<config>/skills` is comparable with the tree and every
        // path this half prints or records is the same path from any directory.
        Some(v) => absolute(&PathBuf::from(v)),
        None => match config::home_var() {
            Some(home) => absolute(&PathBuf::from(home).join(".claude")),
            None => Err("neither CLAUDE_CONFIG_DIR nor HOME is set, so there \
                         is no config directory to work on"
                .to_string()),
        },
    }
}

/// A path made absolute and lexically normalised, ONCE, before anything looks at it.
///
/// Everything downstream compares these paths, writes through them and RECORDS them,
/// and a raw `--tree` string defeated all three. `--tree skills/claude-tabstatus` run
/// from `<config>` walked straight past the refusal whose whole job is to keep the tree
/// out of `skills/`, because that test is a component-prefix test. Worse, the symlink
/// then got the relative string as its target, which resolves against the LINK's
/// directory rather than the shell's - a dangling link with the env key set, install
/// saying "Done.", and nothing painting. And the record kept the relative string, so a
/// later `uninstall` from a different directory removed files from whatever else
/// happened to be named that.
///
/// `fs::canonicalize` is the wrong tool: it resolves symlinks - which is what makes it
/// right for settings.json and wrong here - and it FAILS on a path that does not exist
/// yet, which the tree usually does not. So `..` and `.` are folded textually.
///
/// Before that, `sys::normalize` picks the ONE spelling this path is compared,
/// printed and recorded by. Nothing on Unix; on Windows, where `\\?\C:\x`,
/// `C:\X` and `C:\PROGRA~1`-style short names all name one directory, it is what keeps
/// a re-install from reading its own live tree as an orphan.
fn absolute(p: &Path) -> Result<PathBuf, String> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                format!(
                    "cannot read the current directory ({}), so the relative path {} \
                     cannot be made absolute. Pass an absolute one.",
                    e,
                    p.display()
                )
            })?
            .join(p)
    };
    let abs = sys::normalize(&abs);
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            // At the root `..` is the root, which is what `pop` returning false means.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    Ok(if out.as_os_str().is_empty() { PathBuf::from("/") } else { out })
}

impl Ctx {
    /// For `doctor` and `uninstall`, which have to work on whatever is there.
    fn new() -> Result<Ctx, String> {
        let config = config_dir()?;
        let (tree, from) = live_tree(&config)?;
        Ctx::at(tree, from)
    }

    /// Everything in `new` except finding the plugin directory, so `install` can work
    /// on the tree it RESOLVED rather than one it had to discover.
    fn at(tree: PathBuf, tree_from: TreeFrom) -> Result<Ctx, String> {
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
            tree,
            tree_from,
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
    /// `./bin/tabstatus install`, `cargo run` and a copy in a scratchpad are all
    /// legitimate ways to run `install`, and what must exist afterwards is the copy
    /// inside the tree.
    fn hook_binary(&self) -> PathBuf {
        tree::bin_path(&self.tree)
    }
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Where the plugin directory IS, for the two commands that have to work on whatever
/// is already there.
///
/// Walking up from the running executable is deliberately NOT one of the answers any
/// more, and that is the point of the whole change: `./bin/tabstatus doctor` in a
/// checkout would find `.claude-plugin/plugin.json` above itself and report that the
/// plugin is this checkout - which is exactly the wiring being abolished, restated by
/// the tool as fact. The binary you RUN and the directory Claude Code LOADS are two
/// different things now, and only the config directory knows the second one.
///
/// FIRST the symlink `install` wrote, which is the authoritative record and is taken
/// even when it dangles or points somewhere surprising: a dead link with a live env
/// key behind it is exactly the broken config `doctor` exists to explain and
/// `uninstall` has to clean up, and nothing downstream writes into it on that basis -
/// removal re-checks the marker, and a directory without one is never removed.
///
/// THEN the state record's own `tree` field, for the one shape the link cannot cover:
/// somebody repointed or removed the link by hand, and the tree this install owns
/// would otherwise be an orphan nothing can name.
///
/// THEN the default path, for a config that has neither.
fn live_tree(config: &Path) -> Result<(PathBuf, TreeFrom), String> {
    let link = config.join("skills").join(PLUGIN);
    if let Some(t) = link_target(&link) {
        return Ok((t, TreeFrom::Link));
    }
    if let Some(t) = recorded_tree(config) {
        return Ok((t, TreeFrom::Record));
    }
    Ok((tree::default_tree()?, TreeFrom::Default))
}

/// The tree the state record says this install owns, as install wrote it.
fn recorded_tree(config: &Path) -> Option<PathBuf> {
    let raw = fs::read(config.join(format!("{}.state", PLUGIN))).ok()?;
    let v = json::parse(&raw).ok()?;
    let t = v.as_obj()?.get("tree")?.val.as_str()?.to_vec();
    let p = PathBuf::from(sys::os_string_from_vec(t));
    // Absolute or nothing. `install` writes it absolute; a relative one could only
    // come from a hand-edited record, and resolving it against whatever directory
    // `uninstall` happens to be run from is how a tool removes files somewhere it was
    // never pointed at.
    (p.is_absolute() && p.as_os_str().len() > 1).then_some(p)
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

/// A checkout above `from`, for ONE remaining purpose: `doctor`'s `source:` line, and
/// `install`'s copy of it, which ask whether the manifests in the checkout this binary
/// came out of still match the copies compiled into it. It is no longer how anything
/// finds the plugin directory - see `live_tree`.
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
    sys::mode(&fs::metadata(p).ok()?)
}

/// `" (mode 0600)"` for a report line, with `tail` inside the parentheses - or
/// nothing where the platform has no modes (Windows), where printing the bits we
/// asked for would claim a protection nothing applied.
fn mode_note(mode: u32, tail: &str) -> String {
    if sys::HAS_MODES {
        format!(" (mode {:04o}{})", mode, tail)
    } else {
        String::new()
    }
}

/// Report the protection the backend carries, including Darwin's owner/group.
fn kept_note(mode: u32) -> String {
    if sys::HAS_MODES && sys::HAS_SECURITY {
        mode_note(mode, " and its owner, group and access control list kept")
    } else if sys::HAS_MODES {
        mode_note(mode, " kept")
    } else {
        " (its access control list kept)".to_string()
    }
}

/// Write through a temp file in the same directory and rename, with an explicit
/// mode: the settings.json we replace holds the user's `env` block, so a mode of
/// 0600 has to stay 0600. The shell installer widened it to 0644 through a
/// redirection, which is the bug this signature exists to make impossible.
///
/// On Windows there is no mode to apply: the temp file takes its directory's
/// inherited ACL and the rename carries that over the original. Right for what this
/// tool owns (the record, the tree); settings.json goes through [`write_settings`],
/// which keeps the ACL the file had.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    write_atomic_as(path, bytes, mode, None).map_err(|e| could_not_write(path, &e))
}

fn could_not_write(path: &Path, e: &std::io::Error) -> String {
    format!("could not write {}: {}", path.display(), e)
}

/// [`write_atomic`] for settings.json and its backups: the new file also keeps what
/// `like` carries beyond its mode - on Darwin its owner/group and ACL, on Windows
/// its DACL, so an ACL set on the file survives the rename (`sys::security_of`). `like` is the
/// file being replaced, or for a backup or a restore, the file being copied. Nothing
/// there: the new file is made exactly as `write_atomic` makes it. An ACL this user
/// cannot read refuses the write, before anything is written to `path` - the
/// preflights ask the same question first ([`refuse_unreadable_acl`]), so this is
/// reached only by a change made while the tool runs.
///
/// `allow_no_acl` is `--force`: a filesystem that keeps no Windows ACL at all is then
/// written as `write_atomic` writes, having been warned about in the preflight
/// ([`refuse_unreadable_acl`]). An ACL that exists but cannot be read is refused
/// whatever the flag says.
fn write_settings(path: &Path, bytes: &[u8], mode: u32, like: &Path, allow_no_acl: bool) -> Result<(), String> {
    let keep = match sys::security_of(like) {
        Ok(keep) => keep,
        Err(e) if allow_no_acl && no_acl_here(&e) => None,
        Err(e) => return Err(acl_unreadable(like, &e, &format!("{} was not changed", path.display()))),
    };
    write_atomic_as(path, bytes, mode, keep.as_ref()).map_err(|e| could_not_write(path, &e))
}

/// The one refusal `--force` lifts: a filesystem with no Windows ACL to carry.
fn no_acl_here(e: &std::io::Error) -> bool {
    sys::CAN_FORCE_ACL && e.kind() == std::io::ErrorKind::Unsupported
}

/// A copy with the SOURCE's protection, staged securely and replaced atomically.
/// Linux retains fs::copy; Darwin retains copyfile's non-ACL metadata work on the
/// secured fd; Windows retains its byte-copy policy. See sys::copy_secured.
fn copy_settings(from: &Path, to: &Path, what: &str, allow_no_acl: bool) -> Result<(), String> {
    let fail = |e: &std::io::Error| format!("cannot write {}{}: {}", what, to.display(), e);
    match sys::security_of(from) {
        Ok(None) => fs::copy(from, to).map(drop).map_err(|e| fail(&e)),
        Err(e) if allow_no_acl && no_acl_here(&e) => fs::copy(from, to).map(drop).map_err(|e| fail(&e)),
        Ok(Some(sec)) => {
            atomic_as(to, mode_of(from).unwrap_or(0o600), Some(&sec), |f| sys::copy_secured(from, f, &sec))
                .map_err(|e| fail(&e))
        }
        Err(e) => Err(acl_unreadable(from, &e, &format!("{} was not written", to.display()))),
    }
}

/// Why `p`'s protection cannot be carried to the file written in its place, ending
/// with `tail`. `Unsupported` is a filesystem with no Windows ACL at all (a WSL
/// share): there is nothing to read, and the permissions it does keep - Unix ones -
/// cannot be carried from here either.
fn acl_unreadable(p: &Path, e: &std::io::Error, tail: &str) -> String {
    if no_acl_here(e) {
        return format!(
            "{} is on a filesystem that keeps no Windows access control list ({}): a copy \
             or rewrite of it made from Windows would not keep its permissions (on a WSL \
             share, 0600 comes back 0644). Edit it from the system that owns that \
             filesystem, or re-run with --force to write it anyway. {}.",
            p.display(),
            e,
            tail
        );
    }
    if e.kind() == std::io::ErrorKind::Unsupported {
        return format!("cannot preserve the access control list of {} ({}). {}.", p.display(), e, tail);
    }
    format!(
        "cannot read the access control list of {} ({}), and the file written in its \
         place has to keep it. {}.",
        p.display(),
        e,
        tail
    )
}

/// Refuse, in a preflight, a settings file whose ACL this user cannot read: every
/// rewrite of it, and every backup, has to carry that ACL over, so finding out after
/// the first write would leave a half-done install. Darwin also probes ownership
/// preservation on an empty private file. Linux adds no syscall here.
///
/// With `allow_no_acl` (`--force`), a filesystem that keeps no Windows ACL is let
/// through with a warning instead, said here once so the writes that follow need
/// not repeat it.
fn refuse_unreadable_acl(p: &Path, allow_no_acl: bool) -> Result<(), String> {
    match sys::security_of(p) {
        Ok(None) => Ok(()),
        Ok(Some(sec)) => sec.preflight(p).map_err(|e| format!(
            "cannot preserve the protection of {} ({}). Nothing has been changed.",
            p.display(), e
        )),
        Err(e) if allow_no_acl && no_acl_here(&e) => {
            say(&format!("settings: WARNING {} is on a filesystem that keeps no Windows", p.display()));
            say("          access control list: its permissions will not be kept (on a WSL");
            say("          share, 0600 comes back 0644). Going ahead because of --force.");
            Ok(())
        }
        Err(e) => Err(acl_unreadable(p, &e, "Nothing has been changed")),
    }
}

fn write_atomic_as(path: &Path, bytes: &[u8], mode: u32, keep: Option<&sys::Security>) -> std::io::Result<()> {
    atomic_as(path, mode, keep, |f| f.write_all(bytes))
}

fn atomic_as<F>(path: &Path, mode: u32, keep: Option<&sys::Security>, write: F) -> std::io::Result<()>
where F: FnOnce(&mut fs::File) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.as_encoded_bytes().to_vec()).unwrap_or_default();
    let mut tmp_name = b".".to_vec();
    tmp_name.extend_from_slice(&name);
    tmp_name.extend_from_slice(format!(".cctab-tmp.{}", std::process::id()).as_bytes());
    let tmp = dir.join(sys::os_string_from_vec(tmp_name));
    let _ = fs::remove_file(&tmp);
    let res = (|| -> std::io::Result<()> {
        let mut f = match keep {
            Some(sec) => sys::create_secured(&tmp, sec)?,
            None => sys::with_mode(fs::OpenOptions::new().write(true).create_new(true), mode).open(&tmp)?,
        };
        if let Some(sec) = keep {
            // A secured file is born private; restore the intended mode before
            // configuration bytes, without permitting directory ACL inheritance.
            sys::set_mode(&tmp, mode)?;
            sys::verify_security(&f, sec, mode)?;
        }
        write(&mut f)?;
        // create_new honours `mode` only through the open(2) mode argument,
        // which umask narrows. Set it explicitly so a umask of 022 cannot
        // widen - or narrow - what we asked for.
        sys::set_mode(&tmp, mode)?;
        if let Some(sec) = keep {
            sys::verify_security(&f, sec, mode)?;
        }
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
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

/// The record `uninstall` reads back. Two halves, and they have opposite lifetimes.
///
/// "What was here BEFORE" is written once and never rewritten - a second install must
/// not record the state the first one left. "Which tree this install OWNS" is
/// rewritten every time, because `install --tree <somewhere else>` moves it and a
/// record naming the old one would leave the new tree unfindable and the old one an
/// orphan. Splitting them is what made this state_version 3.
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
    // The tree this install owns. The v2 field here was called "repo" and was never
    // read back by anything, which is why renaming it costs nothing - and it has to
    // be READ back now, because it is how `uninstall` finds the tree when the link no
    // longer points at it.
    out.push_str(&format!("  \"tree\": {},\n", json::quote(c.tree.as_os_str().as_encoded_bytes())));
    out.push_str(&format!(
        "  \"settings_path\": {},\n",
        json::quote(c.settings.as_os_str().as_encoded_bytes())
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
/// one up either. Both are per-run scratch, so a name whose pid no longer runs (no
/// /proc entry; on Windows, no such process or one that has exited) is litter by
/// definition. A LIVE pid is left alone: a concurrent install's temp file is the one
/// thing here that must not be removed under it.
fn sweep_litter(dir: &Path) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let raw = e.file_name();
        let Some(name) = raw.to_str() else { continue };
        let Some(pid) = scratch_pid(name) else { continue };
        // A digit string that is not a pid's canonical spelling (too large, or a
        // leading zero) names no process, exactly as it names no /proc entry. Unknown
        // liveness (a process this user may not ask about) counts as alive: keeping
        // litter is harmless, removing a concurrent install's temp file is not.
        let alive = pid
            .parse::<u32>()
            .ok()
            .filter(|p| p.to_string() == pid)
            .map_or(Some(false), sys::process_alive);
        if alive == Some(false) {
            // The swap's temp LINK is one of these, and on Windows a directory link is
            // not a file `remove_file` will take. On Unix both calls are unlink(2).
            let p = e.path();
            if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = sys::remove_dir_link(&p);
            } else {
                let _ = fs::remove_file(&p);
            }
        }
    }
}

/// The pid inside one of this tool's two scratch names, or `None` for a name that
/// is not ours to remove.
///
/// `.cctab-wtest.<pid>` and `.<file>.cctab-tmp.<pid>` are the only two shapes, and
/// the pid must be all digits: nothing else in either directory ends in
/// `.<digits>` after one of those markers.
pub(crate) fn scratch_pid(name: &str) -> Option<&str> {
    let pid = name
        .strip_prefix(".cctab-wtest.")
        .or_else(|| name.rsplit_once(".cctab-tmp.").map(|(_, p)| p))?;
    let numeric = !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit());
    numeric.then_some(pid)
}

/// Every directory this tool writes scratch files into.
///
/// `c.skills` is here because the atomic symlink swap's temp file lives in it, and
/// that is the directory Claude Code ENUMERATES for plugins - litter there is not
/// merely untidy. The tree and its parent are here because the writability probe and
/// the binary's temp copy live in them, and both are new places for this tool to leave
/// anything. Called after the preflight has passed, so the tree is one we own by then.
fn sweep_all(c: &Ctx) {
    sweep_litter(&c.config);
    let dir = c.settings.parent().unwrap_or(Path::new("."));
    if dir != c.config {
        sweep_litter(dir);
    }
    if c.skills != c.config {
        sweep_litter(&c.skills);
    }
    // The tree, its parent and its bin/: the writability probe lands in whichever of
    // the first two existed at preflight time, and the binary's temp copy in the third.
    // All three are swept whatever exists now, so a probe left in the PARENT by a run
    // that found no tree is still cleaned up by the next run, which finds one.
    sweep_litter(&c.tree);
    if let Some(d) = c.tree.parent() {
        sweep_litter(d);
    }
    sweep_litter(&c.tree.join("bin"));
    // What a non-atomic replace set aside and could not delete at the time - on
    // Windows a binary a hook was still running, or the old junction. Nothing on
    // Unix, where every replace is one rename. Named, because it is something an
    // earlier run left on this disk and this one removed.
    for d in [&c.config, &c.skills, &c.tree.join("bin")] {
        for p in sys::sweep_replaced(d) {
            say(&format!("swept:    {} (set aside by an earlier run)", p.display()));
        }
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

/// Where THIS install writes the plugin tree.
///
/// `--tree` wins. Otherwise the tree the link already points at, but only when that
/// target is OURS or is NOT THERE AT ALL - and those two cases are one rule, not two:
/// a marked directory is a tree we wrote and an absent path cannot be anybody's.
///
/// Reusing an unmarked, EXISTING target is the case that must not happen. It would send
/// `refuse_target` at somebody else's directory and dead-end the whole install with
/// nowhere to go, where falling through to the default path and repointing - loudly -
/// always has a way forward. That one rule is what keeps every awkward link state
/// recoverable, and it is also what makes the migration work: the link currently points
/// at the CHECKOUT, which exists and has no marker, so install picks the default path
/// and repoints instead of trying to write into the source tree.
///
/// The absent case is the heal. A tree deleted by hand leaves a live link, and
/// rewriting the tree where that link already points fixes the install without moving
/// it - `install --tree <somewhere>` once, deleted by accident, must not silently come
/// back at the default path with the chosen location abandoned.
fn resolve_tree(dir: Option<OsString>, config: &Path) -> Result<(PathBuf, TreeFrom), String> {
    if let Some(d) = dir {
        return Ok((absolute(&PathBuf::from(d))?, TreeFrom::Explicit));
    }
    let link = config.join("skills").join(PLUGIN);
    if let Some(t) = link_target(&link) {
        if tree::is_generated(&t) || !t.exists() {
            return Ok((t, TreeFrom::Link));
        }
    }
    Ok((tree::default_tree()?, TreeFrom::Default))
}

/// Four writes, in the order the module header explains, with every refusal decided
/// before the first of them.
///
/// It resolves its own tree rather than being handed one by `with_ctx`, because
/// install is the command that DECIDES where the plugin directory is. Everything else
/// discovers it.
fn install(dir: Option<OsString>, force: bool) -> Result<(), String> {
    let config = config_dir()?;
    let (tree, from) = resolve_tree(dir, &config)?;
    let c = Ctx::at(tree, from)?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot find my own path ({}), so there is nothing to copy", e))?;

    let (existing, prior) = install_preflight(&c, force)?;
    install_header(&c, &exe);
    // Clear this tool's own scratch files left by a killed run, in the directories it
    // is about to write.
    sweep_all(&c);
    fs::create_dir_all(&c.config)
        .map_err(|e| format!("cannot create {}: {}", c.config.display(), e))?;

    // What uninstall would put the link back to, decided from the record BEFORE
    // `record_state` consumes it: the promise `link_the_plugin` prints about a target
    // it is replacing has to be about the record that will actually be read.
    let promise = RecordedPrior::of(prior.as_ref());
    let lines = tree::materialise(&c.tree, &exe, env!("CARGO_PKG_VERSION"), target_triple())
        .map_err(|e| early_failure(&c, &e))?;
    for line in lines {
        say(&line);
    }
    record_state(&c, prior, existing.as_deref())?;
    write_env_key(&c, existing, force)?;
    link_the_plugin(&c, &promise)?;

    say("");
    say("Done. Nothing else on this machine was modified.");
    say("Start a NEW Claude Code session for the plugin and the env key to take effect.");
    say(&format!("Live sessions paint through {}.", c.tree.display()));
    if let Some(co) = plugin_dir_above(&exe) {
        if !tree::is_generated(&co) {
            say(&format!(
                "The checkout at {} is wired to nothing: its hooks.json and plugin.json",
                co.display()
            ));
            say("are source, and `tabstatus install` is what deploys them.");
        }
    }
    say("To undo: tabstatus uninstall");
    Ok(())
}

/// Every reason to refuse, and the settings document to edit if there is none.
///
/// ONE preflight, in front of the FIRST write, which is what lets every refusal here
/// still end "Nothing has been changed." honestly. The old second verb wrapped these
/// refusals AFTER it had written a tree and had to strip that sentence back off them.
///
/// The settings document is `None` when settings.json is to be written fresh: either
/// it does not exist, or it exists and holds nothing but whitespace, which Claude Code
/// reads as no settings at all. The state record comes back too, because reading it can
/// FAIL - a record that will not parse - and that refusal belongs in front of the first
/// write like every other one, not between the tree and settings.json.
#[allow(clippy::type_complexity)]
fn install_preflight(c: &Ctx, force: bool) -> Result<(Option<Vec<u8>>, Option<State>), String> {
    if let Some(e) = refuse_unresolved(c) {
        return Err(e);
    }
    // The tree: is it ours to write, and can we write it? Writability is checked HERE
    // because the alternative is failing mid-materialise, possibly after the marker.
    if let Some(why) = tree::refuse_target(&c.tree, &c.skills) {
        return Err(why);
    }
    if let Some(d) = nearest_existing(&c.tree) {
        if !d.is_dir() {
            return Err(format!(
                "{} is not a directory, and the plugin tree goes under it. Nothing has \
                 been changed.",
                d.display()
            ));
        }
        if !dir_writable(&d) {
            return Err(format!(
                "{} is not writable, and the plugin tree goes {}. Nothing has been \
                 changed.",
                d.display(),
                if d == c.tree { "in it" } else { "under it" }
            ));
        }
    }
    match link_state(&c.link) {
        LinkState::Dir => return Err(refuse_link(&c.link, &format!("a real directory, not a {}", sys::DIR_LINK))),
        LinkState::Other => return Err(refuse_link(&c.link, &format!("not a {}", sys::DIR_LINK))),
        _ => {}
    }
    // `<config>/skills` itself, which the last step has to create or write into.
    // Checked HERE because that step runs after settings.json has already been
    // edited: a refusal there would leave the env key set with no plugin to honour
    // it, which is the one half-state this ordering exists to prevent.
    if c.skills.exists() {
        if !c.skills.is_dir() {
            return Err(refuse_link(&c.skills, "not a directory"));
        }
        if !dir_writable(&c.skills) {
            return Err(format!(
                "{} is not writable, and the plugin {} goes in it. Nothing \
                 has been changed.",
                c.skills.display(),
                sys::DIR_LINK
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
    // The link itself, for the same reason as `skills` above: it is the LAST write, so
    // a refusal there comes after settings.json. Writability settles it on Unix and the
    // seam touches nothing; on Windows a junction is made and removed where the real
    // one will go, because that is the only way to ask - it is how an install that
    // could not make its link used to fail with the env key already set.
    if let Some(d) = nearest_existing(&c.skills).filter(|d| d.is_dir()) {
        if let Err(e) = sys::probe_dir_link(&c.tree, &d) {
            return Err(format!(
                "cannot make a {} in {} to {}: {}. The plugin {} goes {}, and it is the \
                 last thing install writes. Nothing has been changed.",
                sys::DIR_LINK,
                d.display(),
                c.tree.display(),
                e,
                sys::DIR_LINK,
                if d == c.skills { "there" } else { "under it" }
            ));
        }
    }
    let existing = if c.settings.exists() {
        let doc = read_file(&c.settings)?;
        refuse_unreadable_acl(&c.settings, force)?;
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
    Ok((existing, read_state(c)?))
}

/// The deepest ancestor of `p` - `p` itself included - that exists, so the
/// writability probe asks about a directory that is actually there. `None` only if
/// nothing up to the root does, which on a Unix filesystem does not happen.
fn nearest_existing(p: &Path) -> Option<PathBuf> {
    let mut d = Some(p.to_path_buf());
    while let Some(cur) = d {
        if cur.exists() {
            return Some(cur);
        }
        d = cur.parent().map(|x| x.to_path_buf());
    }
    None
}

/// What is about to be touched, before anything is - including, LOUDLY, a link that
/// currently points somewhere else.
fn install_header(c: &Ctx, exe: &Path) {
    say(&format!("tree:     {} ({})", c.tree.display(), c.tree_from.why()));
    say(&format!("config:   {}", c.config.display()));
    say(&format!("binary:   {} (copied into the tree)", exe.display()));
    if let Some(l) = &c.settings_link {
        say(&format!("settings: following the symlink {} -> {}", l.display(), c.settings.display()));
    }
    // The checkout above the running binary is the one place the two copies of a
    // manifest can disagree, and this is the write path that turns the embedded bytes
    // into LIVE WIRING. `doctor` WARNs about exactly this drift; staying silent here -
    // at the moment a stale build's copy becomes the hooks a session runs - would
    // leave the guard's last doorway open.
    if let Some(d) = plugin_dir_above(exe) {
        if !tree::is_generated(&d) {
            say(&format!("source:   {} (the checkout this binary came out of)", d.display()));
            warn_checkout_drift(&d);
        }
    }
    repoint_warning(c);
    say("");
}

/// The migration, said out loud BEFORE anything is written.
///
/// The live wiring on this machine right now is a link to a CHECKOUT. Repointing it is
/// the whole point of the change, and a silent repoint of live wiring is the wrong
/// behaviour whatever the change is for: the operator has to be able to see, before
/// the first write, that a directory they have been editing stops being the plugin.
fn repoint_warning(c: &Ctx) {
    let LinkState::Link(t, _) = link_state(&c.link) else { return };
    if sys::same_path(&t, &c.tree) {
        return;
    }
    say(&format!("plugin:   {}", c.link.display()));
    say(&format!(
        "          now  -> {}{}",
        t.display(),
        if is_checkout(&t) { " (a checkout, not a generated tree)" } else { "" }
    ));
    say(&format!("          will -> {}", c.tree.display()));
    if is_checkout(&t) {
        say("          That checkout stops being the live plugin. Its hooks.json and");
        say("          plugin.json are SOURCE from now on; `tabstatus install` is what");
        say("          deploys them. Nothing in it is modified.");
    }
}

/// A plugin directory that is not one of ours: it holds a `plugin.json`, so Claude
/// Code would load it, but carries no marker. Before this change that WAS the install,
/// which is why it is named rather than lumped in with "somewhere else".
fn is_checkout(p: &Path) -> bool {
    p.join(embedded::PLUGIN_JSON_PATH).is_file() && !tree::is_generated(p)
}

/// The state record: the write-once half, and the half every install rewrites.
///
/// A v2 record is upgraded IN PLACE rather than replaced, because its first half -
/// what settings.json and the symlink looked like before any of this - is unrecoverable
/// once it is lost, and it is the only thing `uninstall` has to go on.
fn record_state(c: &Ctx, prior: Option<State>, existing: Option<&[u8]>) -> Result<(), String> {
    match prior {
        Some(prior) => {
            // The prior half, re-emitted verbatim, with this install's tree beside it.
            let text = state_text(c, &prior);
            let same = fs::read(&c.state).map(|b| b == text).unwrap_or(false);
            if same {
                say(&format!("state:    {} exists - keeping the original record", c.state.display()));
            } else {
                let was = state_version_on_disk(c);
                write_atomic(&c.state, &text, 0o600)?;
                match was {
                    Some(v) if v != STATE_VERSION.to_string().as_bytes() => say(&format!(
                        "state:    upgraded the record to state_version {} to note the \
                         generated tree",
                        STATE_VERSION
                    )),
                    _ => say(&format!(
                        "state:    kept the prior state in {} and noted the tree it owns",
                        c.state.display()
                    )),
                }
            }
        }
        None => {
            let (link_had, link_target) = match link_state(&c.link) {
                LinkState::Link(t, _) => (true, Some(t.as_os_str().as_encoded_bytes().to_vec())),
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
    }
    Ok(())
}

/// Which version the record on disk claims, as its source text, so an upgrade can say
/// it is one. Numbers are kept verbatim by this parser, so comparing the text is
/// exactly comparing the value and needs no integer accessor.
fn state_version_on_disk(c: &Ctx) -> Option<Vec<u8>> {
    let raw = fs::read(&c.state).ok()?;
    match json::parse(&raw).ok()?.as_obj()?.get("state_version")?.val {
        json::J::Num(ref n) => Some(n.clone()),
        _ => None,
    }
}

/// The one key, spliced into the document we parsed in the preflight.
fn write_env_key(c: &Ctx, existing: Option<Vec<u8>>, force: bool) -> Result<(), String> {
    match existing {
        None => {
            // 0600 from birth: this file is where people keep API keys. A blank
            // file that already existed keeps the mode it had.
            let mode = mode_of(&c.settings).unwrap_or(0o600);
            let text = format!("{{\n  \"env\": {{\n    {}: \"1\"\n  }}\n}}\n", json::quote(KEY.as_bytes()));
            write_settings(&c.settings, text.as_bytes(), mode, &c.settings, force)?;
            say(&format!("settings: created {}{}", c.settings.display(), mode_note(mode, "")));
            say(&format!("          env.{} = \"1\"", KEY));
        }
        Some(doc) => match settings::set_env_key(&doc, KEY, "1")? {
            Outcome::Unchanged => {
                say(&format!("settings: env.{} is already \"1\" - unchanged", KEY));
                say("          (no backup was written, because nothing was changed)");
            }
            Outcome::Changed { text, before } => {
                let mode = mode_of(&c.settings).unwrap_or(0o600);
                copy_settings(&c.settings, &c.backup, "the backup ", force)?;
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
                write_settings(&c.settings, &text, mode, &c.settings, force)?;
                say(&format!("settings: set env.{} = \"1\"{}", KEY, kept_note(mode)));
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

/// The LAST step, and the only irreversible one: it is the one write that changes what
/// code a running session executes, so an abort at any earlier point is a complete
/// no-op for a first-time user - the tree is inert until this link points at it, and
/// settings.json is recoverable from its backup and the record.
///
/// It comes after `verify` inside the tree write, because that is what proves the new
/// target works, and after settings.json, because "env key set + plugin gone" is the
/// one combination that paints no title at all.
///
/// And it is ATOMIC. `remove_file` then `symlink` leaves a window where the path does
/// not resolve at all, and a hook firing in that window execs a missing file: 127 per
/// event, and a non-zero PreToolUse hook BLOCKS A TOOL. `rename(2)` over a symlink
/// means the name resolves to the old target or the new one and never to nothing, so
/// the transition itself has no window.
/// What `uninstall` would do with the plugin link, which is NOT "put back whatever
/// this install replaced".
///
/// The record's first half is write-once on purpose - a second install must not record
/// the state the first one left - so from the second install onwards the target being
/// replaced here and the target uninstall reads are different paths. The old wording
/// promised "uninstall puts the old target back" in exactly the case it is printed for:
/// live wiring being repointed.
enum RecordedPrior {
    /// THIS install writes the record, so the target it replaces IS the one uninstall
    /// will put back.
    ThisRun,
    /// A record already existed. Its write-once half names this, and nothing here can
    /// change it.
    Earlier(Option<PathBuf>),
}

impl RecordedPrior {
    fn of(prior: Option<&State>) -> RecordedPrior {
        match prior {
            None => RecordedPrior::ThisRun,
            Some(s) => RecordedPrior::Earlier(
                s.link_target
                    .clone()
                    .filter(|_| s.link_had)
                    .filter(|b| !b.is_empty())
                    .map(|b| PathBuf::from(sys::os_string_from_vec(b))),
            ),
        }
    }
}

fn link_the_plugin(c: &Ctx, promise: &RecordedPrior) -> Result<(), String> {
    fs::create_dir_all(&c.skills).map_err(|e| late_failure(c, &format!("cannot create {}: {}", c.skills.display(), e)))?;
    match link_state(&c.link) {
        LinkState::Absent => {
            symlink(&c.tree, &c.link).map_err(|e| late_failure(c, &e))?;
            say(&format!("{}created", link_label()));
            say(&format!("          {} -> {}", c.link.display(), c.tree.display()));
        }
        LinkState::Link(t, resolves) if sys::same_path(&t, &c.tree) => {
            if resolves {
                say(&format!("{}already correct", link_label()));
                say(&format!("          {} -> {}", c.link.display(), c.tree.display()));
            } else {
                swap_symlink(&c.tree, &c.link).map_err(|e| late_failure(c, &e))?;
                say(&format!("{}points here but does not resolve - recreated", link_label()));
                say(&format!("          {} -> {}", c.link.display(), c.tree.display()));
            }
        }
        LinkState::Link(t, resolves) => {
            let was_checkout = is_checkout(&t);
            // A fourth case, and the one the old wording got wrong: the previous target
            // is a tree WE generated. `install --tree <somewhere else>` is the only way
            // to reach it, and calling that "a symlink this installer did not create"
            // was simply false.
            let was_ours = tree::is_generated(&t);
            swap_symlink(&c.tree, &c.link).map_err(|e| late_failure(c, &e))?;
            say(&if was_checkout {
                format!("{}REPOINTED from the checkout to the generated tree", link_label())
            } else if was_ours {
                format!("{}MOVED the plugin to a different generated tree", link_label())
            } else if resolves {
                format!(
                    "{}WARNING - repointed a {} this installer did not create",
                    link_label(),
                    sys::DIR_LINK
                )
            } else {
                format!(
                    "{}WARNING - replaced a BROKEN {} this installer did not create",
                    link_label(),
                    sys::DIR_LINK
                )
            });
            say(&format!("          {}", c.link.display()));
            say(&format!("          was  {}", t.display()));
            say(&format!("          now  {}", c.tree.display()));
            if was_checkout {
                say("          that checkout is source now, and is not modified.");
            } else if was_ours {
                for line in moved_from_lines(&t) {
                    say(&line);
                }
            } else {
                match promise {
                    RecordedPrior::ThisRun => say("          uninstall puts the old target back."),
                    // is_checkout first: uninstall declines to restore a checkout at
                    // all, so naming the path without that would be a second wrong
                    // promise in the same line.
                    RecordedPrior::Earlier(Some(p)) if is_checkout(p) => {
                        say("          uninstall will NOT put this target back: the record is");
                        say(&format!(
                            "          write-once and names the checkout {}, which is no",
                            p.display()
                        ));
                        say("          longer a plugin directory, so it removes the link instead.");
                    }
                    RecordedPrior::Earlier(Some(p)) => {
                        say("          uninstall will NOT put this target back: the record is");
                        say(&format!(
                            "          write-once, so it restores {} - what the",
                            p.display()
                        ));
                        say("          FIRST install found here.");
                    }
                    RecordedPrior::Earlier(None) => {
                        say("          uninstall will NOT put this target back: the record is");
                        say("          write-once and says there was no link here before, so it");
                        say("          removes the link instead.");
                    }
                }
            }
        }
        LinkState::Dir => return Err(refuse_link(&c.link, &format!("a real directory, not a {}", sys::DIR_LINK))),
        LinkState::Other => return Err(refuse_link(&c.link, &format!("not a {}", sys::DIR_LINK))),
    }
    Ok(())
}

/// Repoint a symlink with no window in which it resolves to nothing: a new link at a
/// pid-named temp name in the same directory, then `rename(2)` onto the live name.
///
/// The single most important call in this module. Everything else here can be retried;
/// a hook that execs a missing `bin/tabstatus` cannot be un-run.
///
/// On Windows the link is a junction, and the last step is still one rename on NTFS;
/// `sys::replace_dir_link` documents the rename-aside fallback for a filesystem that
/// refuses it, and the window that fallback cannot close.
fn swap_symlink(target: &Path, link: &Path) -> Result<(), String> {
    let tmp = tree::scratch_beside(link);
    let _ = sys::remove_dir_link(&tmp);
    sys::link_dir(target, &tmp)
        .map_err(|e| format!("cannot create the {} {}: {}", sys::DIR_LINK, tmp.display(), e))?;
    sys::replace_dir_link(&tmp, link).map_err(|e| {
        let _ = sys::remove_dir_link(&tmp);
        format!("cannot move the {} into place at {}: {}", sys::DIR_LINK, link.display(), e)
    })
}

fn symlink(target: &Path, link: &Path) -> Result<(), String> {
    sys::link_dir(target, link)
        .map_err(|e| format!("cannot create the {} {}: {}", sys::DIR_LINK, link.display(), e))
}

/// The label the plugin link's report lines start with: `symlink:  ` on Unix and
/// `junction: ` on Windows - ten columns either way, so the lines under it align.
fn link_label() -> String {
    format!("{:<10}", format!("{}:", sys::DIR_LINK))
}

/// A failure inside `materialise`, which is the FIRST write - and it lands directly
/// after a header that may have just announced, in three loud lines, that the live
/// plugin link "will ->" the new tree. The bare OS error alone leaves the one question
/// that matters unanswered: did the link move?
///
/// It did not. Nothing outside the tree is touched before this point, so this says so
/// by name rather than leaving it to be inferred.
fn early_failure(c: &Ctx, what: &str) -> String {
    format!(
        "{}\n\
         \n\
         The plugin {} {} was NOT touched, and neither were settings.json or the \
         state record at {} - so live sessions still paint through whatever the header \
         above printed as `now`, and nothing has become live wiring.\n\
         Part of {} may have been written. It carries {}, so a re-run resumes into it \
         rather than refusing.",
        what,
        sys::DIR_LINK,
        c.link.display(),
        c.state.display(),
        c.tree.display(),
        tree::MARKER
    )
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

// --- the checkout above the running binary ----------------------------------

/// The copies compiled in, against the checkout the running binary came out of.
///
/// Only ever a WARN: the bytes that are about to land in the tree are the embedded
/// ones either way, and which of the two the operator MEANT is not ours to decide. A
/// mid-edit working tree is a normal state, so this must never fail an install.
///
/// It is the one thing the deleted `Mode::Checkout` report was ever right about,
/// moved to where it is actionable: an edited `hooks.json` that has not been rebuilt
/// means THIS binary would deploy the OLD copy.
fn warn_checkout_drift(repo: &Path) {
    for (rel, text) in embedded::MANIFESTS {
        if let Verdict::Differs { on_disk, embedded: emb } = embedded::compare(repo, rel, text) {
            say(&format!(
                "          WARN {} there differs from the copy compiled in ({})",
                rel,
                embedded::differs_phrase(on_disk, emb)
            ));
            say(
                "          the tree carries the COMPILED-IN copy; rebuild with \
                 `sh scripts/build.sh` first if you meant to deploy that edit",
            );
        }
    }
}

/// The embedded bytes, verbatim, so `tests/run.sh` can diff them against the files
/// and `doctor` has something to point at on a machine with no source tree.
fn print_embedded(which: Option<&OsStr>) -> i32 {
    let Some(name) = which else {
        fail(&format!("print-embedded needs a name: {}", embedded::NAMES));
        return 1;
    };
    match embedded::by_name(name.as_encoded_bytes()) {
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
                String::from_utf8_lossy(name.as_encoded_bytes()),
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

/// The mirror of install: settings first, the link next, the tree last. See the module
/// header for why that order is the sharper of the two.
fn uninstall(c: &Ctx, force: bool, restore_backup: bool, keep_tree: bool) -> Result<(), String> {
    let prior = match uninstall_preflight(c, force, restore_backup, keep_tree)? {
        Preflight::Go(prior) => prior,
        Preflight::Refused => return Ok(()),
    };
    uninstall_header(c, &prior);
    sweep_all(c);
    // Asked NOW, while the state record still exists: `remove_state` below deletes the
    // record that names one of the two places an orphan can be found, and asking after
    // it found only the default path.
    let orphans = orphan_trees(c);

    remove_env_key(c, &prior, force, restore_backup)?;
    unlink_the_plugin(c, &prior)?;
    remove_state(c)?;
    remove_records();
    // The tmux title pair and window formats, which SessionStart saved aside. Only
    // uninstall restores it: the options are server-wide, so a SessionEnd doing it
    // would unpaint the other claude windows still running.
    for line in tmux::uninstall() {
        say(&line);
    }

    // LAST, because it holds the binary this process is running. Unlinking a
    // running executable is fine on Linux; unlinking it before the writes above
    // would leave the hooks pointing at nothing if one of them failed.
    remove_tree(c, keep_tree, &orphans)?;

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
    Ok(())
}

/// The plugin tree, removed BY DEFAULT now - nothing else owns it, so leaving it
/// behind leaves a whole plugin directory in `~/.local/share` that nothing will ever
/// mention again.
///
/// Conservative, though, and the two rules are what make removing it by default
/// defensible. Only a directory carrying our marker is touched at all: a link that
/// still points at a CHECKOUT - which is the pre-change wiring, and the state an
/// uninstall run before the migration sees - is named and left alone, because that is
/// somebody's source. And within a tree we do own, only the files the marker lists,
/// plus the directories those leave empty. Anything else is NAMED and left; there is
/// no `remove_dir_all` here on a path derived from a symlink.
fn remove_tree(c: &Ctx, keep: bool, orphans: &[PathBuf]) -> Result<(), String> {
    if !tree::is_generated(&c.tree) {
        if c.tree.is_dir() {
            say(&format!(
                "tree:     {} carries no {}, so it was not written by `tabstatus \
                 install` and is left alone",
                c.tree.display(),
                tree::MARKER
            ));
            if is_checkout(&c.tree) {
                say("          (it is a checkout - the wiring from before the plugin tree \
                     existed)");
            }
        }
        return Ok(());
    }
    if keep {
        for line in kept_tree_lines(&c.tree) {
            say(&line);
        }
        return Ok(());
    }
    // `orphans` was collected BEFORE the removal, by the caller: once the live tree is
    // gone it is itself a candidate, and naming the directory we just emptied as an
    // orphan would be a lie.
    let r = tree::remove(&c.tree)?;
    for line in removal_report(&c.tree, &r) {
        say(&line);
    }
    for line in orphans.iter().flat_map(|o| orphan_lines(o)) {
        say(&line);
    }
    Ok(())
}

/// What `remove_tree` says about one removal, as lines, so the rule below is testable
/// without a config directory.
///
/// The rule: the delete-the-directory command is offered only for a tree holding
/// nothing this report had to name as LEFT - neither a file the tool did not write nor
/// a marker-listed path it refused to follow. That command is the line a hurried
/// operator copies, and on Windows a removal that FAILED is the common case (a running
/// hook holds the binary), so it must not land on top of somebody's notes. When there
/// is something of theirs in there, the files that would not go are named instead.
///
/// `failed` is empty on every path that removed what it meant to, so everything here
/// reads exactly as it did before it existed unless a removal failed.
fn removal_report(tree: &Path, r: &tree::Removal) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if r.dir_gone {
        out.push(format!(
            "tree:     removed the plugin tree {} ({} generated file{})",
            tree.display(),
            r.removed,
            if r.removed == 1 { "" } else { "s" }
        ));
    } else {
        out.push(format!(
            "tree:     removed {} generated file{} from {}{}",
            r.removed,
            if r.removed == 1 { "" } else { "s" },
            tree.display(),
            describe_extra(&r.left)
        ));
        // The brief's rule is that uninstall names anything it leaves behind, and a
        // directory is a thing it leaves behind. This arm IS the survival arm, so it
        // must never be the one that says "empty": there is a directory there, and the
        // honest instruction for somebody who wants it gone is `rm -rf`.
        //
        // `blocked` is not checked here, deliberately: with nothing LEFT and nothing
        // FAILED, every path the tree still holds is a real directory. A link inside
        // it is a non-directory entry the removal names as left unless the marker
        // lists it, and a listed one was removed, so a refused path (`../outside`, a
        // component that was a link) points at nothing in here for the command to
        // follow.
        //
        // Nor is an empty `left` proof of anything when the walk behind it did not see
        // the whole tree - it stopped at its bound, or could not read a directory - so
        // then there is no command at all.
        if r.left.is_empty() && r.failed.is_empty() {
            match r.unread {
                None => {
                    out.push("          the directory itself survived - it holds no file this report can".into());
                    out.push(format!(
                        "          name, only empty directories. Remove it with `{}`",
                        sys::remove_dir_command(tree)
                    ));
                }
                Some(u) => {
                    out.push(format!("          the directory itself survived. {}, so nothing here", u.why()));
                    out.push("          can say what is left in it - do NOT delete the directory without".into());
                    out.push("          looking at what it holds.".into());
                }
            }
        }
    }
    // Paths the marker listed that were NOT taken, because they could not be proved to
    // be inside the tree. Loud: a marker is trivially forgeable by anything that can
    // write the tree, so this is where a tampered one shows up.
    for why in &r.blocked {
        out.push(format!("tree:     LEFT a file the marker listed - {}", why));
    }
    // Files of ours that would not go - on Windows, the binary a running hook holds.
    // The marker was kept for them, which is what keeps the tree re-enterable.
    for (p, e) in &r.failed {
        out.push(format!("tree:     could NOT remove {} ({})", p.strip_prefix(tree).unwrap_or(p).display(), e));
    }
    if r.failed.is_empty() {
        return out;
    }
    out.push(format!(
        "          {} still carries its {}, so it is still a tree `tabstatus install`",
        tree.display(),
        tree::MARKER
    ));
    if r.left.is_empty() && r.blocked.is_empty() && r.unread.is_none() {
        out.push(if sys::HAS_UNLINK_RUNNING {
            "          wrote and will reuse. Nothing else is in it, so to finish, remove".into()
        } else {
            "          wrote and will reuse. Once nothing is running what is left, remove".into()
        });
        out.push(format!("          it with `{}`", sys::remove_dir_command(tree)));
        return out;
    }
    // Something of somebody else's is in there: the files, never the directory. The
    // marker goes LAST, because a tree with one of ours and no marker is one `install`
    // refuses forever.
    out.push(if !r.left.is_empty() {
        "          wrote and will reuse. It also holds files nothing here generated, so".into()
    } else if !r.blocked.is_empty() {
        "          wrote and will reuse. Its marker lists paths refused above, so".into()
    } else {
        format!("          wrote and will reuse. {}, so", r.unread.map_or("", tree::Unread::why))
    });
    out.extend(delete_only_these(
        tree,
        r.failed.iter().map(|(p, _)| {
            // A marker-listed path that is a directory fails `remove_file` too, and
            // the removal's walk goes into it, so anything inside it is somebody's and is
            // named above as left. "Delete only these" must not take that with it.
            let holds_something = fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
                && fs::read_dir(p).map_or(true, |mut rd| rd.next().is_some());
            if holds_something {
                format!("{} (a directory - only once it is empty)", p.display())
            } else {
                p.display().to_string()
            }
        }),
    ));
    out
}

/// The instruction for a tree of ours that holds something else too: not the
/// directory, the files - the marker LAST, because a tree with one of ours and no
/// marker is one `install` refuses forever. It follows a line ending in "so" that
/// said why.
fn delete_only_these(tree: &Path, files: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = if sys::HAS_UNLINK_RUNNING {
        vec!["          do NOT delete the directory - delete only these, the marker last:".into()]
    } else {
        vec![
            "          do NOT delete the directory - once nothing is running from the tree,".into(),
            "          delete only these, the marker last:".into(),
        ]
    };
    out.extend(files.map(|f| format!("            {}", f)));
    out.push(format!("            {}", tree::marker_path(tree).display()));
    out
}

/// What to do about a generated tree this run LEAVES in place - the one an
/// `install --tree` moved away from, a `--keep-tree`, an orphan - as lines under the
/// one that introduced it; `None` is the go-ahead for the caller to offer the
/// delete-the-directory command in its own words.
///
/// The same rule as `removal_report`: that command is the line a hurried operator
/// copies, so it is offered only for a tree holding nothing but what its marker lists
/// (`tree::survey`). Anything else in there - a file tabstatus did not write, a link, a
/// marker entry not provably inside, or no readable marker at all - and the directory
/// is NOT to be deleted; our files are named instead.
fn keep_the_directory(tree: &Path) -> Option<Vec<String>> {
    let s = match tree::survey(tree) {
        Ok(s) if s.only_ours() => return None,
        Ok(s) => s,
        Err(why) => {
            return Some(vec![
                format!("          {}, so nothing here can tell which of its files tabstatus", why),
                "          wrote - do NOT delete the directory without looking at what it holds.".into(),
            ])
        }
    };
    let mut out: Vec<String> =
        s.blocked.iter().map(|why| format!("          its marker lists a path this does not follow - {}", why)).collect();
    out.push(if !s.foreign.is_empty() {
        format!("          It also holds {}, so", foreign_files(&s.foreign))
    } else if !s.blocked.is_empty() {
        "          Its marker lists paths refused above, so".into()
    } else {
        format!("          {}, so", s.unread.map_or("", tree::Unread::why))
    });
    out.extend(delete_only_these(tree, s.ours.iter().map(|p| p.display().to_string())));
    Some(out)
}

/// How the delete-the-directory command is introduced where it IS offered. On Windows,
/// only once nothing runs from the tree - removal_report's caveat - because
/// `Remove-Item -Recurse` can take `.tabstatus-generated` before reaching
/// `bin\tabstatus.exe`, and stopping at a running binary then leaves one of ours with
/// no marker: the shape `install` refuses forever.
fn remove_it_with(starts_sentence: bool) -> &'static str {
    match (sys::HAS_UNLINK_RUNNING, starts_sentence) {
        (true, true) => "Remove it with",
        (true, false) => "remove it with",
        (false, true) => "Once nothing is running from it, remove it with",
        (false, false) => "once nothing is running from it, remove it with",
    }
}

/// install's lines about the generated tree the link was just MOVED away from.
fn moved_from_lines(old: &Path) -> Vec<String> {
    match keep_the_directory(old) {
        None => vec![
            format!("          {} is left behind and is now an orphan - {}", old.display(), remove_it_with(false)),
            format!("          `{}`", sys::remove_dir_command(old)),
        ],
        Some(rest) => {
            let mut out = vec![format!("          {} is left behind and is now an orphan.", old.display())];
            out.extend(rest);
            out
        }
    }
}

/// uninstall --keep-tree's line about the tree it kept.
fn kept_tree_lines(tree: &Path) -> Vec<String> {
    match keep_the_directory(tree) {
        None => vec![format!(
            "tree:     {} was kept (--keep-tree). {} `{}`.",
            tree.display(),
            remove_it_with(true),
            sys::remove_dir_command(tree)
        )],
        Some(rest) => {
            let mut out = vec![format!("tree:     {} was kept (--keep-tree).", tree.display())];
            out.extend(rest);
            out
        }
    }
}

/// uninstall's lines about a generated tree that is not the live one.
fn orphan_lines(o: &Path) -> Vec<String> {
    let lead = format!(
        "tree:     {} is another generated tree and was NOT the live one, so it is \
         left behind.",
        o.display()
    );
    match keep_the_directory(o) {
        None => vec![format!("{} {} `{}`.", lead, remove_it_with(true), sys::remove_dir_command(o))],
        Some(rest) => {
            let mut out = vec![lead];
            out.extend(rest);
            out
        }
    }
}

/// The parenthesis naming what removal LEFT BEHIND. A few names, then a count: the
/// point is that the operator can see what is still there and why the directory
/// survived, not that the report reproduces a `find`.
fn describe_extra(extra: &[String]) -> String {
    if extra.is_empty() {
        return String::new();
    }
    format!(", which was left in place because it holds {}", foreign_files(extra))
}

/// `N file(s) nothing here generated: a, b, and K more` - the same few names, then a
/// count, wherever a tree's foreign files are named.
fn foreign_files(extra: &[String]) -> String {
    const SHOW: usize = 5;
    let named: Vec<&str> = extra.iter().take(SHOW).map(String::as_str).collect();
    let more = extra.len() - named.len();
    format!(
        "{} file{} nothing here generated: {}{}",
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
fn uninstall_preflight(c: &Ctx, force: bool, restore_backup: bool, keep_tree: bool) -> Result<Preflight, String> {
    if let Some(e) = refuse_unresolved(c) {
        return Err(e);
    }
    match link_state(&c.link) {
        LinkState::Dir => return Err(refuse_link(&c.link, &format!("a real directory, not a {}", sys::DIR_LINK))),
        LinkState::Other => return Err(refuse_link(&c.link, &format!("not a {}", sys::DIR_LINK))),
        _ => {}
    }
    // The tree goes LAST and holds the binary. Where a running program's file cannot
    // be deleted (Windows), an uninstall run BY that binary would undo everything else
    // and then fail to remove it - a half-removed tree reported as removed - so that
    // is refused here, before anything changes. A hook running it at the same instant
    // is a race this cannot see; the file survives and a later run can take it.
    if !sys::HAS_UNLINK_RUNNING && !keep_tree && tree::is_generated(&c.tree) {
        let hook = c.hook_binary();
        let running = std::env::current_exe()
            .and_then(fs::canonicalize)
            .is_ok_and(|e| fs::canonicalize(&hook).is_ok_and(|h| h == e));
        if running {
            return Err(format!(
                "{} is the binary running this uninstall, and this platform cannot \
                 delete a running program's file, so the plugin tree could not be \
                 removed. Run uninstall from another copy of tabstatus - the one in your \
                 checkout's bin - or pass --keep-tree. Nothing has been changed.",
                hook.display()
            ));
        }
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
        refuse_unreadable_acl(&c.settings, force)?;
    }
    // A restore with no live file gives the restored one the backup's ACL; over a
    // live file it keeps the live one's, checked above, and the backup's is not read.
    if restore_backup && !c.settings.exists() && c.backup.exists() {
        refuse_unreadable_acl(&c.backup, force)?;
    }
    let state = read_state(c)?;

    // The put-back, asked before anything changes, as install asks about its link:
    // `unlink_the_plugin` removes the current link BEFORE it makes the recorded one,
    // so a target this platform cannot link to would cost the user both. Nothing to
    // ask on Unix, where symlink(2) takes any target; on Windows a junction cannot
    // name a share, which a directory symlink install found here may well have.
    if let (LinkState::Link(..), Some(old)) = (link_state(&c.link), put_back_target(state.as_ref())) {
        if let Some(d) = nearest_existing(&c.skills).filter(|d| d.is_dir()) {
            if let Err(e) = sys::probe_dir_link(&old, &d) {
                return Err(format!(
                    "uninstall puts {} back to {}, the target install found there, and a {} \
                     to it cannot be made: {}. The link is removed before that one is made, \
                     so both would be lost. Nothing has been changed.",
                    c.link.display(),
                    old.display(),
                    sys::DIR_LINK,
                    e
                ));
            }
        }
    }

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
            fail(&format!("not removing the plugin {} either - the two together are what", sys::DIR_LINK));
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
        // The ACL and mode: the LIVE file's, as every rewrite keeps them - it is the
        // newest word on who may read this file, and may have been tightened since
        // install. Only with no live file does the restored one take the backup's,
        // which is the ACL the file had when it was backed up.
        let like = if c.settings.exists() {
            copy_settings(&c.settings, &c.safety, "", force)?;
            say(&format!("settings: current file saved to {}", c.safety.display()));
            &c.settings
        } else {
            &c.backup
        };
        let mode = mode_of(like).unwrap_or(0o600);
        write_settings(&c.settings, &raw, mode, like, force)?;
        say(&format!("settings: restored from {}", c.backup.display()));
        if sys::HAS_SECURITY {
            say(if like == &c.settings {
                "          (the access control list of the file it replaced kept)"
            } else {
                "          (with the backup's access control list)"
            });
        }
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
            // Whatever value is about to be LEFT in place. When the record says the
            // key was the user's own, uninstall correctly keeps it - and it is about to
            // unlink the plugin, so this is `late_failure`'s "a tab nothing paints"
            // reached by being scrupulous instead of by failing. It was reported as a
            // neutral "unchanged".
            let kept_on = want.as_deref().map(reads_as_on).unwrap_or(false);
            match settings::restore_env_key(&doc, KEY, want.as_deref(), env_was_there)? {
                Outcome::Unchanged => {
                    say(&format!(
                        "settings: env.{} already holds the value install found - unchanged",
                        KEY
                    ));
                    if kept_on {
                        warn_key_stays_on(c);
                    }
                }
                Outcome::Changed { text, .. } => {
                    let mode = mode_of(&c.settings).unwrap_or(0o600);
                    copy_settings(&c.settings, &c.safety, "", force)?;
                    say(&format!("settings: backed up to {}", c.safety.display()));
                    write_settings(&c.settings, &text, mode, &c.settings, force)?;
                    match &want {
                        Some(raw) => {
                            say(&format!(
                                "settings: restored env.{} = {} (the value install found here)",
                                KEY,
                                String::from_utf8_lossy(raw)
                            ));
                            if kept_on {
                                warn_key_stays_on(c);
                            }
                        }
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
                    say(&format!("         {}", kept_note(mode)));
                }
            }
        }
    }
    Ok(())
}

/// Would Claude Code read this value as "do not paint the title"?
///
/// Deliberately generous, because the rule is Claude Code's and not ours: only the
/// spellings that are obviously OFF count as off, and everything else gets the note.
/// Warning once too often about a key the user set themselves is much the cheaper
/// mistake - the other way round is a blank tab with no explanation anywhere.
fn reads_as_on(raw: &[u8]) -> bool {
    let s = String::from_utf8_lossy(raw);
    !matches!(s.trim().trim_matches('"'), "" | "0" | "false" | "null")
}

/// The combination `late_failure` spells out in install, reached here by uninstall
/// doing the right thing: the record says the key was the user's before this tool ran,
/// so it stays - and the plugin that painted the replacement is being unlinked in the
/// next step. Claude Code's own title painting is off and nothing paints instead.
fn warn_key_stays_on(c: &Ctx) {
    say("          NOTE that value switches Claude Code's OWN title painting off, and the");
    say("          plugin that painted the replacement is unlinked below - so nothing will");
    say(&format!("          paint the tab. It was already set when install ran, so {} keeps", PLUGIN));
    say(&format!("          it; unset it yourself in {} if that was not deliberate.", c.settings.display()));
}

/// AFTER settings.json: the mirror image of install's ordering and for the same
/// reason. The env key switches Claude Code's own title painting off and the plugin
/// paints the replacement, so the moment where only one of the two is undone must be
/// the moment where the KEY is already back. A failure between them then leaves a
/// working install rather than a blank tab.
///
/// One thing it will NOT do: restore a recorded prior target that is a checkout. That
/// is not a hypothetical - the live record on the machine this change was written for
/// says `symlink_before.target` is the checkout, because that is what the link pointed
/// at before the first install, and `record_prior_state` is write-once so the migration
/// leaves it exactly as it is. Restoring it would rebuild the precise wiring this whole
/// change exists to abolish, on an uninstall, silently. So it is declined and said.
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
            let recorded = recorded
                .filter(|old| !old.is_empty())
                .map(|old| PathBuf::from(sys::os_string_from_vec(old)));
            let refuse_restore = recorded.as_deref().filter(|p| is_checkout(p)).map(Path::to_path_buf);
            sys::remove_dir_link(&c.link)
                .map_err(|e| format!("cannot remove {}: {}", c.link.display(), e))?;
            match (refuse_restore, recorded) {
                (Some(co), _) => {
                    say(&format!("{}removed {}", link_label(), c.link.display()));
                    say(&format!("          (was -> {})", t.display()));
                    say(&format!(
                        "          the recorded prior target was the checkout at {};",
                        co.display()
                    ));
                    say("          a checkout is no longer a plugin directory, so the link is");
                    say("          removed rather than pointed back at it.");
                }
                (None, Some(old)) => {
                    symlink(&old, &c.link)?;
                    say(&format!("{}put back the target install found here", link_label()));
                    say(&format!("          {} -> {}", c.link.display(), old.display()));
                }
                (None, None) => {
                    say(&format!("{}removed {}", link_label(), c.link.display()));
                    say(&format!("          (was -> {})", t.display()));
                }
            }
        }
        LinkState::Absent => say(&format!("{}not present - nothing to remove", link_label())),
        LinkState::Dir => return Err(refuse_link(&c.link, &format!("a real directory, not a {}", sys::DIR_LINK))),
        LinkState::Other => return Err(refuse_link(&c.link, &format!("not a {}", sys::DIR_LINK))),
    }
    Ok(())
}

/// The link target `unlink_the_plugin` would put back: the one the record says
/// install found, unless there was none or it is a checkout, which it declines.
fn put_back_target(state: Option<&State>) -> Option<PathBuf> {
    let s = state.filter(|s| s.link_had)?;
    let old = s.link_target.clone().filter(|b| !b.is_empty())?;
    Some(PathBuf::from(sys::os_string_from_vec(old))).filter(|p| !is_checkout(p))
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
/// session will run again to write another. Without this the files sat in the
/// state directory - until logout, or on Windows for good - unmentioned by a report
/// that ends "Done." and
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
    // ONE resolution, for the whole report. This built `Config::from_env()` three
    // separate times, and each one re-ran the whole of `mux::resolve` - survivable
    // while detection was two getenvs, and not survivable now: `MuxOracle::leaf_hint`
    // MAY EXEC, so three constructions are three forks whose answers can disagree,
    // which is precisely the doctor-versus-reality mismatch this report exists to
    // remove. Every `report_*` below takes the stack that was resolved here.
    let cfg = Config::from_env();
    say(&format!("tabstatus {} ({})", env!("CARGO_PKG_VERSION"), target_triple()));
    report_tree(c);
    say(&format!("config:    {}", c.config.display()));
    report_binary(c);
    report_source();
    report_plugin(c);
    report_embedded(c);
    report_env_key(c)?;
    report_state(c);
    report_record();
    report_runtime(&cfg);
    report_stack(&cfg);
    report_tmux(&cfg);
    report_title(&cfg);
    Ok(())
}

/// WHERE the plugin directory is, and how that was decided.
///
/// The repo is no longer the answer to "where is the plugin", and there are three
/// places the answer can come from, so the source is STATED. It replaces a `mode:`
/// line that existed only to say which of two shapes the directory was; there is one
/// shape now, so what is left to report is the location and whether it is ours.
fn report_tree(c: &Ctx) {
    say(&format!("tree:      {} ({})", c.tree.display(), c.tree_from.why()));
    if !c.tree.is_dir() {
        // Both live effects, because they DIFFER and only naming both answers "what
        // happens if I delete the tree but keep the link".
        say("           FAIL the plugin directory is not there.");
        say("           A session already running has its hooks registered and now execs a");
        say("           missing file - 127 per event, and a PreToolUse 127 can block a tool.");
        say("           A NEW session loads no plugin at all and the tab stays BLANK, with no");
        say(&format!("           error anywhere, because env.{} is still set.", KEY));
        say("           `tabstatus install` writes it back.");
        return;
    }
    match tree::read_marker(&c.tree) {
        Some(Ok(m)) => {
            say(&format!(
                "           generated by tabstatus {} ({})",
                String::from_utf8_lossy(&m.version),
                String::from_utf8_lossy(&m.target)
            ));
            // A tree materialised by one architecture and later run by another: the
            // hooks would exec a binary this machine cannot.
            if m.target != target_triple().as_bytes() {
                say(&format!(
                    "           WARN the tree was written by a {} binary and this one is {}",
                    String::from_utf8_lossy(&m.target),
                    target_triple()
                ));
            }
        }
        Some(Err(e)) => say(&format!("           WARN {} is unreadable: {}", tree::MARKER, e)),
        None => {
            say(&format!(
                "           WARN it carries no {}, so `tabstatus install` did not write it",
                tree::MARKER
            ));
            if is_checkout(&c.tree) {
                say("           it is a CHECKOUT - the wiring from before the plugin directory");
                say("           became build output. `tabstatus install` repoints this.");
            }
        }
    }
    report_orphan(c);
}

/// Marked trees that are NOT the live one. Named rather than left silent by both
/// `doctor` and `uninstall`: a hand-repointed link, or an `install --tree <somewhere
/// else>`, leaves a whole working plugin directory that nothing will ever mention
/// again. The two places it can still be found are the state record and the default
/// path.
fn orphan_trees(c: &Ctx) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for cand in [recorded_tree(&c.config), tree::default_tree().ok()].into_iter().flatten() {
        if !sys::same_path(&cand, &c.tree)
            && !out.iter().any(|o| sys::same_path(o, &cand))
            && tree::is_generated(&cand)
        {
            out.push(cand);
        }
    }
    out
}

fn report_orphan(c: &Ctx) {
    for o in orphan_trees(c) {
        say(&format!(
            "           NOTE {} is also a generated tree and is not the live one - an \
             orphan",
            o.display()
        ));
    }
}

/// The checkout the RUNNING BINARY came out of, if there is one: do its manifests still
/// match the copies compiled in?
///
/// A mismatch means the operator edited `hooks/hooks.json` and has not rebuilt, so
/// THIS binary would deploy the OLD copy. WARN, never FAIL - a mid-edit working tree is
/// a normal state. This one line is all that is left of the `Mode::Checkout` report,
/// and it is the only part of it that was ever actionable.
fn report_source() {
    let Some(exe) = std::env::current_exe().ok() else { return };
    let Some(d) = plugin_dir_above(&exe) else { return };
    if tree::is_generated(&d) {
        return;
    }
    let mut drift = false;
    for (rel, text) in embedded::MANIFESTS {
        if let Verdict::Differs { on_disk, embedded: emb } = embedded::compare(&d, rel, text) {
            say(&format!(
                "source:    WARN {} in the checkout at {} differs from the copy compiled",
                rel,
                d.display()
            ));
            say(&format!(
                "           into this binary ({}), so an install from HERE would deploy the",
                embedded::differs_phrase(on_disk, emb)
            ));
            say("           older copy. Rebuild first: sh scripts/build.sh");
            drift = true;
        }
    }
    if !drift {
        say(&format!(
            "source:    OK   {} - both manifests match the copies compiled in",
            d.display()
        ));
    }
}

/// The two manifests in the live tree, against the copies compiled into this binary.
///
/// ONE remedy now, where there used to be two opposite ones: the tree is generated, so
/// drift means the tree is stale and `tabstatus install` rewrites it. The checkout's
/// side of that question is `source:`, above, where it is actionable.
fn report_embedded(c: &Ctx) {
    for (rel, text) in embedded::MANIFESTS {
        match embedded::compare(&c.tree, rel, text) {
            Verdict::Same => say(&format!("embedded:  OK   {} matches the copy compiled in", rel)),
            Verdict::Differs { on_disk, embedded: emb } => {
                say(&format!(
                    "embedded:  WARN {} differs from the copy compiled in ({})",
                    rel,
                    embedded::differs_phrase(on_disk, emb)
                ));
                say("           the tree is stale: `tabstatus install` rewrites it");
            }
            Verdict::Missing => say(&format!(
                "embedded:  FAIL {} is missing, so Claude Code cannot load the plugin. \
                 The copy compiled in is intact: re-run `tabstatus install`",
                rel
            )),
            Verdict::Unreadable(e) => {
                say(&format!("embedded:  WARN {} cannot be read: {}", rel, e))
            }
        }
    }
}

/// The binary hooks.json invokes, which is the one thing whose absence makes every
/// hook exit 127 - and whether it is the build that is running.
fn report_binary(c: &Ctx) {
    let hook = c.hook_binary();
    match fs::metadata(&hook) {
        Ok(m) => match sys::is_executable(&m) {
            Some(true) => say(&format!("binary:    OK   {}", hook.display())),
            Some(false) => say(&format!("binary:    FAIL {} is not executable", hook.display())),
            None => say(&format!(
                "binary:    OK   {} is present - this platform has no execute bit, so \
                 whether it runs is only known when a hook runs it",
                hook.display()
            )),
        },
        Err(_) => say(&format!(
            "binary:    FAIL {} is missing - hooks/hooks.json invokes it, so \
             nothing paints",
            hook.display()
        )),
    }
    report_tree_binary(c);
}

/// Is the live tree's binary the one running? UNCONDITIONALLY, which it was not: this
/// comparison used to be reachable only inside the deleted standalone arm, so the
/// commonest real case of all - `./bin/tabstatus doctor` in the checkout after a
/// rebuild, asking whether the running build has been deployed - never reached it.
fn report_tree_binary(c: &Ctx) {
    let tree_bin = c.hook_binary();
    let exe = std::env::current_exe().ok();
    let same_path = exe
        .as_ref()
        .and_then(|e| fs::canonicalize(e).ok())
        .zip(fs::canonicalize(&tree_bin).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if same_path {
        say("           the binary in the tree is the one running this report");
        return;
    }
    let both = exe.as_ref().and_then(|e| fs::read(e).ok()).zip(fs::read(&tree_bin).ok());
    match both {
        Some((a, b)) if a == b => say("           identical to the one running this report"),
        Some((a, b)) => {
            // Same size and different bytes is the LIKELY shape of this, because a
            // version bump rarely changes the length, so "635440 vs 635440" would
            // read as a bug in the report rather than as the answer.
            say(&format!(
                "           WARN the live tree is running a different build of tabstatus \
                 than this one ({})",
                if a.len() == b.len() {
                    format!("same {} bytes, different content", a.len())
                } else {
                    format!("{} in the tree vs {} running", b.len(), a.len())
                }
            ));
            say("           deploy this build: tabstatus install");
        }
        None => {}
    }
}

/// The symlink Claude Code loads the plugin through - and, when it points at a
/// checkout, the fact that this is the wiring from before the change, said in those
/// words. `doctor` is what people run when a tab misbehaves, so it is how the
/// migration gets discovered.
fn report_plugin(c: &Ctx) {
    match link_state(&c.link) {
        LinkState::Absent => say(&format!(
            "plugin:    FAIL not linked. Run `tabstatus install`. ({})",
            c.link.display()
        )),
        LinkState::Link(t, true) if sys::same_path(&t, &c.tree) && is_checkout(&t) => {
            say(&format!(
                "plugin:    WARN {} points at a CHECKOUT, not at a generated tree:",
                c.link.display()
            ));
            say(&format!("           {}", t.display()));
            say("           That is the wiring from before the plugin directory became build");
            say("           output - a `git checkout` there changes what every running session");
            say("           runs. `tabstatus install` repoints it at a generated tree.");
        }
        LinkState::Link(t, true) if sys::same_path(&t, &c.tree) => {
            say(&format!("plugin:    OK   linked, {} -> {}", c.link.display(), t.display()))
        }
        LinkState::Link(t, true) => say(&format!(
            "plugin:    WARN {} points at {}, not at the tree above",
            c.link.display(),
            t.display()
        )),
        LinkState::Link(t, false) => say(&format!(
            "plugin:    FAIL {} is a broken {} to {}",
            c.link.display(),
            sys::DIR_LINK,
            t.display()
        )),
        LinkState::Dir => say(&format!(
            "plugin:    WARN {} is a real directory, not a link to the plugin tree",
            c.link.display()
        )),
        LinkState::Other => {
            say(&format!("plugin:    WARN {} exists and is not a {}", c.link.display(), sys::DIR_LINK))
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
            "settings:  {}{}{}",
            c.settings.display(),
            mode_note(mode_of(&c.settings).unwrap_or(0), ""),
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

/// What the runtime half would decide from this environment: which terminal, why,
/// and which glyph position that implies.
///
/// The `pty:` line this used to end with is now the platform axis's `session
/// terminal` row, where the fact belongs: `$CLAUDE_PID` is how a PLATFORM reaches
/// the session's own terminal, and it was the one line here that was not about the
/// leaf. Its four sentences are unchanged.
fn report_runtime(cfg: &Config) {
    let konsole_vars = config::flag("KONSOLE_VERSION") || config::flag("KONSOLE_DBUS_SESSION");
    // The layer the ENVIRONMENT claimed, carried out of the one resolution rather
    // than re-read from `$TMUX` and `$STY` here. It is deliberately not
    // `stack.mux.is_some()`: a `$TMUX` that is not `<socket>,<pid>,<session>` leaves
    // nothing to drive and still swallowed the leaf's evidence, and this line has
    // always named the layer that did the swallowing.
    let mux = cfg.stack.claimed.map_or("", |k| k.caps().name);
    let konsole = cfg.stack.leaf == Surface::Konsole;
    // The REASON matters more than the answer, because there are now three of
    // them and they disagree: an explicit CCTAB_TERMINAL, inherited KONSOLE_*,
    // and a multiplexer that makes the inherited kind meaningless.
    let override_ = config::var_nonempty("CCTAB_TERMINAL");
    say(&format!(
        "terminal:  {}",
        match (&override_, konsole, konsole_vars, mux.is_empty()) {
            (Some(v), true, _, _) => format!(
                "Konsole, from CCTAB_TERMINAL={} - the only signal that survives ssh",
                String::from_utf8_lossy(v.as_encoded_bytes())
            ),
            (Some(v), false, _, _) => format!(
                "not Konsole: CCTAB_TERMINAL={} says so explicitly",
                String::from_utf8_lossy(v.as_encoded_bytes())
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
            Some(p) => format!("{} ", String::from_utf8_lossy(p.as_encoded_bytes())),
        },
        implied
    ));
}

// --- doctor: the three axes -------------------------------------------------

/// The capability table's columns, FIXED so that a report from Linux and one from a
/// Windows build diff cleanly. That matters more here than anywhere else in the
/// report: fourteen surface rows exist, six of them have never had a byte delivered
/// to them, and the only way to compare a column that was measured with one that was
/// read out of vendor source is to have them land in the same place.
const AXIS_W: usize = 12;
const VALUE_W: usize = 29;
const NAME_W: usize = 22;
/// `ok` / `n/a` / `off` / `?` / `fail` and one space - [`Support::label`] is the only
/// place those five words are spelled, so this is the only place their width is.
const LABEL_W: usize = 6;
/// Where a capability's own text starts, and therefore where a second line of it is
/// indented to.
const DETAIL_COL: usize = 2 + NAME_W + LABEL_W;

/// One axis: which of the three, what it resolved to, and one phrase about the whole
/// of it.
fn axis(which: &str, value: &str, note: &str) {
    say(format!("{which:<AXIS_W$}{value:<VALUE_W$}{note}").trim_end());
}

/// One capability, in two columns: the verdict word and the one thing a reader can
/// act on.
fn row(name: &str, label: &str, detail: &str) {
    say(format!("  {name:<NAME_W$}{label:<LABEL_W$}{detail}").trim_end());
}

/// A capability whose verdict IS a [`Support`], which is all of them but two.
///
/// `Support::label` and `Support::reason` are the whole formatter - doctor cannot
/// invent a sixth word, and cannot print an absence without the reason the row
/// carries - and `detail` is what to show when the answer is `ok` and there is
/// therefore no reason to print: the grammar, the path, the pid.
fn cap<T>(name: &str, s: &Support<T>, detail: &str) {
    row(name, s.label(), s.reason().as_deref().unwrap_or(detail));
}

/// A second line of one capability's text, under the first.
fn more(detail: &str) {
    say(format!("{:DETAIL_COL$}{detail}", "").trim_end());
}

/// Which OSC and how it is terminated, which is one fact: VTE drops a BEL-terminated
/// `OSC 9;4` on purpose, so a report naming the sequence without its terminator
/// would say that VTE and kitty agree.
fn grammar((osc, t): (&str, Terminator)) -> String {
    format!("{osc} {}", t.name())
}

/// An escape sequence as a REPORT can print it: the bytes themselves, with ESC and
/// BEL named rather than written.
///
/// doctor is read in the terminal whose tab is misbehaving. A report that echoed the
/// real control bytes would arm that terminal while describing the arming.
fn visible(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x1b => out.push_str("ESC"),
            0x07 => out.push_str(" BEL"),
            0x20..=0x7e => out.push(char::from(b)),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    out
}

/// Every name `--surface` accepts, which is every variant's - including the ones
/// this build could never detect, because over ssh an override is the only way a
/// leaf is knowable at all.
fn surface_names() -> String {
    Surface::ALL
        .iter()
        .map(|s| s.caps().name)
        .collect::<Vec<&str>>()
        .join(", ")
}

/// The three axes and what each one can do, from the stack resolved ONCE at the top
/// of the report.
///
/// This is the user-visible payoff of the whole backend refactor: the axes existed
/// and nobody could see them. Platform verdicts lift the paint path's native
/// answers; the surface resolves its protocol catalogue against version evidence
/// here, without adding version queries to the painting path.
fn report_stack(cfg: &Config) {
    report_platform(cfg);
    report_leaf(
        cfg.stack.leaf,
        Some(&leaf_evidence(cfg)),
        surface::version_evidence(cfg.stack.leaf, cfg.stack.claimed.is_some()),
    );
    report_mux(&cfg.stack);
}

/// Can this build NAME the leaf, and what named it? The `Support` shape is not a
/// dressing-up: "nothing named it" is a capability this session does not have, and
/// the two ways of not having it - an override nobody set, and evidence a
/// multiplexer swallowed - have different remedies.
fn leaf_evidence(cfg: &Config) -> Support<String> {
    let in_mux = cfg.stack.claimed.is_some();
    match (
        config::var_nonempty("CCTAB_TERMINAL"),
        surface::evidence(in_mux),
    ) {
        (Some(v), _) => Support::Available(format!(
            "CCTAB_TERMINAL={}",
            String::from_utf8_lossy(v.as_encoded_bytes())
        )),
        (None, Some(var)) => Support::Available(format!("${var}")),
        // Rung 2 of the ladder: nothing in the environment named it and it is named
        // anyway, so the multiplexer did. Unreachable while doctor asks `NoOracle` -
        // and it is here rather than in #18's commit because a report that answered
        // "nothing named it" beside a named leaf would be the same staleness that
        // issue is about.
        (None, None) if cfg.stack.leaf != Surface::Unknown => {
            Support::Available("the multiplexer".to_owned())
        }
        (None, None) if in_mux => {
            Support::Unsupported("a multiplexer swallowed the environment's evidence")
        }
        (None, None) => Support::Unsupported("nothing in the environment named it"),
    }
}

/// The platform axis: what this build's operating system supplies.
///
/// THE MAPPING LIVES HERE AND NOWHERE ELSE. `crate::sys` answers in `Option`, `bool`
/// and `HAS_*`, which is the right shape for a caller that has to branch, and those
/// answers are lifted into [`Support`] at this boundary - the only place a REPORT is
/// produced. No signature in `sys` changes to serve a report, and no row below can
/// claim a capability whose constant the paint path does not read.
///
/// The session's own terminal is reported from `$CLAUDE_PID` and deliberately NOT by
/// opening it: on Windows the console route attaches a console, and releasing one
/// invalidates this process's stdout handles - so a report that proved the
/// capability by taking it would truncate itself, on exactly the platform it exists
/// to explain.
fn report_platform(cfg: &Config) {
    axis(
        "platform",
        std::env::consts::OS,
        &format!(
            "a record carries its session's origin as `{} <pid> <start>`",
            sys::ORIGIN_KEY
        ),
    );
    // The four sentences the `pty:` line used to print, unchanged, now as one row's
    // verdict plus its reason. Which of them applies is decided by the two platform
    // constants and by the stack resolved at the top of the report - never by a
    // second `Tmux::detect()`, which is the drift this commit removes.
    let pid = cfg
        .claude_pid
        .as_deref()
        .map(|p| String::from_utf8_lossy(p.as_encoded_bytes()).into_owned());
    let session: Presence = match (&pid, sys::HAS_SESSION_CONSOLE, cfg.stack.tmux().is_some()) {
        (None, _, _) => Support::Unsupported(
            "CLAUDE_PID is not set, so this is not a hook subprocess (session-start \
             and session-end would do nothing)",
        ),
        // The same test `emit::write_session` makes: inside tmux the title is tmux's
        // carrier, so no console is titled.
        (Some(_), true, true) => Support::Unsupported(
            "no pty here, and inside tmux no console title is set either, so \
             session-start and session-end paint nothing directly",
        ),
        (Some(_), true, false) => YES,
        (Some(_), false, _) if !sys::HAS_SESSION_TTY => Support::Unsupported(
            "this platform has no pty to resolve from it, so session-start and \
             session-end paint nothing directly",
        ),
        (Some(_), false, _) => YES,
    };
    cap(
        "session terminal",
        &session,
        &match (&pid, sys::HAS_SESSION_CONSOLE) {
            (Some(p), true) => format!(
                "CLAUDE_PID={p} - session-start and session-end set the title of its \
                 console, when it is a 64-bit process, an ancestor of the hook, and \
                 its stdout is that console"
            ),
            (Some(p), false) => {
                format!("CLAUDE_PID={p} - session-start and session-end write it directly")
            }
            (None, _) => String::new(),
        },
    );
    if let Some(p) = &pid {
        if !session.is_available() {
            more(&format!("CLAUDE_PID={p}"));
        }
    }
    // OUR OWN pid, because what is under test here is the platform primitive and not
    // some other process: the session's pid may be gone, and on native Windows it
    // may never have been exported at all.
    let me = std::process::id();
    let stamp: Support<u64> = match sys::process_start_time(me) {
        Some(t) => Support::Available(t),
        None => Support::Unsupported(
            "this platform will not say when a pid started, so the reaper falls back \
             to the record's mtime",
        ),
    };
    cap(
        "process stamp",
        &stamp,
        &match &stamp {
            Support::Available(t) => format!("{t}, this process (pid {me})"),
            _ => String::new(),
        },
    );
    let lock: Presence = if sys::HAS_RECORD_LOCK {
        YES
    } else {
        Support::Unsupported("no record lock here that can be proven held, so the \
                              whole state layer stays off")
    };
    cap(
        "record lock",
        &lock,
        "a record is written under an exclusive lock proven to hold that same file",
    );
    let dir: Support<PathBuf> = match state::dir() {
        Some(d) => Support::Available(d),
        None if sys::HAS_RECORD_LOCK => Support::Unsupported(sys::NO_STATE_DIR),
        None => Support::Unsupported("there is no record lock to protect one"),
    };
    cap(
        "state dir",
        &dir,
        &match &dir {
            Support::Available(d) => format!("{} (CCTAB_STATE_DIR, else ${})", d.display(), sys::RUNTIME_DIR_VAR),
            _ => String::new(),
        },
    );
    let modes: Presence = if sys::HAS_MODES {
        YES
    } else {
        Support::Unsupported(
            "no POSIX mode bits here, so the directory's own inherited ACL is what \
             keeps a record private",
        )
    };
    cap("file modes", &modes, "the record directory is created 0700");
    let unlink: Presence = if sys::HAS_UNLINK_RUNNING {
        YES
    } else {
        Support::Unsupported(
            "a running program's own file cannot be replaced in one rename here, so \
             install renames it aside first",
        )
    };
    cap(
        "replace while running",
        &unlink,
        "one rename, atomic, over the very binary the hooks exec",
    );
    row(
        "plugin link",
        "ok",
        &format!("a {} points Claude Code's config at the generated tree", sys::DIR_LINK),
    );
    // `Support::gate` layers OUR knob over the platform's answer, which is what stops
    // this row claiming to be the name that gets painted when `CCTAB_HOST` is what
    // does.
    let named = location::hostname(cfg);
    let host: Support<String> = match named {
        Some(h) => Support::Available(h),
        None => Support::Unsupported("nothing here names this machine"),
    }
    .gate(cfg.host_override.as_ref().map(|_| "CCTAB_HOST"));
    cap(
        "hostname",
        &host,
        match &host {
            Support::Available(h) => h,
            _ => "",
        },
    );
}

/// The surface axis: the leaf terminal's whole capability row.
///
/// `evidence` is `None` for `doctor --surface <name>`, where there is no session to
/// have evidence about - the point of that spelling is to read a table for a machine
/// this is not. Versioned protocols use the surface's reporting resolver in both
/// modes; the offline catalogue never borrows evidence from the local environment.
fn report_leaf(
    s: Surface,
    evidence: Option<&Support<String>>,
    version: surface::VersionEvidence,
) {
    let c = s.caps();
    axis("surface", c.name, &format!("{}, {}", c.human, c.source.why()));
    if let Some(e) = evidence {
        cap(
            "evidence",
            e,
            match e {
                Support::Available(what) => what,
                _ => "",
            },
        );
    }
    // Not a `Support`: which end of a label a terminal throws away is not a
    // capability it has or lacks. The layout DECISION is `glyph:` above, which
    // `CCTAB_GLYPH_POS` can win; this is the measurement under it.
    let (word, cut) = match c.elide {
        Elide::Left => ("left", "the tab label is cut from the left, so a glyph goes last"),
        Elide::Right => ("right", "the tab label is cut from the right, so a glyph goes first"),
        Elide::Unknown => ("?", "no truncation behaviour has ever been observed here"),
    };
    row("elide", word, cut);
    cap("title (OSC 0)", &c.title.osc0, "icon name and window title together");
    cap("title (OSC 1)", &c.title.osc1, "");
    cap("title (OSC 2)", &c.title.osc2, "");
    cap("title stack (CSI 22t)", &c.title.stack_22t, "");
    report_protocol("tab colour", &c.tab_color, version, |g| g.grammar());
    let a = &c.attention;
    cap("bell", &a.bell, "");
    report_protocol("notification", &a.notify, version, |g| g.grammar());
    report_protocol("taskbar progress", &a.progress, version, |g| g.grammar());
    cap("acknowledge", &a.acknowledge, "");
    // `Option<Arming>` has nowhere to carry a reason, so the two absences are spelled
    // here. They are not the same absence: twelve surfaces need no arming, and
    // `Unknown` is REFUSED one, because appearance bytes are never written to a
    // terminal that cannot be named.
    let armed: Presence = match (&c.arming, s) {
        (Some(_), _) => YES,
        (None, Surface::Unknown) => {
            Support::Unsupported("a terminal that cannot be named is sent no appearance bytes")
        }
        (None, _) => Support::Unsupported("a title shows here with nothing armed first"),
    };
    // The pair, always together: `Arming::pair` is the only way to read either one
    // out, because an arm whose restore drifted from it is this project's named
    // recurring defect and a report is where a drift would be seen.
    match &c.arming {
        Some(arm) => {
            let (on, off) = arm.pair();
            cap("arm / restore", &armed, &visible(on));
            more(&format!("back to {}", visible(off)));
            more("which is the terminal's COMPILED-IN default, not your profile");
        }
        None => cap("arm / restore", &armed, ""),
    }
}

/// Format the resolved claim and retain the catalogue grammar and requirement
/// even when the running terminal's version cannot establish support.
fn report_protocol<T>(
    name: &str,
    protocol: &surface::Protocol<T>,
    version: surface::VersionEvidence,
    syntax: impl Fn(&T) -> (&'static str, Terminator),
) {
    let detail = protocol
        .catalogue
        .emittable()
        .map(|g| grammar(syntax(g)))
        .unwrap_or_default();
    cap(name, protocol.reported(version), &detail);
    if let Some(minimum) = protocol.minimum {
        more(&format!("{detail}; requires {minimum} or newer"));
    } else if !detail.is_empty() && !protocol.catalogue.is_available() {
        more(&detail);
    }
}

/// The multiplexer axis: which one the environment claimed, whether it is one we can
/// drive, and what it does for us.
fn report_mux(st: &Stack) {
    // The three absences a single `Option<Mux>` flattened into one: our knob turned
    // it off, the environment named one we cannot drive, and there is none. Only the
    // first has a remedy, and it is the name of the knob.
    let driving: Presence = match (&st.mux, st.disabled_by, st.claimed) {
        (Some(_), _, _) => YES,
        (None, Some(knob), _) => Support::Disabled(knob),
        (None, None, Some(MuxKind::Tmux)) => Support::Unsupported(
            "$TMUX is not <socket>,<pid>,<session>, so there is nothing to drive",
        ),
        (None, None, Some(_)) => {
            Support::Unsupported("the environment names one and nothing here can drive it")
        }
        (None, None, None) => Support::Unsupported("neither $TMUX nor $STY is set"),
    };
    axis(
        "multiplexer",
        st.claimed.map_or("none", |k| k.caps().name),
        &if driving.is_available() {
            String::new()
        } else {
            driving.to_string()
        },
    );
    // The rows describe what we could ASK of it, so they are printed for a
    // multiplexer that is actually there and for no other.
    let Some(m) = &st.mux else { return };
    let caps = m.caps();
    cap(
        "outer title",
        &caps.title_renderer(),
        "it re-renders its own format on a timer, which is what lets a glyph decay",
    );
    cap(
        "client registry",
        &caps.client_registry,
        "it names each attached client's pty, where the leaf's appearance bytes go",
    );
}

/// `doctor --surface <name>`: one surface's capability table, for a terminal this
/// machine cannot run.
///
/// Every input is `&'static` data, so this needs no terminal, no config directory and
/// no session - which is how a human reads the Windows column before any Windows box
/// exists. It prints the surface axis ALONE: the other two describe this machine, and
/// this spelling is about another one.
fn surface_table(name: &OsStr) -> i32 {
    let want = name.as_encoded_bytes();
    let Some(s) = surface::by_name(want) else {
        fail(&format!(
            "no surface is named {}. One of: {}",
            String::from_utf8_lossy(want),
            surface_names()
        ));
        return 1;
    };
    version();
    report_leaf(s, None, surface::VersionEvidence::Catalogue);
    // At the AXIS's own column, not a capability's: indented to `more`'s depth it
    // would read as a third line of the arming row above it.
    say(&format!(
        "{:AXIS_W$}CCTAB_TERMINAL={} is what names this surface to a session that \
         cannot detect it",
        "",
        s.caps().name
    ));
    0
}

/// Inside tmux or not, the socket, the pane, whether the decay's clock is running,
/// whether the outer terminal can be given a title at all, and whose
/// set-titles-string is installed.
///
/// Two tmux invocations, both read-only, both on a cold path. Nothing here is a
/// copy of what the runtime half decides: [`tmux::report`] is in the module that
/// decides it.
fn report_tmux(cfg: &Config) {
    for line in tmux::report(cfg) {
        say(&line);
    }
}

/// The runtime half's own pipeline, CALLED rather than copied, so this line cannot
/// drift from what actually paints.
fn report_title(cfg: &Config) {
    let title = render::compose(Paint::Line(Glyph::Idle), cfg).title;
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
        assert!(matches!(parse(&["install"]), Subcommand::Install { tree: None, force: false }));
        assert!(matches!(
            parse(&["uninstall"]),
            Subcommand::Uninstall { force: false, restore_backup: false, keep_tree: false }
        ));
        // The word that used to be a second install verb still parses, so it can point
        // at its replacement instead of erroring obscurely - and it must stay a
        // subcommand, because a word that falls through to the paint path would paint
        // an idle tab.
        assert!(matches!(parse(&["standalone"]), Subcommand::StandaloneGone));
        assert!(matches!(parse(&["standalone", "/tmp/x"]), Subcommand::StandaloneGone));
        assert!(matches!(parse(&["doctor"]), Subcommand::Doctor { surface: None }));
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
        match parse(&["install", "--tree", "/tmp/somewhere"]) {
            Subcommand::Install { tree: Some(d), force: false } => assert_eq!(d, OsString::from("/tmp/somewhere")),
            _ => panic!("install --tree must carry the directory"),
        }
        // install's --force lifts only the no-ACL refusal; it combines with --tree in
        // either order.
        for words in [&["install", "--force"][..], &["install", "--force", "--tree", "/t"][..], &["install", "--tree", "/t", "--force"][..]] {
            assert!(matches!(parse(words), Subcommand::Install { force: true, .. }), "{:?}", words);
        }
        assert!(matches!(
            parse(&["uninstall", "--force"]),
            Subcommand::Uninstall { force: true, restore_backup: false, keep_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--restore-backup"]),
            Subcommand::Uninstall { force: false, restore_backup: true, keep_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--restore-backup", "--force"]),
            Subcommand::Uninstall { force: true, restore_backup: true, keep_tree: false }
        ));
        assert!(matches!(
            parse(&["uninstall", "--keep-tree"]),
            Subcommand::Uninstall { force: false, restore_backup: false, keep_tree: true }
        ));
        // Repeats are idempotent, as they were: the last one wins.
        match parse(&["install", "--tree", "/a", "--tree", "/b"]) {
            Subcommand::Install { tree: Some(d), .. } => assert_eq!(d, OsString::from("/b")),
            _ => panic!("the last --tree wins"),
        }
    }

    /// `--tree` is the only option here that takes a value, so the two ways to get it
    /// wrong need saying rather than guessing: a missing directory must not silently
    /// mean the default, and a mistyped flag must not become a directory NAME and get
    /// a plugin tree materialised into `./--force`.
    #[test]
    fn tree_needs_a_directory_and_will_not_take_a_flag_as_one() {
        for words in [&["install", "--tree"][..], &["install", "--tree", "--force"][..]] {
            match parse(words) {
                Subcommand::BadUsage(why) => assert!(why.contains("--tree"), "{}", why),
                _ => panic!("{:?} should be a BadUsage", words),
            }
        }
        // And a bare directory, which is how the deleted verb took it: named, with the
        // spelling that works.
        match parse(&["install", "/tmp/somewhere"]) {
            Subcommand::BadUsage(why) => {
                assert!(why.contains("--tree /tmp/somewhere"), "{}", why)
            }
            _ => panic!("a bare directory should be a BadUsage"),
        }
    }

    #[test]
    fn an_option_the_verb_does_not_accept_is_named_rather_than_ignored() {
        // `--restore-backup` is a real flag, but install has never taken it.
        for words in [
            &["install", "--restore-backup"][..],
            &["install", "--bogus"][..],
            &["install", "--tree", "/tmp/x", "--bogus"][..],
            &["uninstall", "--bogus"][..],
            // `--keep-tree` is a real flag, but only uninstall takes it.
            &["install", "--keep-tree"][..],
            // `--tree` is a real flag, but only install takes it.
            &["uninstall", "--tree"][..],
        ] {
            match parse(words) {
                Subcommand::BadOption(a) => assert_eq!(a, OsString::from(words[words.len() - 1])),
                _ => panic!("{:?} should be a BadOption", words),
            }
        }
    }

    #[test]
    fn the_three_read_only_verbs_ignore_their_arguments() {
        // Reproduced, not improved: `doctor --force` has always run doctor, and the
        // one option doctor now takes must not turn the others into refusals.
        assert!(matches!(
            parse(&["doctor", "--force", "--bogus"]),
            Subcommand::Doctor { surface: None }
        ));
        assert!(matches!(parse(&["version", "--bogus"]), Subcommand::Version));
        assert!(matches!(parse(&["help", "--bogus"]), Subcommand::Help));
    }

    /// `--surface` takes a value, is found among arguments doctor ignores, and names
    /// the fourteen when it is given nothing usable - a report must not fall through
    /// to the paint path and paint an idle tab, which is what a `None` from `parse`
    /// would have done.
    #[test]
    fn the_surface_table_is_asked_for_by_name_or_refused_by_name() {
        for words in [
            vec!["doctor", "--surface", "konsole"],
            vec!["doctor", "--force", "--surface", "konsole"],
        ] {
            match parse(&words) {
                Subcommand::Doctor { surface: Some(n) } => assert_eq!(n, OsString::from("konsole")),
                _ => panic!("{words:?} should name a surface"),
            }
        }
        for words in [
            vec!["doctor", "--surface"],
            vec!["doctor", "--surface", ""],
            vec!["doctor", "--surface", "--force"],
        ] {
            match parse(&words) {
                Subcommand::BadUsage(why) => {
                    assert!(why.contains("windows-terminal"), "{why}");
                    assert!(why.contains("konsole"), "{why}");
                }
                _ => panic!("{words:?} should be a BadUsage naming the fourteen"),
            }
        }
    }

    /// Every one of the fourteen names reaches its own row, because that list is what
    /// the refusal above prints and what a reader over ssh has to choose from.
    #[test]
    fn every_surface_name_the_refusal_prints_is_a_surface() {
        let printed = surface_names();
        for s in Surface::ALL {
            let name = s.caps().name;
            assert!(printed.contains(name), "{name} is not offered");
            let found = surface::by_name(name.to_uppercase().as_bytes());
            assert!(matches!(found, Some(f) if f == *s), "{name}");
        }
    }

    /// The verbs must be as disjoint from the edge names as the old ones.
    #[test]
    fn print_embedded_carries_its_name_and_near_misses_still_paint() {
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
        // The two Claude Code demands of a plugin directory, which is what install
        // writes and what `doctor` compares - so the embedded set and the set a
        // loadable tree needs cannot drift apart.
        for (rel, _) in embedded::MANIFESTS {
            assert!(
                [".claude-plugin/plugin.json", "hooks/hooks.json"].contains(&rel),
                "{} is embedded but is not one of the two a plugin directory needs",
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
            tree: PathBuf::from("/tree"),
            tree_from: TreeFrom::Default,
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
        // The half every install rewrites, and the ONLY field of the record that is
        // read back other than the prior state: without it, `uninstall` can find the
        // tree only through the link, and a hand-repointed link orphans it.
        assert_eq!(
            root.get("tree").and_then(|m| m.val.as_str()),
            Some(&b"/tree"[..])
        );
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
    // Non-UTF-8 path bytes exist only on Unix; a Windows OsString cannot hold them.
    #[cfg(unix)]
    #[test]
    fn an_invalid_utf8_path_is_quoted_one_replacement_per_byte() {
        let mut c = ctx();
        c.tree = PathBuf::from(sys::os_string_from_vec(b"/tree/tr\xf0\x9f\x98x".to_vec()));
        let raw = state_text(&c, &State {
            env_had: false,
            env_raw: None,
            env_object_had: false,
            link_had: false,
            link_target: None,
        });
        let v = json::parse(&raw).expect("still valid JSON");
        let t = v.as_obj().and_then(|o| o.get("tree")).and_then(|m| m.val.as_str());
        // Three bytes of a truncated four-byte sequence: three U+FFFD, not one.
        assert_eq!(t, Some("/tree/tr\u{fffd}\u{fffd}\u{fffd}x".as_bytes()));
    }

    /// A fresh directory under the system temp dir, unique to this test and process.
    /// Nothing here resolves HOME or a config directory: `removal_report` takes the
    /// tree it reports on, and `tree::remove` touches only that.
    fn report_scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cctab-report-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// A marker-listed generated file that `remove_file` cannot take, on every
    /// platform: it is a directory. Its removal FAILS, so the marker stays - the same
    /// shape a running hook holding the binary leaves on Windows. `extra` is written
    /// beside it as a file the tool did not generate; `listed` adds marker entries.
    fn tree_with_a_failing_file(d: &Path, extra: Option<&str>, listed: &[&str]) -> (PathBuf, PathBuf) {
        let tree = d.join("claude-tabstatus");
        let stuck = tree.join("hooks").join("hooks.json");
        fs::create_dir_all(&stuck).expect("mkdir");
        fs::create_dir_all(tree.join(".claude-plugin")).expect("mkdir");
        fs::write(tree.join(".claude-plugin").join("plugin.json"), b"{}").expect("write");
        if let Some(name) = extra {
            fs::write(tree.join(name), b"mine").expect("write");
        }
        let mut files = vec![".claude-plugin/plugin.json", "hooks/hooks.json"];
        files.extend_from_slice(listed);
        fs::write(tree::marker_path(&tree), tree::marker_text("0.1.0", "t", &files)).expect("write");
        (tree, stuck)
    }

    /// The delete-the-directory command is the line a hurried operator copies. With a
    /// file of theirs in the tree it must not be printed, even when a removal failed -
    /// which on Windows is any uninstall run during a session. The files that would
    /// not go are named instead, and the marker is kept.
    #[test]
    fn a_failed_removal_never_offers_to_delete_a_directory_holding_somebody_elses_file() {
        let d = report_scratch("foreign");
        let (tree, stuck) = tree_with_a_failing_file(&d, Some("NOTES.txt"), &[]);
        let r = tree::remove(&tree).expect("removed");
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert_eq!(r.left, vec!["NOTES.txt"]);
        let lines = removal_report(&tree, &r);
        let text = lines.join("\n");
        assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
        assert!(!text.contains("rm -rf") && !text.contains("Remove-Item"), "{}", text);
        assert!(text.contains("do NOT delete the directory"), "{}", text);
        assert!(text.contains("It also holds files nothing here generated"), "{}", text);
        assert_eq!(text.contains("nothing is running"), !sys::HAS_UNLINK_RUNNING, "{}", text);
        assert!(lines.contains(&format!("            {}", stuck.display())), "{}", text);
        assert!(lines.contains(&format!("            {}", tree::marker_path(&tree).display())), "{}", text);
        assert!(tree::is_generated(&tree), "the marker stays while a file it lists does");
        assert_eq!(fs::read(tree.join("NOTES.txt")).expect("kept"), b"mine");
        let _ = fs::remove_dir_all(&d);

        // A marker-listed path refused as outside the tree is a thing left too.
        let d = report_scratch("blocked");
        let (tree, _) = tree_with_a_failing_file(&d, None, &["../outside"]);
        let r = tree::remove(&tree).expect("removed");
        assert!(r.left.is_empty(), "{:?}", r.left);
        assert_eq!(r.blocked.len(), 1, "{:?}", r.blocked);
        let text = removal_report(&tree, &r).join("\n");
        assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
        assert!(text.contains("do NOT delete the directory"), "{}", text);
        // Nothing foreign is IN the tree, so the report must not say there is.
        assert!(text.contains("Its marker lists paths refused above"), "{}", text);
        assert!(!text.contains("It also holds"), "{}", text);
        assert_eq!(text.contains("nothing is running"), !sys::HAS_UNLINK_RUNNING, "{}", text);
        let _ = fs::remove_dir_all(&d);
    }

    /// The failing marker-listed path is a directory with somebody's file inside:
    /// "delete only these" must not hand over that directory as something to delete
    /// outright. The file inside is named as left, the directory is qualified.
    #[test]
    fn a_failed_directory_holding_somebody_elses_file_is_not_listed_for_deletion() {
        let d = report_scratch("dirheld");
        let (tree, stuck) = tree_with_a_failing_file(&d, None, &[]);
        fs::write(stuck.join("X"), b"mine").expect("write");
        let r = tree::remove(&tree).expect("removed");
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert_eq!(r.left, vec!["hooks/hooks.json/X"]);
        let lines = removal_report(&tree, &r);
        let text = lines.join("\n");
        assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
        assert!(!lines.contains(&format!("            {}", stuck.display())), "{}", text);
        assert!(
            lines.contains(&format!("            {} (a directory - only once it is empty)", stuck.display())),
            "{}",
            text
        );
        assert_eq!(fs::read(stuck.join("X")).expect("kept"), b"mine");
        let _ = fs::remove_dir_all(&d);
    }

    /// Without a failure, the survival arm reads exactly as it did before `failed`
    /// existed - including with a refused marker entry, which points at nothing in
    /// the tree for the command to follow.
    #[test]
    fn a_tree_left_with_only_empty_directories_still_offers_the_command() {
        for (tag, listed) in [("emptyok", &[][..]), ("emptyblk", &["../outside"][..])] {
            let d = report_scratch(tag);
            let tree = d.join("claude-tabstatus");
            fs::create_dir_all(tree.join("keep").join("deeper")).expect("mkdir");
            fs::create_dir_all(tree.join("bin")).expect("mkdir");
            fs::write(tree.join("bin").join("x"), b"x").expect("write");
            let mut files = vec!["bin/x"];
            files.extend_from_slice(listed);
            fs::write(tree::marker_path(&tree), tree::marker_text("0.1.0", "t", &files)).expect("write");
            let r = tree::remove(&tree).expect("removed");
            assert!(r.failed.is_empty() && r.left.is_empty() && !r.dir_gone, "{:?} {:?}", r.failed, r.left);
            assert_eq!(r.blocked.len(), listed.len(), "{:?}", r.blocked);
            let mut want = vec![
                format!("tree:     removed 1 generated file from {}", tree.display()),
                "          the directory itself survived - it holds no file this report can".to_string(),
                format!("          name, only empty directories. Remove it with `{}`", sys::remove_dir_command(&tree)),
            ];
            want.extend(r.blocked.iter().map(|why| format!("tree:     LEFT a file the marker listed - {}", why)));
            assert_eq!(removal_report(&tree, &r), want);
            let _ = fs::remove_dir_all(&d);
        }
    }

    /// ...and with nothing of anybody else's in there, the command is still offered:
    /// every file left is ours.
    #[test]
    fn a_failed_removal_still_offers_to_delete_a_directory_holding_only_ours() {
        let d = report_scratch("ours");
        let (tree, _) = tree_with_a_failing_file(&d, None, &[]);
        let r = tree::remove(&tree).expect("removed");
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert!(r.left.is_empty() && r.blocked.is_empty(), "{:?} {:?}", r.left, r.blocked);
        let lines = removal_report(&tree, &r);
        assert_eq!(
            lines.last().map(String::as_str),
            Some(format!("          it with `{}`", sys::remove_dir_command(&tree)).as_str()),
            "{:?}",
            lines
        );
        let text = lines.join("\n");
        assert!(!text.contains("do NOT delete"), "{:?}", lines);
        assert_eq!(text.contains("nothing is running"), !sys::HAS_UNLINK_RUNNING, "{:?}", lines);
        let _ = fs::remove_dir_all(&d);
    }

    /// The case that made this common: the binary a running hook holds, played by a
    /// handle that shares nothing, beside a file of somebody's.
    #[test]
    #[cfg(windows)]
    fn a_held_binary_beside_a_foreign_file_names_the_binary_not_the_directory() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = report_scratch("held");
        let tree = d.join("claude-tabstatus");
        let bin = tree::bin_path(&tree);
        fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
        fs::write(&bin, b"x").expect("write");
        fs::write(tree.join("NOTES.txt"), b"mine").expect("write");
        fs::write(tree::marker_path(&tree), tree::marker_text("0.1.0", "t", &[tree::BIN])).expect("write");
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(&bin).expect("open");
        let r = tree::remove(&tree).expect("removed");
        let text = removal_report(&tree, &r).join("\n");
        drop(held);
        assert!(!text.contains("Remove-Item"), "{}", text);
        assert!(text.contains("once nothing is running from the tree"), "{}", text);
        assert!(text.contains(&format!("            {}", bin.display())), "{}", text);
        let _ = fs::remove_dir_all(&d);
    }

    /// A generated tree of plain files its marker lists, and `extra` beside them - a
    /// file the tool did not write. Returns the tree and its generated files in the
    /// marker's order.
    fn left_tree(d: &Path, extra: Option<&str>) -> (PathBuf, Vec<PathBuf>) {
        let tree = d.join("claude-tabstatus");
        let ours = vec![tree::bin_path(&tree), tree.join("hooks").join("hooks.json")];
        for p in &ours {
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(p, b"x").expect("write");
        }
        if let Some(name) = extra {
            fs::write(tree.join(name), b"mine").expect("write");
        }
        fs::write(tree::marker_path(&tree), tree::marker_text("0.1.0", "t", &[tree::BIN, "hooks/hooks.json"]))
            .expect("write");
        (tree, ours)
    }

    /// One site that names a generated tree it LEAVES in place: the delete-the-directory
    /// command only for a tree holding nothing but what its marker lists, and otherwise
    /// our files by name, the marker last. Nothing is touched either way.
    fn a_tree_left_in_place_gets_the_command_only_when_it_holds_only_ours(
        tag: &str,
        lines_for: fn(&Path) -> Vec<String>,
    ) {
        let d = report_scratch(&format!("{}-ours", tag));
        let (tree, _) = left_tree(&d, None);
        let text = lines_for(&tree).join("\n");
        assert!(text.contains(&format!("`{}`", sys::remove_dir_command(&tree))), "{}", text);
        assert!(!text.contains("do NOT delete"), "{}", text);
        // removal_report's caveat: on Windows the binary may still be running from it.
        assert_eq!(text.contains("nothing is running from it, remove it with"), !sys::HAS_UNLINK_RUNNING, "{}", text);
        let _ = fs::remove_dir_all(&d);

        let d = report_scratch(&format!("{}-foreign", tag));
        let (tree, ours) = left_tree(&d, Some("NOTES.txt"));
        let lines = lines_for(&tree);
        let text = lines.join("\n");
        assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
        assert!(!text.contains("rm -rf") && !text.contains("Remove-Item"), "{}", text);
        assert!(text.contains("It also holds 1 file nothing here generated: NOTES.txt, so"), "{}", text);
        assert!(text.contains("do NOT delete the directory"), "{}", text);
        assert_eq!(text.contains("nothing is running"), !sys::HAS_UNLINK_RUNNING, "{}", text);
        let listed: Vec<String> = ours
            .iter()
            .chain([tree::marker_path(&tree)].iter())
            .map(|p| format!("            {}", p.display()))
            .collect();
        assert_eq!(lines[lines.len() - listed.len()..], listed[..], "{}", text);
        assert_eq!(fs::read(tree.join("NOTES.txt")).expect("kept"), b"mine");
        assert!(ours.iter().all(|p| p.is_file()) && tree::is_generated(&tree), "{}", text);
        let _ = fs::remove_dir_all(&d);

        // A tree that no longer has a marker never gets the command.
        let d = report_scratch(&format!("{}-unmarked", tag));
        let (tree, _) = left_tree(&d, None);
        fs::remove_file(tree::marker_path(&tree)).expect("rm");
        let text = lines_for(&tree).join("\n");
        assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
        assert!(text.contains("do NOT delete the directory"), "{}", text);
        let _ = fs::remove_dir_all(&d);
    }

    /// A tree the walk could not see all of gets no command either - here because it
    /// stopped at its bound, with nothing foreign among what it did see.
    #[test]
    fn a_tree_left_in_place_that_was_not_read_whole_gets_no_command() {
        let d = report_scratch("toomany");
        let (tree, ours) = left_tree(&d, None);
        for i in 0..4100 {
            fs::create_dir_all(tree.join("bulk").join(i.to_string())).expect("mkdir");
        }
        for lines_for in [moved_from_lines as fn(&Path) -> Vec<String>, kept_tree_lines, orphan_lines] {
            let lines = lines_for(&tree);
            let text = lines.join("\n");
            assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
            assert!(text.contains("It holds more entries than a generated tree ever does, so"), "{}", text);
            assert!(text.contains("do NOT delete the directory"), "{}", text);
            assert_eq!(lines.last(), Some(&format!("            {}", tree::marker_path(&tree).display())), "{}", text);
            assert!(ours.iter().all(|p| lines.contains(&format!("            {}", p.display()))), "{}", text);
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// A removal whose walk did not see the whole tree proves nothing by leaving
    /// `left` empty: no "only empty directories", no command - in the survival arm and
    /// after a failure alike.
    #[test]
    fn a_removal_that_did_not_read_the_whole_tree_never_offers_the_command() {
        let tree = PathBuf::from("/nowhere/claude-tabstatus");
        for unread in [tree::Unread::TooMany, tree::Unread::Unreadable] {
            let survived = tree::Removal {
                removed: 3,
                left: Vec::new(),
                blocked: Vec::new(),
                failed: Vec::new(),
                unread: Some(unread),
                dir_gone: false,
            };
            let text = removal_report(&tree, &survived).join("\n");
            assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
            assert!(!text.contains("only empty directories"), "{}", text);
            assert!(text.contains(unread.why()), "{}", text);
            assert!(text.contains("do NOT delete the directory"), "{}", text);

            let stuck = tree::bin_path(&tree);
            let failed = tree::Removal {
                removed: 2,
                left: Vec::new(),
                blocked: Vec::new(),
                failed: vec![(stuck.clone(), "in use".into())],
                unread: Some(unread),
                dir_gone: false,
            };
            let lines = removal_report(&tree, &failed);
            let text = lines.join("\n");
            assert!(!text.contains(&sys::remove_dir_command(&tree)), "{}", text);
            assert!(text.contains(&format!("wrote and will reuse. {}, so", unread.why())), "{}", text);
            assert!(text.contains("do NOT delete the directory"), "{}", text);
            assert_eq!(
                lines[lines.len() - 2..],
                [
                    format!("            {}", stuck.display()),
                    format!("            {}", tree::marker_path(&tree).display())
                ],
                "{}",
                text
            );
        }
    }

    /// install, moving the plugin to another tree: the hint about the OLD one.
    #[test]
    fn the_tree_install_moved_away_from_gets_the_command_only_when_it_holds_only_ours() {
        a_tree_left_in_place_gets_the_command_only_when_it_holds_only_ours("moved", moved_from_lines);
    }

    /// uninstall --keep-tree: the "remove it later" hint.
    #[test]
    fn a_kept_tree_gets_the_command_only_when_it_holds_only_ours() {
        a_tree_left_in_place_gets_the_command_only_when_it_holds_only_ours("kept", kept_tree_lines);
    }

    /// uninstall naming a generated tree that is not the live one.
    #[test]
    fn an_orphan_tree_gets_the_command_only_when_it_holds_only_ours() {
        a_tree_left_in_place_gets_the_command_only_when_it_holds_only_ours("orphan", orphan_lines);
    }

    #[test]
    fn the_target_triple_names_the_build_that_is_running() {
        let t = target_triple();
        assert!(t.contains('-'), "{}", t);
        assert_ne!(t, "unknown-target", "this crate is built for a named target");
    }

    /// A Ctx whose config directory is a fresh scratch directory, for the tests that
    /// drive the real settings steps.
    #[cfg(windows)]
    fn scratch_ctx(tag: &str) -> Ctx {
        let config = std::env::temp_dir().join(format!("cctab-acl-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&config);
        fs::create_dir_all(&config).expect("mkdir");
        Ctx {
            skills: config.join("skills"),
            link: config.join("skills").join(PLUGIN),
            settings: config.join("settings.json"),
            state: config.join(format!("{}.state", PLUGIN)),
            backup: config.join("settings.json.cctab-preinstall"),
            safety: config.join("settings.json.cctab-preuninstall"),
            config,
            ..ctx()
        }
    }

    #[cfg(windows)]
    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// Install's edit, uninstall's edit and `--restore-backup`, on a settings.json
    /// with an ACL of its own - one hardened by hand (inheritance off, Administrators
    /// removed, Guests denied), and one that still inherits but carries an explicit
    /// deny. Every rewrite keeps the file's DACL exactly, flags and all, and every
    /// backup is born with the same one: the backups hold the same secrets.
    #[test]
    #[cfg(windows)]
    fn every_settings_write_and_backup_keeps_the_files_own_acl() {
        use crate::sys::test_acl::{harden, sddl, set_sddl};
        for kind in ["protected", "explicit"] {
            let c = scratch_ctx(kind);
            fs::write(&c.settings, b"{\"env\": {\"ANTHROPIC_API_KEY\": \"sk-x\"}}\n").expect("write");
            if kind == "protected" {
                harden(&c.settings);
            } else {
                set_sddl(&c.settings, "D:(D;;FR;;;BG)");
            }
            let acl = sddl(&c.settings);
            assert!(acl.contains("(D;;FR;;;BG)"), "{kind}: {acl}");
            assert_eq!(acl.starts_with("D:P"), kind == "protected", "{acl}");
            assert_eq!(acl.contains(";ID;"), kind == "explicit", "{acl}");
            refuse_unreadable_acl(&c.settings, false).expect("readable");

            // install
            let doc = fs::read(&c.settings).expect("read");
            write_env_key(&c, Some(doc), false).expect("install's edit");
            let now = fs::read(&c.settings).expect("read");
            assert!(settings::env_raw_text(&now, KEY).expect("parse").is_some());
            assert_eq!(sddl(&c.settings), acl, "{kind}: settings.json after install");
            assert_eq!(sddl(&c.backup), acl, "{kind}: the pre-install backup");

            // uninstall, removing the key
            let prior = Prior { blank_settings: false, state: None };
            remove_env_key(&c, &prior, true, false).expect("uninstall's edit");
            let now = fs::read(&c.settings).expect("read");
            assert!(settings::env_raw_text(&now, KEY).expect("parse").is_none());
            assert_eq!(sddl(&c.settings), acl, "{kind}: settings.json after uninstall");
            assert_eq!(sddl(&c.safety), acl, "{kind}: the pre-uninstall backup");

            // --restore-backup over a live file keeps the LIVE file's ACL, even where
            // the backup's has since become a different one.
            // (Another deny, on the entries this process already has: a DACL that drops
            // them can leave a sandboxed test unable to rename the file it made.)
            let other = match acl.find('(') {
                Some(i) if kind == "protected" => format!("{}(D;;FW;;;AN){}", &acl[..i], &acl[i..]),
                _ => "D:(D;;FW;;;AN)".to_string(),
            };
            set_sddl(&c.backup, &other);
            remove_env_key(&c, &prior, false, true).expect("restore");
            assert_eq!(fs::read(&c.settings).expect("read"), fs::read(&c.backup).expect("read"));
            assert_eq!(sddl(&c.settings), acl, "{kind}: settings.json after a restore");
            assert_eq!(sddl(&c.safety), acl, "{kind}: the copy a restore saves");

            // ... and with no live file, the restored one takes the backup's.
            let backup_acl = sddl(&c.backup);
            assert_ne!(backup_acl, acl);
            fs::remove_file(&c.settings).expect("rm");
            remove_env_key(&c, &prior, false, true).expect("restore with no live file");
            assert_eq!(sddl(&c.settings), backup_acl, "{kind}: restored with no live file");

            let want = ["settings.json", "settings.json.cctab-preinstall", "settings.json.cctab-preuninstall"];
            assert_eq!(names_in(&c.config), want, "no temp file left behind");
            let _ = fs::remove_dir_all(&c.config);
        }
    }

    /// No ACL of its own: the rewritten file still inherits, exactly as before - and a
    /// settings.json created where there was none gets its directory's ACL, as it
    /// always did.
    #[test]
    #[cfg(windows)]
    fn a_settings_file_that_inherits_still_inherits_and_a_new_one_is_unchanged() {
        use crate::sys::test_acl::sddl;
        let c = scratch_ctx("inherits");
        let plain = c.config.join("plain");
        fs::write(&plain, b"x").expect("write");
        let inherited = sddl(&plain);
        fs::remove_file(&plain).expect("rm");
        assert!(!inherited.starts_with("D:P") && inherited.contains(";ID;"), "{inherited}");
        assert!(!inherited.contains("(A;;") && !inherited.contains("(D;;"), "nothing explicit: {inherited}");

        write_env_key(&c, None, false).expect("created");
        assert_eq!(sddl(&c.settings), inherited, "a new settings.json");
        fs::write(&c.settings, b"{}").expect("write");
        write_env_key(&c, Some(b"{}".to_vec()), false).expect("edited");
        assert_eq!(sddl(&c.settings), inherited, "an edited one");
        assert_eq!(sddl(&c.backup), inherited, "its backup");
        let _ = fs::remove_dir_all(&c.config);
    }

    /// An ACL this user may not read is refused - by the preflight, saying nothing
    /// was changed, and by the write and the backup themselves, which then write
    /// nothing - rather than replaced with the directory's.
    #[test]
    #[cfg(windows)]
    fn a_settings_acl_that_cannot_be_read_refuses_the_write() {
        use crate::sys::test_acl::set_sddl;
        let c = scratch_ctx("unreadable");
        fs::write(&c.settings, b"{}").expect("write");
        // OWNER RIGHTS replaces the owner's implicit READ_CONTROL and WRITE_DAC: this
        // process may read the data and nothing else. The directory's delete-child
        // right still lets the cleanup remove it.
        set_sddl(&c.settings, "D:P(A;;0x1;;;OW)");
        let e = refuse_unreadable_acl(&c.settings, false).expect_err("refused");
        assert!(e.contains("access control list") && e.ends_with("Nothing has been changed."), "{e}");
        let e = write_settings(&c.settings, b"{\"x\": 1}", 0o600, &c.settings, false).expect_err("refused");
        assert!(e.contains("was not changed"), "{e}");
        let e = copy_settings(&c.settings, &c.backup, "the backup ", false).expect_err("refused");
        assert!(e.contains("was not written"), "{e}");
        assert_eq!(fs::metadata(&c.settings).expect("stat").len(), 2, "untouched");
        assert_eq!(names_in(&c.config), ["settings.json"], "nothing written, nothing left");
        let _ = fs::remove_dir_all(&c.config);
    }

    /// A settings.json moved in from a directory that gave it a deny keeps that
    /// INHERITED entry through a rewrite and in its backup: the DACL is carried as it
    /// was, not recomputed from the directory it now sits in.
    #[test]
    #[cfg(windows)]
    fn a_moved_in_settings_file_keeps_the_entries_it_inherited_elsewhere() {
        use crate::sys::test_acl::{sddl, set_sddl};
        let c = scratch_ctx("moved");
        let elsewhere = c.config.join("elsewhere");
        fs::create_dir(&elsewhere).expect("mkdir");
        set_sddl(&elsewhere, "D:(D;OICI;FR;;;BG)");
        let born = elsewhere.join("settings.json");
        fs::write(&born, b"{}").expect("write");
        fs::rename(&born, &c.settings).expect("move");
        fs::remove_dir(&elsewhere).expect("rmdir");
        let acl = sddl(&c.settings);
        assert!(acl.starts_with("D:AI") && acl.contains("(D;ID;FR;;;BG)"), "{acl}");

        write_env_key(&c, Some(b"{}".to_vec()), false).expect("install's edit");
        assert_eq!(sddl(&c.settings), acl, "settings.json");
        assert_eq!(sddl(&c.backup), acl, "its backup");
        let _ = fs::remove_dir_all(&c.config);
    }

    /// A filesystem that keeps no Windows ACL (a WSL share: `security_of` answers
    /// `Unsupported`) is refused for what it is, not as an ACL this user cannot read.
    #[test]
    fn a_filesystem_with_no_acl_is_refused_for_what_it_is() {
        let e = std::io::Error::new(std::io::ErrorKind::Unsupported, std::io::Error::from_raw_os_error(1));
        let m = acl_unreadable(Path::new("settings.json"), &e, "Nothing has been changed");
        assert_eq!(m.starts_with("settings.json is on a filesystem that keeps no Windows access control list"), sys::CAN_FORCE_ACL, "{m}");
        assert!(m.ends_with("Nothing has been changed."), "{m}");
        assert_eq!(m.contains("re-run with --force"), sys::CAN_FORCE_ACL, "{m}");
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let m = acl_unreadable(Path::new("settings.json"), &e, "Nothing has been changed");
        assert!(m.starts_with("cannot read the access control list of settings.json"), "{m}");
        assert!(!m.contains("--force"), "an unreadable ACL is not something --force lifts: {m}");
        assert!(!no_acl_here(&e));
        assert_eq!(no_acl_here(&std::io::Error::from(std::io::ErrorKind::Unsupported)), sys::CAN_FORCE_ACL);
    }

    /// `--force` lifts the no-ACL refusal, and only it. Needs a settings.json on a
    /// filesystem with no Windows ACL, so run by hand:
    /// `CCTAB_TEST_NO_ACL_DIR=\\wsl.localhost\<distro>\tmp\<dir> cargo test -- --ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a directory on a WSL share, named by CCTAB_TEST_NO_ACL_DIR"]
    fn force_writes_settings_on_a_filesystem_with_no_acl() {
        let d = PathBuf::from(std::env::var_os("CCTAB_TEST_NO_ACL_DIR").expect("CCTAB_TEST_NO_ACL_DIR"));
        let settings = d.join("settings.json");
        let backup = d.join("settings.json.cctab-preinstall");
        fs::write(&settings, b"{}").expect("writable share");
        let _ = fs::remove_file(&backup);
        assert!(refuse_unreadable_acl(&settings, false).is_err(), "refused without --force");
        assert!(write_settings(&settings, b"{\"a\":1}", 0o600, &settings, false).is_err());
        assert_eq!(fs::read(&settings).expect("still there"), b"{}", "nothing changed when refused");
        refuse_unreadable_acl(&settings, true).expect("--force lets it through");
        copy_settings(&settings, &backup, "the backup ", true).expect("backup written");
        write_settings(&settings, b"{\"a\":1}", 0o600, &settings, true).expect("written");
        assert_eq!(fs::read(&settings).expect("read"), b"{\"a\":1}");
        assert_eq!(fs::read(&backup).expect("read"), b"{}");
        let _ = fs::remove_file(&backup);
        let _ = fs::remove_file(&settings);
    }
}
