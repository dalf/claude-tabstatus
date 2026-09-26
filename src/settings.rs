//! The one edit this tool makes to `settings.json`, done as a TEXT SPLICE.
//!
//! The shell installer merged with jq, which reprints the whole document: a
//! hand-formatted `settings.json` came back reindented, and the installer had
//! to warn about it. Here the document is parsed only to find out WHERE the
//! member goes, and then the member's text is spliced in. Every other byte -
//! key order, indentation, blank lines, a `\/` escape someone's editor wrote -
//! is preserved literally, which is the strongest form of "preserving every
//! other key" available.
//!
//! Every edit is checked twice before it is offered to the caller:
//!
//!   1. the spliced text must parse again, and
//!   2. with our one key removed from both documents, the parse trees must be
//!      structurally identical - same keys, same order, same values. That is
//!      the clobber guard, and it is what makes a splice as safe as a reprint.

use crate::json::{self, Member, Obj, J};

pub enum Outcome {
    /// The file already says what we want it to say.
    Unchanged,
    Changed { text: Vec<u8>, before: Before },
}

/// What was in the file before, so that `uninstall` restores rather than
/// deletes. `raw` is the value's ORIGINAL TEXT, so restoring is byte-exact.
pub struct Before {
    pub had: bool,
    pub raw: Option<Vec<u8>>,
}

/// The first key that appears twice in `obj`, if any.
///
/// Duplicate members are legal to WRITE and ambiguous to READ: this parser keeps
/// them all and resolves first-wins, while every JSON reader that matters here -
/// JavaScript's `JSON.parse`, hence Claude Code - resolves last-wins. With two
/// `env` members, install spliced its key into the FIRST one, reported success
/// and exited 0, but the key was not in the effective environment: the plugin
/// installed and Claude Code's built-in title went on repainting over it. A
/// silent false success is the worst outcome available, so a document this tool
/// must round-trip and whose meaning it cannot agree on with its reader is
/// refused instead.
fn first_duplicate(obj: &Obj) -> Option<Vec<u8>> {
    for (i, m) in obj.members.iter().enumerate() {
        if obj.members[..i].iter().any(|n| n.key == m.key) {
            return Some(m.key.clone());
        }
    }
    None
}

fn refuse_duplicate(key: &[u8], whose: &str) -> String {
    format!(
        "{} appears more than once {}. JSON readers disagree about which one wins \
         - this tool resolves the first, Claude Code resolves the last - so \
         editing the file could set a key that is never read. Remove the duplicate \
         yourself, then re-run.",
        String::from_utf8_lossy(key),
        whose
    )
}

fn root_of(doc: &[u8]) -> Result<J, String> {
    let v = json::parse(doc)?;
    match v.as_obj() {
        Some(o) => match first_duplicate(o) {
            Some(k) => Err(refuse_duplicate(&k, "at the top level")),
            None => Ok(v),
        },
        None => Err("the top level is not a JSON object".to_string()),
    }
}

/// The `env` member, checked for shape. `Ok(None)` means there is no `env` key,
/// which is fine; an `env` that is not an object is a refusal, because merging
/// into it would produce something Claude Code cannot read.
fn env_of(root: &Obj) -> Result<Option<&Member>, String> {
    match root.get("env") {
        None => Ok(None),
        Some(m) => match &m.val {
            J::Obj(env) => match first_duplicate(env) {
                Some(k) => Err(refuse_duplicate(&k, "inside \"env\"")),
                None => Ok(Some(m)),
            },
            _ => Err("\"env\" is present but is not a JSON object".to_string()),
        },
    }
}

/// Byte span of the whitespace at the start of the line containing `pos`.
fn line_indent(doc: &[u8], pos: usize) -> &[u8] {
    let bol = doc[..pos].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let end = doc[bol..pos]
        .iter()
        .position(|&b| b != b' ' && b != b'\t')
        .map_or(pos, |k| bol + k);
    &doc[bol..end]
}

/// The document's indentation step, taken from the first top-level member's own
/// indent, defaulting to two spaces. Only ever used when a brand-new object has
/// to be formatted from nothing.
fn indent_unit(doc: &[u8], root: &Obj) -> Vec<u8> {
    if let Some(m) = root.members.first() {
        let ind = line_indent(doc, m.start);
        if !ind.is_empty() {
            return ind.to_vec();
        }
    }
    b"  ".to_vec()
}

