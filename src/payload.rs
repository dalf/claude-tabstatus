//! Selective, top-level hook metadata parsing. Unused values are syntax-checked and
//! skipped with `IgnoredAny`, never materialized as a JSON tree. JSON whitespace,
//! member order and escapes do not change a field's meaning.

use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde_json::value::RawValue;
use std::fmt;
use std::io::{self, Read};

/// Maximum complete input size, including whitespace. Oversized input is still
/// drained to EOF so the hook writer does not encounter a closed pipe.
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Identity metadata is retained whole or not at all; truncated keys could
/// correlate two unrelated requests. This bounds each persisted component.
pub const MAX_ELICITATION_ID_BYTES: usize = 64;

#[derive(Debug, Default)]
pub struct Payload {
    session_id: Option<String>,
    agent_id: Option<String>,
    source: Option<String>,
    notification_type: Option<String>,
    hook_event_name: Option<String>,
    prompt_first: Option<char>,
    background_tasks_empty: Option<bool>,
    mcp_server_name: Option<String>,
    elicitation_id: Option<String>,
    mode: Option<String>,
    action: Option<String>,
}

impl Payload {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn read() -> io::Result<Self> {
        Self::from_reader(&mut io::stdin().lock())
    }

    #[cfg(test)]
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        Self::from_reader(&mut &bytes[..])
    }

    fn from_reader(reader: &mut impl Read) -> io::Result<Self> {
        let mut bytes = Vec::with_capacity(1024);
        let mut buf = [0; 65536];
        let mut oversized = false;
        while let Some(n) = read_chunk(reader, &mut buf)? {
            if oversized {
                continue;
            }
            if n > MAX_INPUT_BYTES - bytes.len() {
                oversized = true;
                continue;
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        if oversized {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "hook input exceeds 16 MiB",
            ));
        }
        // Empty stdin is the established manual invocation interface. Whitespace
        // alone, unlike no bytes at all, is an invalid nonempty JSON document.
        if bytes.is_empty() {
            return Ok(Self::empty());
        }
        // IgnoredAny need not decode strings. Validate UTF-8 even in skipped data.
        std::str::from_utf8(&bytes).map_err(invalid_data)?;
        serde_json::from_slice(&bytes).map_err(invalid_data)
    }

    pub fn session_id(&self) -> Option<&str> {
        nonempty(self.session_id.as_deref())
    }
    pub fn agent_id(&self) -> Option<&str> {
        nonempty(self.agent_id.as_deref())
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn notification_type(&self) -> Option<&str> {
        self.notification_type.as_deref()
    }
    pub fn hook_event_name(&self) -> Option<&str> {
        self.hook_event_name.as_deref()
    }
    pub fn prompt_first(&self) -> Option<char> {
        self.prompt_first
    }
    /// Whether `background_tasks` holds nothing but artifact watches.
    pub fn background_tasks_empty(&self) -> Option<bool> {
        self.background_tasks_empty
    }
    pub fn mcp_server_name(&self) -> Option<&str> {
        self.mcp_server_name.as_deref()
    }
    pub fn elicitation_id(&self) -> Option<&str> {
        self.elicitation_id.as_deref()
    }
    pub fn mode(&self) -> Option<&str> {
        self.mode.as_deref()
    }
    pub fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    pub fn elicitation_mode_supported(&self) -> bool {
        matches!(self.mode(), None | Some("form" | "url"))
    }
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}
fn invalid_data(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

// The key visitor matches borrowed names without allocating unknown keys. Serde
// also calls visit_str with decoded escaped names, so duplicate detection sees
// `agent_id` and `agent\u005fid` as the same recognized field.
#[derive(Clone, Copy)]
enum Field {
    Session,
    Agent,
    Source,
    Notification,
    Event,
    Prompt,
    Background,
    McpServer,
    ElicitationId,
    Mode,
    Action,
    Other,
}
impl Field {
    fn name(self) -> &'static str {
        match self {
            Self::Session => "session_id",
            Self::Agent => "agent_id",
            Self::Source => "source",
            Self::Notification => "notification_type",
            Self::Event => "hook_event_name",
            Self::Prompt => "prompt",
            Self::Background => "background_tasks",
            Self::McpServer => "mcp_server_name",
            Self::ElicitationId => "elicitation_id",
            Self::Mode => "mode",
            Self::Action => "action",
            Self::Other => "unknown",
        }
    }
}
impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Keys;
        impl Visitor<'_> for Keys {
            type Value = Field;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object key")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Field, E> {
                Ok(match s {
                    "session_id" => Field::Session,
                    "agent_id" => Field::Agent,
                    "source" => Field::Source,
                    "notification_type" => Field::Notification,
                    "hook_event_name" => Field::Event,
                    "prompt" => Field::Prompt,
                    "background_tasks" => Field::Background,
                    "mcp_server_name" => Field::McpServer,
                    "elicitation_id" => Field::ElicitationId,
                    "mode" => Field::Mode,
                    "action" => Field::Action,
                    _ => Field::Other,
                })
            }
        }
        d.deserialize_identifier(Keys)
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Metadata;
        impl<'de> Visitor<'de> for Metadata {
            type Value = Payload;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a hook object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Payload, M::Error> {
                let mut p = Payload::empty();
                let mut seen = 0u16;
                while let Some(field) = map.next_key::<Field>()? {
                    if matches!(field, Field::Other) {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let bit = 1 << field as u8;
                    if seen & bit != 0 {
                        return Err(de::Error::duplicate_field(field.name()));
                    }
                    seen |= bit;
                    match field {
                        Field::Session => p.session_id = map.next_value()?,
                        Field::Agent => p.agent_id = map.next_value()?,
                        Field::Source => p.source = map.next_value()?,
                        Field::Notification => p.notification_type = map.next_value()?,
                        Field::Event => p.hook_event_name = map.next_value()?,
                        Field::Prompt => {
                            p.prompt_first =
                                map.next_value::<Option<FirstChar>>()?.and_then(|s| s.0)
                        }
                        Field::Background => {
                            p.background_tasks_empty =
                                map.next_value::<Option<EmptyArray>>()?.map(|a| a.0)
                        }
                        Field::McpServer => {
                            p.mcp_server_name = map.next_value::<Option<Identity>>()?.and_then(|s| s.0)
                        }
                        Field::ElicitationId => {
                            p.elicitation_id = map.next_value::<Option<Identity>>()?.and_then(|s| s.0)
                        }
                        Field::Mode => p.mode = map.next_value()?,
                        Field::Action => p.action = map.next_value()?,
                        Field::Other => unreachable!(),
                    }
                }
                Ok(p)
            }
        }
        d.deserialize_map(Metadata)
    }
}

