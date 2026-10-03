//! session-start and session-end on Windows, end to end, inside a pseudo console
//! this test makes. (Windows only; elsewhere this file compiles to nothing.)
//!
//! Those two edges title the console Claude Code runs in - see
//! `sys::windows::set_session_title` - and what a terminal receives is the pseudo
//! console's output stream, so that stream is what these tests read. A stand-in
//! claude runs INSIDE the pseudo console and spawns the real `tabstatus` the way
//! Claude Code spawns a hook: through Git Bash when there is one, CREATE_NO_WINDOW
//! (`windowsHide`), stdio on pipes, the payload on stdin, `CLAUDE_PID` naming it.
//!
//! THE DEVELOPER'S OWN TAB IS NEVER A TARGET. The stand-in starts from an environment
//! block built here, without the ambient `CLAUDE_PID`, and every hook is handed a pid
//! explicitly - the stand-in's, a decoy's, or a relay's. Nothing names an ancestor of this
//! process, and those include the terminal the tests were started from.
//!
//! The pseudo console is kernel32's: the inbox conhost. `CCTAB_TEST_CONPTY_DLL` names
//! a ConPTY package's conpty.dll instead (OpenConsole.exe beside it; VS Code ships
//! one), which is the modern passthrough ConPTY a Windows Terminal tab runs on. The
//! two end a title's OSC differently - BEL and ST - and both are read.
#![cfg(windows)]

use std::ffi::{c_void, OsStr, OsString};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, IntoRawHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, SetConsoleMode, SetStdHandle, COORD,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, HPCON, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// The binary under test: the one `hooks.json` runs.
const BIN: &str = env!("CARGO_BIN_EXE_tabstatus");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// What a re-executed copy of this test binary is: `claude` or `decoy`.
const ROLE: &str = "CCTAB_TEST_CONPTY_ROLE";
/// Written by the stand-in before its first hook; only what follows it is asserted.
const BEFORE: &str = "<<cctab-conpty-before>>";

// ------------------------------------------------------------------ the tests

#[test]
fn session_start_paints_the_tab_and_session_end_clears_it() {
    let run = Scenario::new("paint").run();
    let title = run.expected_title();
    // The same title the `terminalSequence` path paints for an idle edge in the same
    // directory - so a tab reads the same before and after its first prompt.
    assert!(!title.is_empty(), "{run}");
    assert_eq!(title, run.json_title(), "{run}");
    assert_eq!(run.titles(), vec![title, String::new()], "{run}");
    // The Konsole arming (OSC 50) has no console form and is never sent here.
    assert_eq!(find(run.after_marker(), b"\x1b]50;"), None, "{run}");
    run.assert_hooks_clean();
}

/// Claude Code with `windowsHide: false` would share its console with the hook; the
/// same detach-and-attach paints it.
#[test]
fn a_hook_that_shares_claudes_console_paints_it_too() {
    let run = Scenario::new("inherit").spawn("inherit").run();
    assert_eq!(run.titles(), vec![run.expected_title(), String::new()], "{run}");
    run.assert_hooks_clean();
}

/// `claude -p > out.txt` typed at a tab: guard 2, a file is not a character device.
#[test]
fn a_claude_redirected_to_a_file_is_not_painted() {
    let run = Scenario::new("file").stdout("file").run();
    assert!(run.titles().is_empty(), "{run}");
    run.assert_hooks_clean();
}

/// `claude -p > NUL`: a character device, so guard 3 is the one that refuses it.
#[test]
fn a_claude_writing_to_nul_is_not_painted() {
    let run = Scenario::new("nul").stdout("nul").run();
    assert!(run.titles().is_empty(), "{run}");
    run.assert_hooks_clean();
}

/// A live process on THIS console, its stdout this console, that is not an ancestor
/// of the hook - a recycled or foreign `CLAUDE_PID`: guard 1 alone refuses it.
#[test]
fn a_pid_that_is_not_an_ancestor_is_not_painted_even_on_the_same_console() {
    let run = Scenario::new("decoy").target("decoy").run();
    assert!(run.titles().is_empty(), "{run}");
    run.assert_hooks_clean();
}

/// The control for the next test: the hook is spawned by a RELAY, a second claude on
/// the stand-in's console that names itself in `CLAUDE_PID` and lives until the hook
/// is done, as Claude does. It is painted.
#[test]
fn a_hook_spawned_by_a_relay_claude_paints_its_console() {
    let run = Scenario::new("relay").target("relay").run();
    assert_eq!(run.titles(), vec![run.expected_title(), String::new()], "{run}");
    run.assert_hooks_clean();
}

