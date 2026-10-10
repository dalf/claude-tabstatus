//! Cold, session-wide lifecycle coordination. Pane titles are asynchronous
//! display transport; they cannot be the membership register for new owners.

use super::{ask, capture, command, digits, Tmux, IS};
use crate::{config::Config, edge::Paint, payload::Payload, sys};
use serde_json::{json, Map, Value};
use std::{fs, io, path::Path};

pub(super) const MEMBERS: &str = "@cctab_members";

/// Held from before membership/policy selection through the last client and
/// carrier write. Never unlink the anchor: queued lockers must share one inode.
pub struct Lifecycle<'a> {
    tmux: &'a Tmux,
    others: bool,
    eligible: bool,
}

impl Lifecycle<'_> {
    /// A missing server preserves the historical direct-only fallback. A live
    /// server whose coordination cannot be established fails closed instead.
    pub fn begin<'a>(
        cfg: &'a Config,
        payload: &Payload,
        paint: Paint,
    ) -> io::Result<Option<Lifecycle<'a>>> {
        let Some(t) = cfg.stack.tmux() else {
            return Ok(None);
        };
        let fields = match ask(
            t,
            &[
                "#{socket_path}",
                "#{session_id}",
                "#{pane_id}",
                "#{pane_pid}",
            ],
        ) {
            Ok(fields) => fields,
            Err(_) => {
                let raw = std::env::var_os("TMUX").unwrap_or_default();
                let socket = raw
                    .as_encoded_bytes()
                    .split(|b| *b == b',')
                    .next()
                    .unwrap_or_default();
                if !Path::new(&sys::os_str_from_bytes(socket)).exists() {
                    return Ok(None);
                }
                return Err(io::Error::other("tmux lifecycle target unavailable"));
            }
        };
        let [socket, session, pane, pane_pid] = fields.as_slice() else {
            return Err(io::Error::other("invalid tmux lifecycle target"));
        };
        if !valid_id(session, '$') || !valid_id(pane, '%') || !Path::new(socket).is_absolute() {
            return Err(io::Error::other("invalid tmux lifecycle identity"));
        }
        let socket = Path::new(socket);
        let mut name = socket
            .file_name()
            .ok_or_else(|| io::Error::other("missing socket name"))?
            .to_owned();
        name.push(format!(".cctab-{}.lock", &session[1..]));
        let anchor = socket.with_file_name(name);
        // The socket's own directory supplies the shared location, independent
        // of HOME, state directories, binary paths and the spelling of $TMUX.
        let mut opts = fs::OpenOptions::new();
        match fs::symlink_metadata(&anchor) {
            Ok(m) if !m.is_file() => return Err(io::Error::other("invalid tmux lifecycle anchor")),
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        opts.read(true).write(true).create(true).truncate(false);
        let file = sys::with_mode(&mut opts, 0o600).open(&anchor)?;
        sys::lock_exclusive(&file)?;
        let meta = fs::symlink_metadata(&anchor)?;
        let identity =
            sys::file_id_of(&file).ok_or_else(|| io::Error::other("unknown lock identity"))?;
        if !meta.is_file() || Some(identity) != sys::file_id_at(&anchor, &meta) {
            return Err(io::Error::other("tmux lifecycle anchor changed"));
        }
        *t.lifecycle_lock.borrow_mut() = Some(file);
        let mut held = Lifecycle {
            tmux: t,
            others: false,
            eligible: true,
        };
        // Use the actual session ID throughout the membership read/write. Pane
        // movement during a lifecycle remains a topology change, not migration.
        let target = Tmux {
            pane: Some(session.into()),
            lifecycle_lock: Default::default(),
        };
        let raw = ask(&target, &[&format!("#{{{MEMBERS}}}")]).map_err(tmux_error)?;
        let mut members = decode(&raw[0])?;
        let mut c = command();
        c.args(["list-panes", "-s", "-t", session, "-F"]);
        c.arg(format!("#{{pane_id}} {IS}"));
        let panes = capture(c).map_err(tmux_error)?;
        let existing: Vec<(&str, bool)> = panes
            .lines()
            .filter_map(|line| {
                let (id, carrier) = line.split_once(' ')?;
                valid_id(id, '%').then_some((id, carrier == "1"))
            })
            .collect();
        members.retain(|id, _| existing.iter().any(|(pane, _)| pane == id));
        // A killed hook cannot strand the lock. An interrupted registration
        // retains an obligation while Claude lives; proven death/reuse retires
        // it on the next lifecycle, even if the stale carrier remains visible.
        for row in members.values_mut() {
            let pid = row[0].as_u64().unwrap() as u32;
            let start = row[1].as_u64().unwrap();
            let alive = if start == 0 {
                sys::process_alive(pid)
            } else {
                sys::same_process(pid, start)
            };
            if row[3] == true && alive == Some(false) {
                row[3] = Value::Bool(false);
            }
        }
        let pid = cfg
            .claude_pid
            .as_ref()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
            .or_else(|| pane_pid.parse::<u32>().ok())
            .unwrap_or(0);
        let token = payload.session_id().unwrap_or("");
        let prior = members.get(pane);
        if paint == Paint::SessionStart {
            members.insert(
                pane.clone(),
                json!([pid, sys::process_start_time(pid).unwrap_or(0), token, true]),
            );
        } else {
            if let Some(row) = prior {
                // Ignore an old end delivered after a replacement in this pane.
                // Missing metadata cannot prove that two sessions differ.
                if row[0] != pid || (!token.is_empty() && row[2] != "" && row[2] != token) {
                    held.eligible = false;
                    return Ok(Some(held));
                }
                if row[3] == false {
                    // A repeated end can recover interrupted final cleanup, but
                    // must not fall through to the legacy assumed restore once
                    // the policy was successfully removed.
                    held.eligible =
                        ask(&target, &["#{@cctab_armed}"]).map_err(tmux_error)?[0] != "";
                }
            }
            members.insert(
                pane.clone(),
                json!([pid, sys::process_start_time(pid).unwrap_or(0), token, false]),
            );
        }
        held.others = existing.iter().any(|(id, carrier)| {
            *id != pane && members.get(*id).map_or(*carrier, |row| row[3] == true)
        });
        let mut c = command();
        t.inherit_lock(&mut c);
        c.args(["set-option", "-t", session, "--", MEMBERS]);
        c.arg(json!({"v": 1, "panes": members}).to_string());
        capture(c).map_err(tmux_error)?;
        Ok(Some(held))
    }

    pub fn eligible(&self) -> bool {
        self.eligible
    }
    pub fn has_owners(&self) -> bool {
        self.others
    }
}