/// Insert `member` as the FIRST member of `obj`, matching the layout already
/// there: the whitespace between `{` and the existing first key is reused, so
/// the new member lands at the same indent on its own line when the file is
/// pretty-printed and inline when it is not.
fn insert_first(doc: &[u8], obj: &Obj, member: &[u8], unit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(doc.len() + member.len() + 8);
    out.extend_from_slice(&doc[..obj.open + 1]);
    match obj.members.first() {
        Some(first) => {
            let lead = &doc[obj.open + 1..first.start];
            out.extend_from_slice(lead);
            out.extend_from_slice(member);
            out.push(b',');
            out.extend_from_slice(lead);
            out.extend_from_slice(&doc[first.start..]);
        }
        None => {
            // An empty object, `{}` or `{ }`: invent a layout from the
            // surrounding line's indent.
            let base = line_indent(doc, obj.open).to_vec();
            out.push(b'\n');
            out.extend_from_slice(&base);
            out.extend_from_slice(unit);
            out.extend_from_slice(member);
            out.push(b'\n');
            out.extend_from_slice(&base);
            out.extend_from_slice(&doc[obj.close..]);
        }
    }
    out
}

/// Remove member `i` of `obj`, taking exactly one comma with it and leaving the
/// surrounding layout alone.
fn remove_member(doc: &[u8], obj: &Obj, i: usize) -> Vec<u8> {
    let m = &obj.members[i];
    let n = obj.members.len();
    let (from, to) = if n == 1 {
        // The last member: collapse the object to `{}` rather than leave a
        // blank line inside empty braces.
        (obj.open + 1, obj.close)
    } else if i + 1 < n {
        // Eat forward through the comma to the next key.
        (m.start, obj.members[i + 1].start)
    } else {
        // The last of several: eat backward from the previous value, which
        // takes the comma that preceded us.
        (obj.members[i - 1].end, m.end)
    };
    let mut out = Vec::with_capacity(doc.len());
    out.extend_from_slice(&doc[..from]);
    out.extend_from_slice(&doc[to..]);
    out
}

fn replace_span(doc: &[u8], from: usize, to: usize, with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(doc.len() + with.len());
    out.extend_from_slice(&doc[..from]);
    out.extend_from_slice(with);
    out.extend_from_slice(&doc[to..]);
    out
}

/// Strip our one key from a parse tree, and the whole `env` object with it when
/// that leaves it empty, so that the two sides of the clobber guard compare
/// equal whether or not the installer created `env`.
fn normalized(doc: &[u8], key: &str) -> Result<J, String> {
    let mut v = json::parse(doc)?;
    if let J::Obj(root) = &mut v {
        if let Some(ei) = root.index_of("env") {
            let mut drop_env = false;
            if let J::Obj(env) = &mut root.members[ei].val {
                if let Some(ki) = env.index_of(key) {
                    env.members.remove(ki);
                }
                drop_env = env.members.is_empty();
            }
            if drop_env {
                root.members.remove(ei);
            }
        }
    }
    Ok(v)
}

/// The guard both public entry points run before they hand anything back.
fn verify(old: &[u8], new: &[u8], key: &str) -> Result<(), String> {
    let a = normalized(old, key)?;
    let b = normalized(new, key).map_err(|e| format!("the edited file would not parse: {}", e))?;
    if !json::same(&a, &b) {
        return Err("the edit would have changed something other than that one key".to_string());
    }
    Ok(())
}

/// `env.<key> = "<value>"`, adding `env` if it is not there.
pub fn set_env_key(doc: &[u8], key: &str, value: &str) -> Result<Outcome, String> {
    let rv = root_of(doc)?;
    let root = rv.as_obj().unwrap();
    let want = format!("{}: {}", json::quote(key.as_bytes()), json::quote(value.as_bytes()));
    let (text, before) = match env_of(root)? {
        None => {
            let unit = indent_unit(doc, root);
            let base = line_indent(doc, root.open).to_vec();
            let mut inner = Vec::new();
            inner.extend_from_slice(json::quote(b"env").as_bytes());
            inner.extend_from_slice(b": {\n");
            inner.extend_from_slice(&base);
            inner.extend_from_slice(&unit);
            inner.extend_from_slice(&unit);
            inner.extend_from_slice(want.as_bytes());
            inner.push(b'\n');
            inner.extend_from_slice(&base);
            inner.extend_from_slice(&unit);
            inner.push(b'}');
            (
                insert_first(doc, root, &inner, &unit),
                Before { had: false, raw: None },
            )
        }
        Some(envm) => {
            let env = envm.val.as_obj().unwrap();
            match env.index_of(key) {
                None => (
                    insert_first(doc, env, want.as_bytes(), &indent_unit(doc, root)),
                    Before { had: false, raw: None },
                ),
                Some(ki) => {
                    let m = &env.members[ki];
                    let raw = doc[m.val_start..m.end].to_vec();
                    if m.val.as_str() == Some(value.as_bytes()) {
                        return Ok(Outcome::Unchanged);
                    }
                    (
                        replace_span(doc, m.val_start, m.end, json::quote(value.as_bytes()).as_bytes()),
                        Before { had: true, raw: Some(raw) },
                    )
                }
            }
        }
    };
    verify(doc, &text, key)?;
    Ok(Outcome::Changed { text, before })
}

