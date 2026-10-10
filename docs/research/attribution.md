# Attribution and provenance for the macOS session-terminal route

What `src/sys/unix.rs`'s macOS code took from where, what each source's licence
says, and what was deliberately not taken.

> **This is a record of what each licence document says, not legal advice.** Every
> statement below is a reading of a file that was opened while writing this page;
> where the reading is an inference rather than a quotation, it says so. Nobody here
> is a lawyer, and a licence question that matters should be answered by one.

This program is **GPL-3.0-or-later** (`Cargo.toml`, `license = "GPL-3.0-or-later"`).
That is the direction every row below is measured against: what may flow *into* it.

## The four sources, and what crossed

| source | licence | what was taken | what was NOT |
|---|---|---|---|
| **`libc` 0.2** | `MIT OR Apache-2.0` | a dependency: `proc_pidfdinfo`, `proc_pidinfo`, `kill`, and the `vinfo_stat` / `vnode_info` / `vnode_info_path` declarations | — |
| **XNU** (`bsd/sys/proc_info.h`, `bsd/kern/proc_info.c`) | **APSL 2.0 — GPL-incompatible** | the **ABI** and the kernel's documented behaviour: field order, struct sizes, the flavour number, the `buffersize` gate, `CHECK_SAME_USER`, `fp_get_ftype(… DTYPE_VNODE, EBADF …)` | **no text, no comments, no transcription, no code** |
| **lsof** (`lib/dialects/darwin/dfile.c`) | a non-standard permissive Purdue/Abell licence | the **error taxonomy as a technique**: `nb <= 0` is an error, `ENOENT` means a revoked vnode, a short count is an error and not a short read | **no code**, and not its NUL-termination policy, which this tightened |
| **`darwin-libproc-sys` 0.2** | `Apache-2.0 OR MIT` | **nothing — no dependency taken.** See below | — |

### `libc` — MIT OR Apache-2.0, and one-way compatible

`~/.cargo/registry/src/…/libc-0.2.189/Cargo.toml` declares
`license = "MIT OR Apache-2.0"`, and the crate ships both `LICENSE-MIT` and
`LICENSE-APACHE`. Both are permissive, and the GNU project lists each as compatible
with GPL-3.0 — **one way**: MIT- or Apache-2.0-licensed code may be combined into a
GPL-3.0-or-later work, and the combination is distributed under the GPL. Nothing
flows back the other way, and nothing here asks it to.

It is a `[target.'cfg(target_os = "macos")'.dependencies]` entry, so no Linux or
Windows build resolves it at all, and it drags in **nothing**: this project's
`Cargo.lock` lists `libc` with no `dependencies` key. The crate declares exactly one,
`rustc-std-workspace-core`, and it is optional and reached only through the
`rustc-dep-of-std` feature, which nothing here enables.

### XNU — APSL 2.0, which is why nothing was copied

Apple's Public Source Licence 2.0 is a free software licence and is **not**
compatible with the GPL; the FSF lists it that way. So no Apple text may enter this
program: not a line of code, not a comment, not a transcribed doc string.

What did cross is the **ABI**: that `struct proc_fileinfo` is
`fi_openflags, fi_status, fi_offset, fi_type, fi_guardflags`; that
`struct vnode_fdinfowithpath` is `pfi` then `pvip`; that the flavour number is 2;
that the kernel returns `ENOMEM` when `buffersize` is below the flavour's own size;
that the permission gate is `CHECK_SAME_USER`; and that the flavour is served only
behind `fp_get_ftype(p, fd, DTYPE_VNODE, EBADF, &fp)`. An ABI is the set of facts
any conforming caller must match to interoperate at all — an interface, not an
expression of one. The Rust declarations and every comment around them in
`src/sys/unix.rs` are this project's own words.

