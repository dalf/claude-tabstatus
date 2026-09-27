//! The two manifests Claude Code needs on disk, compiled into the binary.
//!
//! Claude Code will not load a plugin that is not a directory with
//! `.claude-plugin/plugin.json` and `hooks/hooks.json` in it. Those two files are
//! SOURCE - tracked files you read, diff and edit - and they reach a running session
//! the way `src/main.rs` does: through a build. They are carried HERE, and `install`
//! writes them back out into a directory it generates (`src/tree.rs`), so the plugin
//! directory is build output like `bin/` and a `git checkout` cannot change what a
//! running session executes. The same two files being compiled in is also what makes
//! a remote install one scp and one run.
//!
//! `include_str!`, not a crate. It is a std macro, it costs no dependency, and it
//! is the direct analogue of Go's `//go:embed`: the asset becomes a rustc rebuild
//! INPUT, so a binary produced by `cargo build` cannot carry a copy that
//! disagrees with the tree it was built from. That property is the whole reason a
//! second copy of a version-controlled file is acceptable at all, and it is
//! verified rather than trusted - `target/*/release/tabstatus.d` lists both JSON
//! files, and editing only the JSON re-triggers a compile.
//!
//! `rust-embed` and `include_dir` solve a different problem: globbing an asset
//! TREE and iterating it at runtime. Both are proc-macro crates, and here the
//! asset set is two files with fixed paths enumerated in one place. A glob would
//! buy nothing and cost the one thing `Cargo.toml` says is not negotiable.
//!
//! `include_str!` rather than `include_bytes!` because both files are UTF-8 JSON
//! and a `&str` makes the comparison and the write trivially the same bytes. If
//! either ever gains a non-UTF-8 byte the BUILD fails, which is the right place
//! for that to happen.
//!
//! Three layers keep the copies honest, and this module is layer 3's only code:
//! layer 1 is the rebuild dependency above, layer 2 adds both paths to
//! `bin/sources.sha256` (so a PREBUILT release asset cannot go stale against an
//! edited hooks.json), and layer 3 is `tabstatus print-embedded`, which streams
//! these bytes so the test suite can `diff` them against the files and `doctor`
//! can report on a machine that has no source tree at all.

use std::path::Path;

pub const PLUGIN_JSON_PATH: &str = ".claude-plugin/plugin.json";
pub const HOOKS_JSON_PATH: &str = "hooks/hooks.json";

pub const PLUGIN_JSON: &str = include_str!("../.claude-plugin/plugin.json");
pub const HOOKS_JSON: &str = include_str!("../hooks/hooks.json");

/// Every manifest a generated tree needs, as `(relative path, contents)`. The
/// single enumeration: `install` writes this list, `doctor` compares it, and the
/// marker records it.
pub const MANIFESTS: [(&str, &str); 2] =
    [(PLUGIN_JSON_PATH, PLUGIN_JSON), (HOOKS_JSON_PATH, HOOKS_JSON)];

/// The word `print-embedded` takes. Short names because the point is a test
/// assertion and a human spot-check, and the full relative path because that is
/// what `doctor` prints and what somebody will paste back.
pub fn by_name(name: &[u8]) -> Option<(&'static str, &'static str)> {
    match name {
        b"plugin" | b"plugin.json" | b".claude-plugin/plugin.json" => Some(MANIFESTS[0]),
        b"hooks" | b"hooks.json" | b"hooks/hooks.json" => Some(MANIFESTS[1]),
        _ => None,
    }
}

/// The names `print-embedded` accepts, for the refusal message.
pub const NAMES: &str = "plugin | hooks";

/// One manifest on disk, against the copy compiled in.
///
/// Deliberately not a `bool`: the three failures want three different sentences,
/// and "differs" wants the two lengths, because that is the one number that makes
/// a drift report actionable without a diff.
pub enum Verdict {
    Same,
    Differs { on_disk: usize, embedded: usize },
    Missing,
    Unreadable(String),
}

/// How a [`Verdict::Differs`] reads. Two byte counts are the one detail that makes
/// a drift report actionable without a diff - except when they are the SAME number,
/// which is the commonest case of all: a plain version bump keeps a manifest's
/// length, so `(438 vs 438 bytes)` reads as a false positive rather than as the
/// answer. Same distinction `doctor` already drew for the binary comparison, now in
/// the one place both spellings come from.
pub fn differs_phrase(on_disk: usize, embedded: usize) -> String {
    if on_disk == embedded {
        format!("same {} bytes, different content", on_disk)
    } else {
        format!("{} vs {} bytes", on_disk, embedded)
    }
}