/// The same relay, but it exits - and nothing here holds it open any more - before its
/// hook reads the payload, so before the hook can look at its `CLAUDE_PID`: Claude
/// quitting as a hook starts. Nothing is painted: the walk finds that pid exited -
/// something else may still hold its process open - or no longer there.
///
/// What this does NOT cover is the race the hook guards against by opening that pid
/// once the walk has matched it, checking its creation time is the walk's, and
/// holding it to the attach: Claude exiting in the middle AND its pid going to
/// another process on a console in between. Pid reuse cannot be forced; that case is
/// closed by construction - no reuse while a handle is open - and the comparison of
/// creation times is unit-tested in sys::windows.
#[test]
fn a_claude_that_exited_before_its_hook_looked_is_not_painted() {
    let run = Scenario::new("exited").target("exited").run();
    assert!(run.titles().is_empty(), "{run}");
    run.assert_hooks_clean();
}

/// Every other edge still prints its one protocol line and titles nothing itself.
#[test]
fn a_terminal_sequence_edge_prints_its_line_and_titles_nothing() {
    let run = Scenario::new("working").edges("working").run();
    assert!(run.titles().is_empty(), "{run}");
    let line = run.hooks().next().expect("one hook");
    assert!(line.contains(r#"exit=Some(0)"#), "{run}");
    assert!(line.contains(r#"stdout="{\"terminalSequence\":\"\\u001b]0;"#), "{run}");
}

// ------------------------------------------------------------------ the harness

/// One run: a pseudo console, a stand-in claude in it, and the hooks it spawns.
struct Scenario {
    tag: &'static str,
    edges: &'static str,
    stdout: &'static str,
    target: &'static str,
    spawn: &'static str,
}

impl Scenario {
    fn new(tag: &'static str) -> Scenario {
        Scenario { tag, edges: "session-start,session-end", stdout: "console", target: "self", spawn: "nowindow" }
    }
    fn edges(mut self, e: &'static str) -> Self {
        self.edges = e;
        self
    }
    fn stdout(mut self, s: &'static str) -> Self {
        self.stdout = s;
        self
    }
    fn target(mut self, t: &'static str) -> Self {
        self.target = t;
        self
    }
    fn spawn(mut self, s: &'static str) -> Self {
        self.spawn = s;
        self
    }

    fn run(self) -> Run {
        let dir = std::env::temp_dir().join(format!("cctab-conpty-{}-{}", self.tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for state in ["state", "reference-state"] {
            std::fs::create_dir_all(dir.join(state)).expect("mkdir");
        }
        let (in_r, mut in_w) = std::io::pipe().expect("pipe");
        let (mut out_r, out_w) = std::io::pipe().expect("pipe");
        let pc = PseudoConsole::new(&in_r, &out_w);
        drop((in_r, out_w));
        let reader = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = out_r.read_to_end(&mut v);
            v
        });
        if pc.modern {
            // The modern ConPTY asks its terminal for DA1 at startup; answer as Windows
            // Terminal does rather than leave it waiting.
            let _ = in_w.write_all(b"\x1b[?61;6;7;14;21;22;23;24;28;32;42c");
        }
        let mut env = env_block(&[
            (ROLE, OsStr::new("claude")),
            ("CCTAB_TEST_CONPTY_DIR", dir.as_os_str()),
            ("CCTAB_TEST_CONPTY_EDGES", OsStr::new(self.edges)),
            ("CCTAB_TEST_CONPTY_STDOUT", OsStr::new(self.stdout)),
            ("CCTAB_TEST_CONPTY_TARGET", OsStr::new(self.target)),
            ("CCTAB_TEST_CONPTY_SPAWN", OsStr::new(self.spawn)),
        ]);
        let pi = spawn_in(&pc, &mut env, "stand_in_claude");
        // SAFETY: live process handles from CreateProcessW, each closed once below.
        let code = unsafe {
            if WaitForSingleObject(pi.hProcess, 60_000) != 0 {
                TerminateProcess(pi.hProcess, 1);
            }
            let mut code = 0u32;
            GetExitCodeProcess(pi.hProcess, &mut code);
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            code
        };
        // The inbox conhost renders on a timer: let the last frame out before closing.
        std::thread::sleep(Duration::from_millis(300));
        drop(pc);
        drop(in_w);
        let stream = reader.join().expect("reader");
        let results = std::fs::read_to_string(dir.join("result.txt")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let run = Run { stream, results, code };
        if std::env::var_os("CCTAB_TEST_CONPTY_SHOW").is_some() {
            eprintln!("--- {}\n{run}", self.tag);
        }
        run
    }
}

struct Run {
    stream: Vec<u8>,
    results: String,
    code: u32,
}

impl Run {
    /// The title of every OSC 0 the pseudo console emitted after the stand-in's
    /// marker - which is everything the hooks could have caused.
    fn titles(&self) -> Vec<String> {
        osc_titles(self.after_marker())
    }

    /// The pseudo console's output from the stand-in's marker on.
    fn after_marker(&self) -> &[u8] {
        let from = find(&self.stream, BEFORE.as_bytes()).expect("the stand-in's marker reached the stream");
        &self.stream[from..]
    }

    /// What session-start paints, from its dry run.
    fn expected_title(&self) -> String {
        self.result("title=")
    }

    /// What the `terminalSequence` path paints for an idle edge in the same directory.
    fn json_title(&self) -> String {
        self.result("json=")
    }

    fn result(&self, key: &str) -> String {
        let line = self.results.lines().find(|l| l.starts_with(key));
        line.and_then(|l| l.strip_prefix(key)).unwrap_or_else(|| panic!("no {key} line\n{self}")).to_owned()
    }

    /// One line per hook the stand-in ran for real, after its two reference lines.
    fn hooks(&self) -> impl Iterator<Item = &str> {
        self.results.lines().skip(2)
    }

    /// Every hook exited 0, printed nothing and reported nothing.
    fn assert_hooks_clean(&self) {
        assert_eq!(self.code, 0, "{self}");
        let hooks: Vec<&str> = self.hooks().collect();
        assert!(!hooks.is_empty(), "{self}");
        for h in hooks {
            assert!(h.contains("exit=Some(0)") && h.contains(r#"stdout="""#) && h.contains(r#"stderr="""#), "{self}");
        }
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stand-in exit {:#x}\n{}\nstream: {:?}", self.code, self.results, String::from_utf8_lossy(&self.stream))?;
        // Every OSC 0 after the marker, byte for byte: what the terminal receives.
        if let Some(from) = find(&self.stream, BEFORE.as_bytes()) {
            for osc in oscs(&self.stream[from..]) {
                write!(f, "\nosc 0 after the marker: {}", osc.escape_ascii())?;
            }
        }
        Ok(())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The payload of every `ESC ] 0 ; ... (BEL | ESC \)` in `b`.
fn osc_titles(b: &[u8]) -> Vec<String> {
    oscs(b)
        .into_iter()
        .map(|o| {
            let body = o.strip_suffix(b"\x07").or_else(|| o.strip_suffix(b"\x1b\\")).unwrap_or(o);
            String::from_utf8_lossy(&body[4..]).into_owned()
        })
        .collect()
}

/// Every `ESC ] 0 ; ... (BEL | ESC \)` in `b`, whole; one the stream cuts off runs
/// to its end.
fn oscs(b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = find(&b[i..], b"\x1b]0;") {
        let start = i + at;
        let end = (start + 4..b.len())
            .find_map(|j| match (b[j], b.get(j + 1)) {
                (0x07, _) => Some(j + 1),
                (0x1b, Some(b'\\')) => Some(j + 2),
                _ => None,
            })
            .unwrap_or(b.len());
        out.push(&b[start..end]);
        i = end;
    }
    out
}

/// A pseudo console: kernel32's, or the one a ConPTY package's conpty.dll makes.
struct PseudoConsole {
    hpc: HPCON,
    close: unsafe extern "system" fn(HPCON),
    modern: bool,
}

impl PseudoConsole {
    fn new(input: &impl AsRawHandle, output: &impl AsRawHandle) -> PseudoConsole {
        type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
        type Close = unsafe extern "system" fn(HPCON);
        let (create, close, modern): (Create, Close, bool) = match std::env::var_os("CCTAB_TEST_CONPTY_DLL") {
            None => (CreatePseudoConsole, ClosePseudoConsole, false),
            // SAFETY: a NUL-terminated path; the two exports have exactly kernel32's
            // signatures, which is the point of the package.
            Some(dll) => unsafe {
                let w: Vec<u16> = dll.encode_wide().chain([0]).collect();
                let lib = LoadLibraryW(w.as_ptr());
                assert!(!lib.is_null(), "LoadLibraryW {}: {}", dll.to_string_lossy(), std::io::Error::last_os_error());
                let c = GetProcAddress(lib, c"CreatePseudoConsole".as_ptr().cast()).expect("CreatePseudoConsole");
                let k = GetProcAddress(lib, c"ClosePseudoConsole".as_ptr().cast()).expect("ClosePseudoConsole");
                (std::mem::transmute::<_, Create>(c), std::mem::transmute::<_, Close>(k), true)
            },
        };
        let mut hpc: HPCON = 0;
        // SAFETY: two live pipe handles, which the pseudo console duplicates.
        let hr = unsafe {
            create(COORD { X: 120, Y: 30 }, input.as_raw_handle() as HANDLE, output.as_raw_handle() as HANDLE, 0, &mut hpc)
        };
        assert!(hr >= 0, "CreatePseudoConsole: {hr:#x}");
        PseudoConsole { hpc, close, modern }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        // SAFETY: the handle CreatePseudoConsole made, closed once.
        unsafe { (self.close)(self.hpc) }
    }
}

/// This environment, minus `CLAUDE_PID`, plus `extra`: a CreateProcessW block.
fn env_block(extra: &[(&str, &OsStr)]) -> Vec<u16> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("CLAUDE_PID"))
        .filter(|(k, _)| !extra.iter().any(|(e, _)| k.eq_ignore_ascii_case(e)))
        .collect();
    vars.extend(extra.iter().map(|(k, v)| (OsString::from(k), v.to_os_string())));
    vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Start this test binary's ignored test `test` inside `pc`, with `env`.
fn spawn_in(pc: &PseudoConsole, env: &mut [u16], test: &str) -> PROCESS_INFORMATION {
    let exe = std::env::current_exe().expect("exe");
    let mut cmd: Vec<u16> =
        format!("\"{}\" --exact {test} --ignored", exe.display()).encode_utf16().chain([0]).collect();
    let mut size = 0usize;
    // SAFETY: the documented two-call sizing of an attribute list, a live HPCON as
    // the one attribute, and buffers that outlive CreateProcessW.
    unsafe {
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        let mut list = vec![0u8; size];
        let attrs = list.as_mut_ptr().cast();
        assert!(InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) != 0);
        assert!(
            UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                pc.hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null()
            ) != 0
        );
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Null standard handles: the child takes the pseudo console's. Without the
        // flag it takes this process's, which a test runner has redirected.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = attrs;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            null(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_ptr().cast(),
            null(),
            &si.StartupInfo,
            &mut pi,
        );
        assert!(ok != 0, "CreateProcessW: {}", std::io::Error::last_os_error());
        DeleteProcThreadAttributeList(attrs);
        pi
    }
}

// ------------------------------------------------------------------ the children

/// Not a test: the stand-in claude, run inside the pseudo console by `Scenario::run`.
#[test]
#[ignore = "a child process for the tests in this file"]
fn stand_in_claude() {
    if std::env::var_os(ROLE).as_deref() != Some(OsStr::new("claude")) {
        return;
    }
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let dir = PathBuf::from(var("CCTAB_TEST_CONPTY_DIR"));
    let mut con = std::fs::OpenOptions::new().read(true).write(true).open("CONOUT$").expect("CONOUT$");
    let mut mode = 0;
    // SAFETY: a live console handle and a live u32. Claude's TUI runs with VT on.
    unsafe {
        GetConsoleMode(con.as_raw_handle() as HANDLE, &mut mode);
        SetConsoleMode(con.as_raw_handle() as HANDLE, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
    // `claude -p > file` and `> NUL`: guard 2 reads the PEB's StandardOutput, which
    // is what SetStdHandle sets. The handle lives until this process exits.
    // SAFETY: a handle this process owns and never closes, left to the process's end.
    let redirect = |f: std::fs::File| unsafe { SetStdHandle(STD_OUTPUT_HANDLE, f.into_raw_handle() as HANDLE) };
    match var("CCTAB_TEST_CONPTY_STDOUT").as_str() {
        "file" => redirect(std::fs::File::create(dir.join("claude.out")).expect("file")),
        "nul" => redirect(std::fs::OpenOptions::new().write(true).open("NUL").expect("NUL")),
        _ => 1,
    };
    let mut decoy = (var("CCTAB_TEST_CONPTY_TARGET") == "decoy").then(|| {
        Command::new(std::env::current_exe().expect("exe"))
            .args(["--exact", "decoy", "--ignored"])
            .env(ROLE, "decoy")
            .stdin(Stdio::piped())
            .spawn()
            .expect("decoy")
    });
    let target = decoy.as_ref().map_or(std::process::id(), |d| d.id());
    let inherit = var("CCTAB_TEST_CONPTY_SPAWN") == "inherit";
    // Some(exits): each edge's hook is spawned by a relay claude, see `relayed_hook`.
    let relay = match var("CCTAB_TEST_CONPTY_TARGET").as_str() {
        "relay" => Some(false),
        "exited" => Some(true),
        _ => None,
    };
    // What session-start WOULD paint, from a dry run through the same spawn path; and
    // what the `terminalSequence` path paints for an idle edge in the same directory,
    // from a real run with a state of its own. Neither titles anything.
    let dry = hook(&dir, "state", "session-start", target, inherit, true);
    let json = hook(&dir, "reference-state", "idle", target, inherit, false);
    let line: serde_json::Value = serde_json::from_str(&json.stdout).unwrap_or_default();
    let sequence = line["terminalSequence"].as_str().unwrap_or_default();
    let json_title = sequence.strip_prefix("\x1b]0;").and_then(|t| t.strip_suffix('\x07'));
    let mut results = format!("title={}\n", dry.stdout.trim_end_matches('\n'));
    results.push_str(&format!("json={}\n", json_title.unwrap_or(&json.line)));
    let _ = write!(con, "{BEFORE}\r\n");
    std::thread::sleep(Duration::from_millis(150));
    for edge in var("CCTAB_TEST_CONPTY_EDGES").split(',') {
        let line = match relay {
            Some(exits) => relayed_hook(&dir, edge, exits),
            None => hook(&dir, "state", edge, target, inherit, false).line,
        };
        results.push_str(&line);
        results.push('\n');
        // Two titles inside one frame of the inbox conhost would be coalesced.
        std::thread::sleep(Duration::from_millis(150));
    }
    if let Some(d) = decoy.as_mut() {
        drop(d.stdin.take());
        let _ = d.wait();
    }
    std::fs::write(dir.join("result.txt"), results).expect("result");
}

/// Not a test: a live process on the stand-in's console that is not the hook's
/// ancestor, until its stdin closes.
#[test]
#[ignore = "a child process for the tests in this file"]
fn decoy() {
    if std::env::var_os(ROLE).as_deref() == Some(OsStr::new("decoy")) {
        let _ = std::io::stdin().read_to_end(&mut Vec::new());
    }
}

/// Not a test: a relay claude on the stand-in's console. It spawns one hook as Claude
/// Code does, naming ITSELF in `CLAUDE_PID`, with its own stdin - the stand-in's
/// payload pipe - and files for the hook's output; writes the hook's pid to
/// `hook.pid`; and then either exits at once or waits for the hook.
#[test]
#[ignore = "a child process for the tests in this file"]
fn relay() {
    if std::env::var_os(ROLE).as_deref() != Some(OsStr::new("relay")) {
        return;
    }
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let dir = PathBuf::from(var("CCTAB_TEST_CONPTY_DIR"));
    let mut c = hook_command(&dir, "state", &var("CCTAB_TEST_CONPTY_EDGE"), std::process::id(), false, false);
    c.stdin(Stdio::inherit())
        .stdout(std::fs::File::create(dir.join("hook.out")).expect("hook.out"))
        .stderr(std::fs::File::create(dir.join("hook.err")).expect("hook.err"));
    let mut child = c.spawn().expect("hook");
    std::fs::write(dir.join("hook.pid.tmp"), child.id().to_string()).expect("pid");
    std::fs::rename(dir.join("hook.pid.tmp"), dir.join("hook.pid")).expect("pid");
    if var("CCTAB_TEST_CONPTY_RELAY_EXITS").is_empty() {
        let _ = child.wait();
    }
}

/// Run `tabstatus <edge>` through a [`relay`] on this console, and report it as
/// [`hook`] does. With `exits`, the relay has exited and its handle here is closed
/// before the hook is given its payload - which it reads before anything else - so
/// its `CLAUDE_PID` is gone by the time it looks. The hook is opened by pid while it
/// waits on that payload, alive, so the pid is its own.
fn relayed_hook(dir: &Path, edge: &str, exits: bool) -> String {
    let (payload_r, mut payload_w) = std::io::pipe().expect("pipe");
    let pid_file = dir.join("hook.pid");
    let _ = std::fs::remove_file(&pid_file);
    let mut relay = Command::new(std::env::current_exe().expect("exe"))
        .args(["--exact", "relay", "--ignored"])
        .env(ROLE, "relay")
        .env("CCTAB_TEST_CONPTY_EDGE", edge)
        .env("CCTAB_TEST_CONPTY_RELAY_EXITS", if exits { "1" } else { "" })
        .stdin(payload_r)
        .spawn()
        .expect("relay");
    let t = Instant::now();
    let pid: u32 = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file).ok().and_then(|s| s.parse().ok()) {
            break pid;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the relay wrote no hook.pid");
        std::thread::sleep(Duration::from_millis(10));
    };
    // SAFETY: no pointers; a null handle is the failure, checked at once.
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    assert!(!h.is_null(), "open hook {pid}: {}", std::io::Error::last_os_error());
    // With `exits`, waited for and then its handle closed: nothing here keeps its pid.
    let live_relay = if exits {
        let _ = relay.wait();
        drop(relay);
        None
    } else {
        Some(relay)
    };
    let t = Instant::now();
    let _ = payload_w.write_all(payload(edge).as_bytes());
    let _ = payload_w.write_all(b"\n");
    drop(payload_w);
    // SAFETY: the live process handle opened above, closed once here.
    let code = unsafe {
        let code = (WaitForSingleObject(h, 30_000) == 0).then(|| {
            let mut code = 0u32;
            GetExitCodeProcess(h, &mut code);
            code as i32
        });
        CloseHandle(h);
        code
    };
    let ms = t.elapsed().as_millis();
    if let Some(mut relay) = live_relay {
        let _ = relay.wait();
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    format!("{edge} exit={code:?} ms={ms} stdout={:?} stderr={:?}", read("hook.out"), read("hook.err"))
}

struct Hook {
    stdout: String,
    line: String,
}

/// What Claude Code writes to the hook for `edge`.
fn payload(edge: &str) -> &'static str {
    match edge {
        "session-start" => r#"{"session_id":"conpty","hook_event_name":"SessionStart","source":"startup"}"#,
        "session-end" => r#"{"session_id":"conpty","hook_event_name":"SessionEnd","reason":"exit"}"#,
        "idle" => r#"{"session_id":"conpty","hook_event_name":"Stop"}"#,
        _ => r#"{"session_id":"conpty","hook_event_name":"UserPromptSubmit"}"#,
    }
}

/// Run `tabstatus <edge>` as Claude Code runs a hook: `bash -c` through Git Bash
/// when there is one, CREATE_NO_WINDOW unless `inherit`, stdio on pipes, the payload
/// on stdin, and `CLAUDE_PID` = `target`; in `dir`, with `dir\<state>` as its state.
fn hook(dir: &Path, state: &str, edge: &str, target: u32, inherit: bool, dry: bool) -> Hook {
    let mut c = hook_command(dir, state, edge, target, inherit, dry);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let t = Instant::now();
    let mut child = c.spawn().expect("hook");
    let mut stdin = child.stdin.take().expect("stdin");
    let _ = stdin.write_all(payload(edge).as_bytes());
    let _ = stdin.write_all(b"\n");
    drop(stdin);
    let out = child.wait_with_output().expect("hook");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = format!(
        "{edge} exit={:?} ms={} stdout={:?} stderr={:?}",
        out.status.code(),
        t.elapsed().as_millis(),
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
    Hook { stdout, line }
}

/// The hook's command, all but its stdio: see [`hook`].
fn hook_command(dir: &Path, state: &str, edge: &str, target: u32, inherit: bool, dry: bool) -> Command {
    let bash = std::env::var_os("CCTAB_TEST_GIT_BASH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"));
    let mut c = if bash.is_file() {
        let mut c = Command::new(&bash);
        c.arg("-c").arg(format!("\"{}\" {edge}", BIN.replace('\\', "/")));
        c
    } else {
        let mut c = Command::new(BIN);
        c.arg(edge);
        c
    };
    if !inherit {
        c.creation_flags(CREATE_NO_WINDOW);
    }
    for v in ["TMUX", "TMUX_PANE", "STY", "KONSOLE_VERSION", "KONSOLE_DBUS_SESSION", "CCTAB_TERMINAL", "CCTAB_DRY_RUN"] {
        c.env_remove(v);
    }
    if dry {
        c.env("CCTAB_DRY_RUN", "1");
    }
    c.current_dir(dir).env("CLAUDE_PID", target.to_string()).env("CCTAB_STATE_DIR", dir.join(state));
    c
}