struct Identity(Option<String>);
impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Bounded;
        impl Visitor<'_> for Bounded {
            type Value = Identity;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an elicitation identity string")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Identity, E> {
                let valid = !s.is_empty()
                    && s.len() <= MAX_ELICITATION_ID_BYTES
                    && !s.chars().any(char::is_control);
                Ok(Identity(valid.then(|| s.to_owned())))
            }
        }
        d.deserialize_str(Bounded)
    }
}

struct FirstChar(Option<char>);
impl<'de> Deserialize<'de> for FirstChar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct First;
        impl Visitor<'_> for First {
            type Value = FirstChar;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a prompt string")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<FirstChar, E> {
                Ok(FirstChar(s.chars().next()))
            }
        }
        d.deserialize_str(First)
    }
}
struct EmptyArray(bool);
impl<'de> Deserialize<'de> for EmptyArray {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Array;
        impl<'de> Visitor<'de> for Array {
            type Value = EmptyArray;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a background task array")
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<EmptyArray, S::Error> {
                let mut empty = true;
                while let Some(entry) = seq.next_element::<&RawValue>()? {
                    let mut entry = serde_json::Deserializer::from_str(entry.get());
                    empty &= Probe::Entry.deserialize(&mut entry).unwrap_or(false);
                }
                Ok(EmptyArray(empty))
            }
        }
        d.deserialize_seq(Array)
    }
}