Seven `const _: () = assert!(…)` items pin the required sizes and the two offsets
the code walks, and both Apple ABIs are `cargo check`ed in CI. Wrong sizes or
checked offsets fail the build; same-width field permutations can still pass.
Native arm64 process and PTY tests separately exercise the declarations, while
Intel remains compile-only. See [validation scope](../architecture.md#macos-validation).

### lsof — a non-standard permissive licence, and only the technique

`lib/dialects/darwin/dfile.c` carries **two** copyright notices — "Portions
Copyright 2005-2007 Apple Inc." and "Copyright 2005 Purdue Research Foundation",
written by Allan Nathanson (Apple) and Victor A. Abell (Purdue) — over a
hand-written four-clause permissive grant: use for any purpose, alter and
redistribute freely, subject to no-warranty, no-misrepresentation-of-origin,
mark-altered-versions, and do-not-remove-this-notice. It is permissive, it is not
OSI-templated, and the joint Apple copyright is a second reason to keep it at arm's
length.

Only the **technique** was used: that a byte count of `<= 0` is the error, that
`ENOENT` distinguishes a revoked vnode, and that a count short of the struct is a
hard error rather than a short read to tolerate. This project's implementation is
stricter than lsof's in one place and says so in the code: lsof forces a terminator
into the last byte of `vip_path` before calling `strlen`, whereas
`fd1_path_from_vnode` **refuses** an array with no terminator, because a truncated
path names a different file and `/dev/ttys004` cut short is still a writable
character device.

### `darwin-libproc-sys` — the crate that would have been the obvious shortcut

`darwin-libproc-sys` 0.2.0 declares exactly the structs this needed, under a
compatible `Apache-2.0 OR MIT`. It was **not** taken, and the reason is a field
order. Read out of the crate's own `src/pidinfo.rs` against XNU's header:

```
xnu bsd/sys/proc_info.h          darwin-libproc-sys 0.2.0 src/pidinfo.rs
struct vnode_info {              pub struct vnode_info {
    struct vinfo_stat vi_stat;       pub vi_stat: vinfo_stat,
    int               vi_type;       pub vi_type: libc::c_int,
    int               vi_pad;        pub vi_fsid: libc::fsid_t,   <-- swapped
    fsid_t            vi_fsid;       pub vi_pad: libc::c_int,     <-- swapped
};                               }
```

`fsid_t` is `[i32; 2]` and `c_int` is 4 bytes, so **both spellings are the same 152
bytes** and a size assert cannot tell them apart — only `vi_fsid`'s offset moves.
Nothing this project reads lives inside `vnode_info`, so the bug would not have bitten
here; that is luck, not a reason to depend on it. `libc`'s declaration matches the
header, is checked against Apple's real SDK by libc's own `libc-test` CI, and costs
zero transitive dependencies, so `libc` is the declaration used and this crate is a
worked example of the error class the asserts in `src/sys/unix.rs` are blind to.

## The other dependency, for completeness

`windows-sys` 0.61 is `MIT OR Apache-2.0`, the same one-way-compatible pair, behind
`[target.'cfg(windows)'.dependencies]`. It is #28's and is recorded here only so
this page is the whole list.

## How each row was checked

Every licence string above was read in this tree or in the local crate registry, not
recalled:

* `libc` and `windows-sys`: `license = "MIT OR Apache-2.0"` in each vendored
  `Cargo.toml`, plus `libc`'s shipped `LICENSE-MIT` and `LICENSE-APACHE`.
* `darwin-libproc-sys`: `license = "Apache-2.0 OR MIT"` in the crate archive fetched
  from crates.io, whose `src/pidinfo.rs` is quoted above.
* lsof and XNU: the licence headers and the sources themselves, read as text.
* This program's own licence: `Cargo.toml`.

The compatibility statements — that MIT and Apache-2.0 flow one way into
GPL-3.0-or-later, and that APSL 2.0 does not — are the FSF's published positions,
which is a reading of their licence list and not something this tree can verify.