/// Compare one manifest under `root` with the copy compiled in. Byte equality,
/// not JSON equality: a reformatted hooks.json is a file the user changed and
/// wants to hear about, and the whole point of this comparison is that the
/// embedded copy is a byte-for-byte projection.
pub fn compare(root: &Path, rel: &str, embedded: &str) -> Verdict {
    let p = root.join(rel);
    match std::fs::read(&p) {
        Ok(bytes) => {
            if bytes == embedded.as_bytes() {
                Verdict::Same
            } else {
                Verdict::Differs { on_disk: bytes.len(), embedded: embedded.len() }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Verdict::Missing,
        Err(e) => Verdict::Unreadable(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    #[test]
    fn both_manifests_are_embedded_and_parse_as_json() {
        for (rel, text) in MANIFESTS {
            assert!(!text.is_empty(), "{} is empty", rel);
            json::parse(text.as_bytes()).unwrap_or_else(|e| panic!("{} does not parse: {}", rel, e));
        }
    }

    /// The version in plugin.json is what Claude Code shows, and the version in
    /// Cargo.toml is what `tabstatus version` prints. A bump that touched only one
    /// of them is a drift this catches at `cargo test` time, before either copy
    /// reaches a tree.
    #[test]
    fn the_embedded_plugin_manifest_agrees_with_cargo() {
        let v = json::parse(PLUGIN_JSON.as_bytes()).expect("valid JSON");
        let o = v.as_obj().expect("an object at the top");
        assert_eq!(
            o.get("name").and_then(|m| m.val.as_str()),
            Some(&b"claude-tabstatus"[..]),
            "plugin.json must name the plugin the installer links"
        );
        assert_eq!(
            o.get("version").and_then(|m| m.val.as_str()),
            Some(env!("CARGO_PKG_VERSION").as_bytes()),
            "plugin.json version and Cargo.toml version disagree"
        );
    }

    /// The embedded hooks.json has to be the wiring `tests/run.sh` pins, and the
    /// one thing that can be checked without reparsing the table is that every
    /// edge is invoked through `${CLAUDE_PLUGIN_ROOT}/bin/tabstatus` - the path
    /// `install` materialises.
    #[test]
    fn the_embedded_hooks_invoke_the_path_a_generated_tree_provides() {
        let n = HOOKS_JSON.matches("${CLAUDE_PLUGIN_ROOT}/bin/tabstatus").count();
        assert_eq!(n, 11, "eleven hook edges, all through the plugin root");
    }

    #[test]
    fn every_spelling_of_a_manifest_name_resolves_and_nothing_else_does() {
        for n in [&b"plugin"[..], b"plugin.json", b".claude-plugin/plugin.json"] {
            assert_eq!(by_name(n).map(|(r, _)| r), Some(PLUGIN_JSON_PATH), "{:?}", n);
        }
        for n in [&b"hooks"[..], b"hooks.json", b"hooks/hooks.json"] {
            assert_eq!(by_name(n).map(|(r, _)| r), Some(HOOKS_JSON_PATH), "{:?}", n);
        }
        for n in [&b""[..], b"Hooks", b"hooks.JSON", b"settings.json", b"bin/tabstatus"] {
            assert!(by_name(n).is_none(), "{:?} must not resolve", n);
        }
    }

    /// The report a bumped version produces. 0.1.0 and 0.2.0 are the same length, so
    /// the commonest real drift is the one where two byte counts say nothing.
    #[test]
    fn a_difference_with_no_size_difference_says_so_instead_of_repeating_the_number() {
        assert_eq!(differs_phrase(438, 438), "same 438 bytes, different content");
        assert_eq!(differs_phrase(2842, 3068), "2842 vs 3068 bytes");
        assert_eq!(differs_phrase(0, 438), "0 vs 438 bytes");
    }

    #[test]
    fn compare_tells_the_three_failures_apart() {
        let dir = std::env::temp_dir().join(format!("cctab-emb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("hooks")).expect("a scratch tree");

        assert!(matches!(compare(&dir, HOOKS_JSON_PATH, HOOKS_JSON), Verdict::Missing));

        std::fs::write(dir.join(HOOKS_JSON_PATH), HOOKS_JSON).expect("write");
        assert!(matches!(compare(&dir, HOOKS_JSON_PATH, HOOKS_JSON), Verdict::Same));

        // One byte, and it is caught: this is a byte comparison, not a JSON one.
        std::fs::write(dir.join(HOOKS_JSON_PATH), format!("{} ", HOOKS_JSON)).expect("write");
        match compare(&dir, HOOKS_JSON_PATH, HOOKS_JSON) {
            Verdict::Differs { on_disk, embedded } => {
                assert_eq!(on_disk, HOOKS_JSON.len() + 1);
                assert_eq!(embedded, HOOKS_JSON.len());
            }
            _ => panic!("a trailing space is a difference"),
        }

        // A directory where the file goes is neither missing nor different.
        std::fs::remove_file(dir.join(HOOKS_JSON_PATH)).expect("rm");
        std::fs::create_dir(dir.join(HOOKS_JSON_PATH)).expect("mkdir");
        assert!(matches!(compare(&dir, HOOKS_JSON_PATH, HOOKS_JSON), Verdict::Unreadable(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
