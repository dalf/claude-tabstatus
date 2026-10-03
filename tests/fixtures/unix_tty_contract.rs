//! Test-only observer of the native API's return values, which the silent CLI
//! deliberately hides. Compile the production backend, not a copy of its guards.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::Path;

type FileId = (u64, u128);
#[path = "../../src/sys/unix.rs"]
mod unix;

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3);
    match args[1].as_str() {
        "session" => {
            let f = unix::session_tty(OsStr::new(&args[2]));
            println!("{}", if f.is_some() { "some" } else { "none" });
        }
        "write" => match unix::write_tty(Path::new(&args[2]), b"probe") {
            Ok(wrote) => println!("{wrote}"),
            Err(error) => println!("error:{}", error.raw_os_error().expect("native errno")),
        },
        _ => panic!("unknown probe"),
    }
}