/// Undo `set_env_key`: either put back the exact text that was there, or remove
/// the key - and the `env` object with it when that leaves it empty, so nothing
/// the installer created is left behind.
///
/// `env_was_there` is the state file's record of whether `env` existed BEFORE the
/// install. Without it this function cannot tell an `env` it created from an
/// empty one the user already had, and a differential fuzz found exactly that:
/// a settings.json holding `"env": {}` came back without it. `false` means
/// "install created it, so take it away"; `true` means leave the empty object
/// alone.
pub fn restore_env_key(
    doc: &[u8],
    key: &str,
    raw: Option<&[u8]>,
    env_was_there: bool,
) -> Result<Outcome, String> {
    let rv = root_of(doc)?;
    let root = rv.as_obj().unwrap();
    let envm = match env_of(root)? {
        None => {
            // Nothing to remove. A recorded value has to go back even so.
            return match raw {
                None => Ok(Outcome::Unchanged),
                Some(raw) => {
                    let unit = indent_unit(doc, root);
                    let base = line_indent(doc, root.open).to_vec();
                    let mut inner = Vec::new();
                    inner.extend_from_slice(json::quote(b"env").as_bytes());
                    inner.extend_from_slice(b": {\n");
                    inner.extend_from_slice(&base);
                    inner.extend_from_slice(&unit);
                    inner.extend_from_slice(&unit);
                    inner.extend_from_slice(json::quote(key.as_bytes()).as_bytes());
                    inner.extend_from_slice(b": ");
                    inner.extend_from_slice(raw);
                    inner.push(b'\n');
                    inner.extend_from_slice(&base);
                    inner.extend_from_slice(&unit);
                    inner.push(b'}');
                    let text = insert_first(doc, root, &inner, &unit);
                    verify(doc, &text, key)?;
                    Ok(Outcome::Changed { text, before: Before { had: false, raw: None } })
                }
            };
        }
        Some(m) => m,
    };
    let env = envm.val.as_obj().unwrap();
    let ki = env.index_of(key);
    let text = match (ki, raw) {
        (None, None) => return Ok(Outcome::Unchanged),
        (None, Some(raw)) => {
            let mut member = Vec::new();
            member.extend_from_slice(json::quote(key.as_bytes()).as_bytes());
            member.extend_from_slice(b": ");
            member.extend_from_slice(raw);
            insert_first(doc, env, &member, &indent_unit(doc, root))
        }
        (Some(ki), Some(raw)) => {
            let m = &env.members[ki];
            if &doc[m.val_start..m.end] == raw {
                return Ok(Outcome::Unchanged);
            }
            replace_span(doc, m.val_start, m.end, raw)
        }
        (Some(ki), None) => {
            if env.members.len() == 1 && !env_was_there {
                // Removing the only key from an `env` the installer created:
                // take the whole member out, so no empty object is left behind.
                let ri = root.index_of("env").unwrap();
                remove_member(doc, root, ri)
            } else {
                remove_member(doc, env, ki)
            }
        }
    };
    verify(doc, &text, key)?;
    Ok(Outcome::Changed { text, before: Before { had: ki.is_some(), raw: None } })
}

/// `.env[key]`'s value as a string, for `doctor` and for the uninstall guard.
pub fn env_value(doc: &[u8], key: &str) -> Result<Option<Vec<u8>>, String> {
    let rv = root_of(doc)?;
    let root = rv.as_obj().unwrap();
    Ok(match env_of(root)? {
        None => None,
        Some(m) => m
            .val
            .as_obj()
            .unwrap()
            .get(key)
            .map(|k| k.val.as_str().map(|s| s.to_vec()).unwrap_or_default()),
    })
}

/// The raw TEXT of `.env[key]`'s value, or `None` when the key is absent. The
/// state record keeps this rather than a decoded value, so that `uninstall`
/// restores the user's own byte sequence and not a re-encoding of it.
pub fn env_raw_text(doc: &[u8], key: &str) -> Result<Option<Vec<u8>>, String> {
    let rv = root_of(doc)?;
    let root = rv.as_obj().unwrap();
    Ok(match env_of(root)? {
        None => None,
        Some(m) => m
            .val
            .as_obj()
            .unwrap()
            .get(key)
            .map(|k| doc[k.val_start..k.end].to_vec()),
    })
}

/// Whether the document already has an `env` object, for the state record.
pub fn has_env_object(doc: &[u8]) -> Result<bool, String> {
    let rv = root_of(doc)?;
    Ok(env_of(rv.as_obj().unwrap())?.is_some())
}