// Claude Code re-arms an artifact's live-update watch on session resume as a
// monitor that never ends, so counting it would hold the tab purple for good.
// Only a `monitor` whose description names a claude.ai artifact is skipped. Any
// other entry - other monitors, malformed or non-object entries, an entry
// repeating `type` or `description` - still counts, as does wording Claude Code
// may change: a miss fails toward purple. Each entry is skipped as leniently as
// any unused value, then reread; a strict decoding error there (a lone surrogate,
// an out-of-range number) counts the entry instead of rejecting the payload.
// Strings are inspected in place, never retained.
#[derive(Clone, Copy)]
enum Probe {
    Entry,
    Text(fn(&str) -> bool),
}
impl<'de> DeserializeSeed<'de> for Probe {
    type Value = bool;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Probe {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<bool, M::Error> {
        let (mut kind, mut text, mut repeated) = (None, None, false);
        while let Some(key) = map.next_key::<EntryKey>()? {
            let (slot, test): (_, fn(&str) -> bool) = match (self, key) {
                (Self::Entry, EntryKey::Type) => (&mut kind, |s| s == "monitor"),
                (Self::Entry, EntryKey::Description) => (&mut text, |s| {
                    s.contains("claude.ai/artifact/") || s.contains("claude.ai/code/artifact/")
                }),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    continue;
                }
            };
            repeated |= slot.is_some();
            *slot = Some(map.next_value_seed(Self::Text(test))?);
        }
        Ok(!repeated && kind == Some(true) && text == Some(true))
    }
    fn visit_str<E>(self, s: &str) -> Result<bool, E> {
        Ok(matches!(self, Self::Text(test) if test(s)))
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<bool, S::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(false)
    }
    fn visit_bool<E>(self, _: bool) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_i64<E>(self, _: i64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_u64<E>(self, _: u64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_f64<E>(self, _: f64) -> Result<bool, E> {
        Ok(false)
    }
    fn visit_unit<E>(self) -> Result<bool, E> {
        Ok(false)
    }
}
enum EntryKey {
    Type,
    Description,
    Other,
}
impl<'de> Deserialize<'de> for EntryKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Keys;
        impl Visitor<'_> for Keys {
            type Value = EntryKey;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object key")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<EntryKey, E> {
                Ok(match s {
                    "type" => EntryKey::Type,
                    "description" => EntryKey::Description,
                    _ => EntryKey::Other,
                })
            }
        }
        d.deserialize_identifier(Keys)
    }
}

