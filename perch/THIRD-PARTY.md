# Third-party code in the perch

The perch is MIT (`Cargo.toml`). This file is where its dependencies that carry a notice of their own are listed, with the licence text a
redistribution has to keep. The two the terminal host adds (slice T1a) are:

| Crate | Version | Licence | Used for | Text |
|---|---|---|---|---|
| `portable-pty` | `=0.9.0` | MIT, Copyright (c) 2018 Wez Furlong | the PTY pair, the login shell on it as a session leader, the master's reader and writer | below |
| `alacritty_terminal` | `=0.26.0` | **Apache-2.0 only**, Copyright 2020 The Alacritty Project | the screen kept beside the raw ring: `Term` fed through its `vte` parser, read back as text and as a snapshot | [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt) |

Both are pinned exactly in `Cargo.toml` and recorded in `Cargo.lock`; a bump is a reviewed change, and `tests/deps_smoke.rs` runs both against
a real shell so a bump that changes what the terminal host relies on fails there. `alacritty_terminal` is built with `default-features = false`,
which leaves out its optional `serde`; no other crate of the Alacritty project is used.

`LICENSES/Apache-2.0.txt` is the `LICENSE-APACHE` file that ships inside the `alacritty_terminal` 0.26.0 crate, copied byte for byte
(sha256 `8884289e3969c089cd556c53fa1d3f51d86b29e52a8c1d660a55ce8292986910`), with the Alacritty project's own appendix as the crate has it.

## portable-pty, MIT

```text
MIT License

Copyright (c) 2018 Wez Furlong

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## What they pull in

The two crates added 45 packages to `Cargo.lock`. Twenty-two of them are compiled into a Linux build (`cargo tree --target x86_64-unknown-linux-musl`):
`nix`, `filedescriptor`, `serial2`, `shell-words`, `downcast-rs`, `thiserror` (and its macro crate), `vte`, `polling`, `rustix-openpty`,
`signal-hook`, `unicode-width`, `cursor-icon`, `home`, `parking_lot`, `parking_lot_core`, `lock_api`, `scopeguard`, `arrayvec`, `bitflags` 1.x and the
two above. Every one of those carries MIT, Apache-2.0 or a dual licence that includes one of them (`serial2` is BSD-2-Clause or Apache-2.0,
`cursor-icon` adds Zlib, `nix` is MIT), read from the crates' own manifests.

The other twenty-three are in the lock because cargo records every platform in it, and are not compiled into the Linux binary the release
builds: the Windows ones (`winapi` and its two gnu import crates, `windows-sys`, `windows-targets`, eight `windows_*` crates, `winreg`, `miow`,
`piper`, `shared_library`), the ones for other systems (`hermit-abi`, `redox_syscall`) and four helpers that only another target pulls in
(`cfg_aliases`, `concurrent-queue`, `crossbeam-utils`, `futures-io`). Their sources are not in the offline cache, so their manifests were not read.

None of the licences that were read is GPL, and `tests/gpl_boundary.rs` fails if any package named `aoide*`, the GPL crates this one must
never link, appears in the lock.