impl Drop for Lifecycle<'_> {
    fn drop(&mut self) {
        self.tmux.lifecycle_lock.borrow_mut().take();
    }
}

fn valid_id(s: &str, prefix: char) -> bool {
    s.starts_with(prefix) && digits(s[1..].as_bytes())
}

fn tmux_error(_: super::Fail) -> io::Error {
    io::Error::other("tmux lifecycle command failed")
}

/// Reject unknown/corrupt register shapes instead of dropping their owners.
fn decode(raw: &str) -> io::Result<Map<String, Value>> {
    if raw.is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(raw).map_err(io::Error::other)?;
    let members = value["panes"]
        .as_object()
        .filter(|_| value["v"] == 1)
        .ok_or_else(|| io::Error::other("unknown tmux membership register"))?;
    for (pane, row) in members {
        if !valid_id(pane, '%')
            || !row.as_array().is_some_and(|r| r.len() == 4)
            || !row[0].as_u64().is_some_and(|pid| pid <= u32::MAX as u64)
            || row[1].as_u64().is_none()
            || row[2].as_str().is_none()
            || row[3].as_bool().is_none()
        {
            return Err(io::Error::other("invalid tmux membership entry"));
        }
    }
    Ok(members.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_torn_membership_cannot_become_an_empty_register() {
        assert!(decode("").unwrap().is_empty());
        assert_eq!(
            decode(r#"{"v":1,"panes":{"%0":[1,2,"s",true]}}"#)
                .unwrap()
                .len(),
            1
        );
        for invalid in [
            "{",
            r#"{"v":2,"panes":{}}"#,
            r#"{"v":1,"panes":{"%0":[]}}"#,
            r#"{"v":1,"panes":{"%0":[4294967296,0,"",true]}}"#,
        ] {
            assert!(decode(invalid).is_err(), "{invalid}");
        }
    }
}