/// Metadata-independent stateless edges still just drain stdin.
pub fn drain_stdin() {
    let mut buf = [0; 65536];
    let mut stdin = io::stdin().lock();
    while matches!(read_chunk(&mut stdin, &mut buf), Ok(Some(_))) {}
}
fn read_chunk(r: &mut impl Read, buf: &mut [u8]) -> io::Result<Option<usize>> {
    loop {
        match r.read(buf) {
            Ok(0) => return Ok(None),
            Ok(n) => return Ok(Some(n)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(s: &str) -> Payload {
        Payload::from_bytes(s.as_bytes()).unwrap()
    }

    #[test]
    fn only_top_level_metadata_counts() {
        let p = parse(
            r#"{"tool_response":{"session_id":"other","agent_id":"a","source":"compact","notification_type":"permission_prompt","hook_event_name":"UserPromptSubmit","prompt":"yes","background_tasks":[]},"session_id":"main"}"#,
        );
        assert_eq!(p.session_id(), Some("main"));
        assert_eq!(p.agent_id(), None);
        assert_eq!(p.source(), None);
        assert_eq!(p.notification_type(), None);
        assert_eq!(p.hook_event_name(), None);
        assert_eq!(p.prompt_first(), None);
        assert_eq!(p.background_tasks_empty(), None);
    }
    #[test]
    fn multiline_reordered_late_and_escaped_metadata_has_one_meaning() {
        let p = parse(&format!("{{\n\"padding\":\"{}\",\n\"agent\\u005fid\" : \"a\\u0031\",\"notification_type\":\"permission_\\u0070rompt\",\"session_id\":\"s1\",\"background_tasks\": [ \n ],\"prompt\":\"\\u003ctask\"\n}}", "x".repeat(40000)));
        assert_eq!(p.agent_id(), Some("a1"));
        assert_eq!(p.session_id(), Some("s1"));
        assert_eq!(p.notification_type(), Some("permission_prompt"));
        assert_eq!(p.background_tasks_empty(), Some(true));
        assert_eq!(p.prompt_first(), Some('<'));
    }
    #[test]
    fn null_and_empty_ids_are_absent_and_background_absent_is_not_empty() {
        let p = parse(
            r#"{"session_id":"","agent_id":null,"source":null,"notification_type":null,"hook_event_name":null,"prompt":null,"background_tasks":null}"#,
        );
        assert_eq!(p.session_id(), None);
        assert_eq!(p.agent_id(), None);
        assert_eq!(p.background_tasks_empty(), None);
        assert_eq!(
            parse(r#"{"background_tasks":[null,{},[1]]}"#).background_tasks_empty(),
            Some(false)
        );
    }
    #[test]
    fn only_artifact_watches_leave_background_tasks_empty() {
        let empty = |tasks: &str| {
            parse(&format!(r#"{{"background_tasks":[{tasks}]}}"#)).background_tasks_empty()
        };
        let watch = r#"{"id":"ss33q6quc","type":"monitor","status":"running","description":"live updates for artifact https://claude.ai/artifact/TdSu4K4Nq5fSMbGLoStgbz (re-armed on session resume)"}"#;
        let workflow = r#"{"id":"wqot22fdh","type":"workflow","status":"running","description":"tiny test","name":"tiny"}"#;
        assert_eq!(empty(watch), Some(true));
        assert_eq!(empty(&format!("{watch},{watch}")), Some(true));
        assert_eq!(empty(&format!("{watch},{workflow}")), Some(false));
        assert_eq!(empty(&format!("{workflow},{watch}")), Some(false));
        for tasks in [
            r#"{"description":"x claude.ai/code/artifact/abc","type":"monitor"}"#,
            r#"{"type":"\u006donitor","description":"claude.ai\/artifact\/x"}"#,
            r#"{"type":"monitor","nested":{"type":"x"},"description":"claude.ai/artifact/x"}"#,
        ] {
            assert_eq!(empty(tasks), Some(true), "{tasks}");
        }
        // Repeated `type` or `description` cannot reject the input as a repeated
        // top-level field does: that would leave the tab unpainted, not purple.
        for tasks in [
            r#"{"type":"monitor","description":"CI run 123"}"#,
            r#"{"type":"monitor"}"#,
            r#"{"type":"monitor","description":null}"#,
            r#"{"type":"monitor","description":["claude.ai/artifact/x"]}"#,
            r#"{"type":"Monitor","description":"claude.ai/artifact/x"}"#,
            r#"{"type":"workflow","description":"claude.ai/artifact/x"}"#,
            r#"{"type":"monitor","description":"claude.ai/artifact/x","type":"monitor"}"#,
            r#"{"type":"monitor","description":"claude.ai/artifact/x","description":"CI"}"#,
            r#"{"type":"monitor","descr\u0069ption":"claude.ai/artifact/x","description":"claude.ai/artifact/x"}"#,
            r#""claude.ai/artifact/x""#,
            r#"null,{},[1]"#,
            r#"{"type":"monitor","description":"claude.ai/artifact/x \uD800"}"#,
            r#"{"type":"monitor","description":"claude.ai/artifact/x","\uD800":1}"#,
            r#""\uDC00",1e999"#,
        ] {
            assert_eq!(empty(tasks), Some(false), "{tasks}");
        }
    }
    #[test]
    fn wrong_types_and_duplicate_recognized_keys_are_rejected() {
        for field in [
            "session_id",
            "agent_id",
            "source",
            "notification_type",
            "hook_event_name",
            "prompt",
            "background_tasks",
            "mcp_server_name",
            "elicitation_id",
            "mode",
            "action",
        ] {
            for value in ["0", "true", "{}"] {
                assert!(
                    Payload::from_bytes(format!(r#"{{"{field}":{value}}}"#).as_bytes()).is_err(),
                    "{field}: {value}"
                );
            }
            assert!(
                Payload::from_bytes(format!(r#"{{"{field}":null,"{field}":null}}"#).as_bytes())
                    .is_err(),
                "duplicate {field}"
            );
        }
        assert!(Payload::from_bytes(br#"{"agent_id":"a","agent\u005fid":"b"}"#).is_err());
        assert!(Payload::from_bytes(br#"{"agent_id":[]}"#).is_err());
        assert!(Payload::from_bytes(br#"{"background_tasks":"[]"}"#).is_err());
        // Unknown keys never influence a decision; duplicate unknowns are allowed.
        parse(r#"{"x":1,"x":2}"#);
    }
    #[test]
    fn elicitation_metadata_is_top_level_and_identity_is_never_truncated() {
        let p = parse(r#"{"content":{"mcp_server_name":"nested","elicitation_id":"nested","action":"accept","mode":"url"},"mcp_server_name":"s\\name","elicitation_id":"id\u0031","action":"decline"}"#);
        assert_eq!(p.mcp_server_name(), Some("s\\name"));
        assert_eq!(p.elicitation_id(), Some("id1"));
        assert_eq!(p.action(), Some("decline"));
        assert_eq!(p.mode(), None);
        assert!(p.elicitation_mode_supported());
        for id in ["x".repeat(64), "é".repeat(32)] {
            let p = parse(&serde_json::json!({"mcp_server_name":id, "elicitation_id":id}).to_string());
            assert_eq!(p.mcp_server_name(), Some(id.as_str()));
            assert_eq!(p.elicitation_id(), Some(id.as_str()));
        }
        for id in [String::new(), "x".repeat(65), "é".repeat(33), "a\nb".into(), "\u{85}".into()] {
            let p = parse(&serde_json::json!({"mcp_server_name":id, "elicitation_id":id}).to_string());
            assert_eq!(p.mcp_server_name(), None);
            assert_eq!(p.elicitation_id(), None);
        }
        assert!(Payload::from_bytes(br#"{"elicitation_id":"a","elicitation\u005fid":"b"}"#).is_err());
    }
    #[test]
    fn only_empty_stdin_is_the_manual_exception() {
        assert!(Payload::from_bytes(b"").is_ok());
        for s in [
            " ",
            "\n",
            "[]",
            "null",
            "true",
            "1",
            "\"x\"",
            "{",
            "{} {}",
            "{}\n{}",
            "{\"x\":1,}",
            "{\"x\":\"\\q\"}",
        ] {
            assert!(Payload::from_bytes(s.as_bytes()).is_err(), "{s:?}");
        }
        assert!(Payload::from_bytes(b"{\"ignored\":\"a\0b\"}").is_err());
        assert!(Payload::from_bytes(b"{\"ignored\":\"\xff\"}").is_err());
        parse(" \n {} \t\r\n");
    }
    #[test]
    fn ignored_deep_data_uses_the_nonrecursive_skip_and_still_checks_syntax() {
        let s = format!(
            "{{\"tool_response\":{}0{},\"agent_id\":\"a\"}}",
            "[".repeat(10000),
            "]".repeat(10000)
        );
        assert_eq!(parse(&s).agent_id(), Some("a"));
        let broken = s.replacen(']', "", 1);
        assert!(Payload::from_bytes(broken.as_bytes()).is_err());
    }
    #[test]
    fn ignored_surrogate_escapes_are_not_decoded_but_retained_strings_are() {
        // IgnoredAny accepts JSON escape syntax without Unicode scalar decoding.
        parse(r#"{"tool_response":"\uD800"}"#);
        assert!(Payload::from_bytes(br#"{"agent_id":"\uD800"}"#).is_err());
    }

    #[test]
    fn input_limit_counts_whitespace_and_oversize_is_drained() {
        let mut bytes = b"{}".to_vec();
        bytes.resize(MAX_INPUT_BYTES, b' ');
        assert!(Payload::from_bytes(&bytes).is_ok());
        bytes.extend_from_slice(&vec![b' '; 65537]);
        let mut cursor = io::Cursor::new(&bytes);
        assert!(Payload::from_reader(&mut cursor).is_err());
        assert_eq!(cursor.position(), bytes.len() as u64);
    }
    #[test]
    fn interrupted_and_chunked_reads_retry_but_io_errors_reject_a_complete_prefix() {
        struct Chunks<'a> {
            rest: &'a [u8],
            interrupt: bool,
            fail: bool,
        }
        impl Read for Chunks<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.interrupt {
                    self.interrupt = false;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.rest.is_empty() {
                    return if self.fail {
                        Err(io::ErrorKind::Other.into())
                    } else {
                        Ok(0)
                    };
                }
                buf[0] = self.rest[0];
                self.rest = &self.rest[1..];
                self.interrupt = true;
                Ok(1)
            }
        }
        let bytes = br#"{"agent_id":"a1"}"#;
        let mut r = Chunks {
            rest: bytes,
            interrupt: true,
            fail: false,
        };
        assert_eq!(Payload::from_reader(&mut r).unwrap().agent_id(), Some("a1"));
        r.rest = bytes;
        r.fail = true;
        assert!(Payload::from_reader(&mut r).is_err());
    }
}
