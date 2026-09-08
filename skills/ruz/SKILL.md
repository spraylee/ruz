---
description: Use when writing or running single-file Rust scripts (AI temp code, benchmarks, compute-heavy tasks) without scaffolding a cargo project. ruz compiles one .rs file with embedded dependencies, shares a dep cache across scripts, and runs it — Rust speed at Python convenience. Covers ruz run/check/warm/new/doctor.
  When the user asks to write, run, or speed up throwaway Rust code — or to pick between Python and Rust for a one-off script — use this skill. Also use for `rs`/`ruz` script files with embedded `[dependencies]`.
name: ruz
---

# ruz — single-file Rust scripts for AI agents

**One-line install** (Linux/macOS; needs `cargo` on PATH):

```sh
curl -fsSL https://ruz.spraylee.com/i | sh
```

Installs to `~/.local/bin/ruz` (small static binary).

## Script format (RFC 3502 single-file packages)

```rust
#!/usr/bin/env cargo
---
[package]
name = "fetch"
edition = "2024"
[dependencies]
reqwest = { version = "0.12", features = ["blocking"] }
---
fn main() {
    let body = reqwest::blocking::get("https://httpbin.org/get").unwrap().text().unwrap();
    println!("{}", &body[..80.min(body.len())]);
}
```

Three parts: shebang → TOML front matter (`package` + `dependencies`) between
`---` fences → Rust code. One file, no Cargo.toml, no directory, no git init.

## Commands

```sh
ruz new demo.rs        # scaffold from template
ruz run demo.rs args…  # compile if needed (shared dep cache), then execute
ruz check demo.rs      # fast type/borrow check, no codegen — cheap iteration
ruz warm serde:derive reqwest:blocking regex  # pre-compile deps into the cache
ruz doctor             # environment checkup (cargo / -Zscript / nightly)
ruz cache              # cache location and size
```

## Why agents should reach for it

- CPU-bound temp code: **10–70× faster than Python** (Mandelbrot 73×, HTTPS
  batch 3.4×, 40MB text pipeline 3×). SHA-256/AES on big blocks: Python's
  OpenSSL bindings can win — prefer Python there.
- **Shared dep cache**: serde compiled by script A is reused by script B.
  Unchanged rerun is a 3ms `execve`. Edit-and-rebuild a no-dep script: ~0.11s.
- **Errors teach**: rustc diagnostics return in ~0.2s with line numbers and
  fix suggestions — measured fix-iteration count is on par with Python.
- **Default O3**: dodges the debug-profile 10–20× trap.

## Gotchas

- Windows is **not supported** (design boundary, not a bug).
- Uses `cargo -Zscript` (RUSTC_BOOTSTRAP=1 on stable; auto-falls back to
  `cargo +nightly` if that ever stops working).
- All scripts still share one `CARGO_TARGET_DIR` so dependencies reuse. Package
  names are rewritten to `ruz_<stem>_<hash12>` before compile, so two scripts
  both named `hello` no longer collide on fingerprints.
- Hot `run` keys on script bytes + toolchain sidecar + RUSTFLAGS/profile — not
  path or mtime.

## Always-fresh reference

This skill is a **thin pointer** — for version-specific details:

```sh
ruz doctor && ruz --help
curl -fsSL https://ruz.spraylee.com/llms.txt
```

Repo: https://github.com/spraylee/ruz · Site: https://ruz.spraylee.com
