//! Hand-rolled D-Bus client: exactly one method call (org.freedesktop.Notifications.Notify).
//! No dependencies. Measures how hard "just do it yourself" really is.
use std::io::{Read, Write};
use std::convert::TryInto;
use std::os::unix::net::UnixStream;

struct M(Vec<u8>);
impl M {
    fn new() -> M { M(Vec::with_capacity(512)) }
    fn align(&mut self, n: usize) { while self.0.len() % n != 0 { self.0.push(0) } }
    fn u8(&mut self, v: u8) { self.0.push(v) }
    fn u32(&mut self, v: u32) { self.align(4); self.0.extend_from_slice(&v.to_le_bytes()) }
    fn i32(&mut self, v: i32) { self.align(4); self.0.extend_from_slice(&v.to_le_bytes()) }
    fn str(&mut self, s: &str) { self.u32(s.len() as u32); self.0.extend_from_slice(s.as_bytes()); self.0.push(0) }
    fn sig(&mut self, s: &str) { self.u8(s.len() as u8); self.0.extend_from_slice(s.as_bytes()); self.0.push(0) }
    /// Array whose body is written by `f`; `elem_align` is the alignment of the element type.
    fn array(&mut self, elem_align: usize, f: impl FnOnce(&mut M)) {
        self.align(4);
        let len_at = self.0.len();
        self.0.extend_from_slice(&[0; 4]);
        self.align(elem_align);
        let start = self.0.len();
        f(self);
        let n = (self.0.len() - start) as u32;
        self.0[len_at..len_at + 4].copy_from_slice(&n.to_le_bytes());
    }
}

fn header(serial: u32, path: &str, iface: &str, member: &str, dest: &str, body_sig: &str, body: &[u8]) -> Vec<u8> {
    let mut m = M::new();
    m.u8(b'l');           // little endian
    m.u8(1);              // METHOD_CALL
    m.u8(0);              // flags
    m.u8(1);              // protocol version
    m.u32(body.len() as u32);
    m.u32(serial);
    m.array(8, |m| {
        let mut field = |m: &mut M, code: u8, sig: &str, val: &str| {
            m.align(8); m.u8(code); m.sig(sig); m.str(val);
        };
        field(&mut *m, 1, "o", path);
        field(&mut *m, 2, "s", iface);
        field(&mut *m, 3, "s", member);
        field(&mut *m, 6, "s", dest);
        if !body_sig.is_empty() {
            m.align(8); m.u8(8); m.sig("g"); m.sig(body_sig);
        }
    });
    m.align(8);
    m.0.extend_from_slice(body);
    m.0
}

fn main() {
    let t0 = std::time::Instant::now();
    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap();
    let path = addr.split(',').find_map(|p| p.strip_prefix("unix:path=")).unwrap().to_string();
    let mut s = UnixStream::connect(path).unwrap();

    // AUTH EXTERNAL: the uid, in ASCII, hex-encoded.
    let uid = unsafe { libc_getuid() }.to_string();
    let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
    s.write_all(format!("\0AUTH EXTERNAL {hex}\r\nBEGIN\r\n").as_bytes()).unwrap();
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).unwrap();
    assert!(buf[..n].starts_with(b"OK "), "auth: {:?}", String::from_utf8_lossy(&buf[..n]));

    // Hello is mandatory before any other message.
    let hello = header(1, "/org/freedesktop/DBus", "org.freedesktop.DBus", "Hello", "org.freedesktop.DBus", "", &[]);
    s.write_all(&hello).unwrap();

    // Notify(susssasa{sv}i)
    let mut b = M::new();
    b.str("cctab");                       // app_name
    b.u32(std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0)); // replaces_id
    b.str("utilities-terminal");          // app_icon
    b.str("claude-tabstatus");            // summary
    b.str("waiting for input");           // body
    b.array(4, |_m| {});                  // actions: as (empty)
    b.array(8, |m| {                      // hints: a{sv}
        m.align(8); m.str("urgency"); m.sig("y"); m.u8(1);
        m.align(8); m.str("desktop-entry"); m.sig("s"); m.str("org.kde.konsole");
    });
    b.i32(-1);                            // expire_timeout
    let msg = header(2, "/org/freedesktop/Notifications", "org.freedesktop.Notifications",
                     "Notify", "org.freedesktop.Notifications", "susssasa{sv}i", &b.0);
    s.write_all(&msg).unwrap();

    eprintln!("connect+auth+hello+notify written: {:?}", t0.elapsed());
    let t1 = std::time::Instant::now();
    // Read replies until we see METHOD_RETURN with reply_serial 2, then print the id.
    let mut acc = Vec::new();
    let mut tmp = [0u8; 4096];
    if std::env::var("CCTAB_NO_WAIT").is_err() {
        for _ in 0..64 {
            let n = s.read(&mut tmp).unwrap();
            acc.extend_from_slice(&tmp[..n]);
            if acc.len() >= 400 { break }
            if acc.iter().any(|_| false) { break }
            // Stop as soon as a 4-byte-body METHOD_RETURN is in the buffer.
            let mut i = 0usize; let mut done = false;
            while i + 16 <= acc.len() {
                let bl = u32::from_le_bytes(acc[i+4..i+8].try_into().unwrap()) as usize;
                let hl = u32::from_le_bytes(acc[i+12..i+16].try_into().unwrap()) as usize;
                if acc[i+1] == 2 && bl == 4 { done = true; break }
                i = i + ((16 + hl + 7) & !7) + bl;
            }
            if done { break }
        }
    }
    // Walk the reply stream: each message is 16 bytes of fixed header, a u32
    // header-array length, that array, padding to 8, then the body.
    eprintln!("wait for reply: {:?}", t1.elapsed());
    let mut i = 0usize;
    while i + 16 <= acc.len() {
        let body_len = u32::from_le_bytes(acc[i+4..i+8].try_into().unwrap()) as usize;
        let hdr_len = u32::from_le_bytes(acc[i+12..i+16].try_into().unwrap()) as usize;
        let body_at = i + ((16 + hdr_len + 7) & !7);
        if acc[i+1] == 2 && body_len == 4 && body_at + 4 <= acc.len() {
            let id = u32::from_le_bytes(acc[body_at..body_at+4].try_into().unwrap());
            println!("notification id = {id}");
        }
        i = body_at + body_len;
        if body_len == 0 && hdr_len == 0 { break }
    }
}
extern "C" { #[link_name = "getuid"] fn libc_getuid() -> u32; }
