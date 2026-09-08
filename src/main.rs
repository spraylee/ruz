//! ruz — 单文件 Rust 脚本运行器（热路径内容哈希直跑）。

use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: &str = "0.2.0";
const SCHEME: &[u8] = b"ruz-run-cache-v2\0";
const BIN_CAP: usize = 200;
const HELP: &str = "\
ruz — single-file Rust script runner for AI agents.

Write Rust like you write Python: one file, embedded deps, zero scaffolding.

Usage:
    ruz run <file.rs> [args...]     # compile if needed, then execute
    ruz check <file.rs>             # fast type/borrow check, no codegen
    ruz warm <crate[:feature]...>   # pre-compile crates into the shared cache
    ruz new [name.rs]               # scaffold a script
    ruz cache                       # show cache location and size
    ruz doctor                      # diagnose environment (cargo / -Zscript)

Script format (RFC 3502 single-file packages):

    #!/usr/bin/env cargo
    ---
    [package]
    name = \"hello\"
    edition = \"2024\"
    [dependencies]
    regex = \"1\"
    ---
    fn main() { ... }

How it works:
    - cargo's built-in single-file packages (-Zscript)
    - hot run: content-hash cache + execve (no cargo)
    - RUSTC_BOOTSTRAP=1 lets stable cargo accept -Zscript; if that ever
      stops working, falls back to `cargo +nightly` automatically
    - shared CARGO_TARGET_DIR + CARGO_HOME: dependencies compiled once are
      reused by every later script
    - unique package names per script hash (no fingerprint collisions)
    - opt-level=3 by default (debug profile is 10-20x slower on compute)
";

const TPL: &str = "\
#!/usr/bin/env cargo
---
[package]
name = \"{name}\"
edition = \"2024\"
[dependencies]
---
fn main() {{
    // args: std::env::args().skip(1)
    println!(\"Hello from {name}!\");
}}
";

fn main() {
    std::process::exit(dispatch());
}

fn dispatch() -> i32 {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print!("{HELP}");
        return 0;
    };
    match cmd.as_str() {
        "-h" | "--help" => {
            print!("{HELP}");
            0
        }
        "-V" | "--version" => {
            println!("ruz {VERSION}");
            0
        }
        "run" => cmd_run(args.collect()),
        "check" => cmd_check(args.collect()),
        "warm" => cmd_warm(args.collect()),
        "new" => cmd_new(args.collect()),
        "cache" => cmd_cache(),
        "doctor" => cmd_doctor(),
        other => {
            die(&format!(
                "unknown command: {other} (try run/check/warm/new/cache/doctor)"
            ));
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("ruz: {msg}");
    std::process::exit(1);
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn cache_root() -> PathBuf {
    home_dir().join(".cache/ruz")
}

fn target_dir() -> PathBuf {
    cache_root().join("target")
}

fn mode_file() -> PathBuf {
    cache_root().join("cargo-mode")
}

fn fp_file() -> PathBuf {
    cache_root().join("toolchain.fp")
}

fn bin_root() -> PathBuf {
    cache_root().join("bin")
}

fn src_cache() -> PathBuf {
    cache_root().join("src")
}

fn default_cargo_home() -> PathBuf {
    cache_root().join("cargo-home")
}

fn ensure_dirs() {
    let _ = fs::create_dir_all(target_dir());
    let _ = fs::create_dir_all(bin_root());
    let _ = fs::create_dir_all(src_cache());
}

fn nonce() -> String {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}.{}", std::process::id(), ns)
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        nonce()
    ));
    fs::write(&tmp, data)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn is_executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.len() > 0 && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn mtime_ns(meta: &fs::Metadata) -> u64 {
    let sec = meta.mtime();
    let nsec = meta.mtime_nsec();
    if sec < 0 {
        return 0;
    }
    (sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(nsec.max(0) as u64)
}

struct BinStat {
    mtime_ns: u64,
    size: u64,
}

fn stat_bin(path: &Path) -> io::Result<BinStat> {
    let meta = fs::metadata(path)?;
    Ok(BinStat {
        mtime_ns: mtime_ns(&meta),
        size: meta.len(),
    })
}

fn read_mode() -> Option<String> {
    let s = fs::read_to_string(mode_file()).ok()?;
    let m = s.trim();
    if m == "stable" || m == "nightly" {
        Some(m.to_string())
    } else {
        None
    }
}

fn write_mode(mode: &str) {
    let _ = atomic_write(&mode_file(), mode.as_bytes());
}

fn apply_cargo_env(cmd: &mut Command) {
    cmd.env("CARGO_TARGET_DIR", target_dir());
    cmd.env("RUSTC_BOOTSTRAP", "1");
    if env::var_os("CARGO_HOME").is_none() {
        cmd.env("CARGO_HOME", default_cargo_home());
    }
    if env::var_os("CARGO_PROFILE_DEV_OPT_LEVEL").is_none() {
        cmd.env("CARGO_PROFILE_DEV_OPT_LEVEL", "3");
    }
    if env::var_os("CARGO_PROFILE_DEV_DEBUG").is_none() {
        cmd.env("CARGO_PROFILE_DEV_DEBUG", "0");
    }
}

struct CargoOutcome {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn invoke_cargo(mode: &str, args: &[&str], capture: bool) -> io::Result<CargoOutcome> {
    let mut cmd = Command::new("cargo");
    apply_cargo_env(&mut cmd);
    if mode == "nightly" {
        cmd.arg("+nightly");
    }
    cmd.args(args);
    if capture {
        let out = cmd.output()?;
        Ok(CargoOutcome {
            status: out.status,
            stdout: out.stdout,
            stderr: out.stderr,
        })
    } else {
        let status = cmd.status()?;
        Ok(CargoOutcome {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

fn run_cargo(args: &[&str], capture: bool) -> io::Result<CargoOutcome> {
    let mode = read_mode().unwrap_or_else(|| "stable".to_string());
    let result = invoke_cargo(&mode, args, capture)?;
    if !result.status.success() && mode == "stable" && capture {
        let err = String::from_utf8_lossy(&result.stderr).to_ascii_lowercase();
        if err.contains("nightly") || err.contains("-zscript") {
            write_mode("nightly");
            return invoke_cargo("nightly", args, capture);
        }
    }
    Ok(result)
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn require_script(argv: &[String], usage: &str) -> PathBuf {
    let Some(script) = argv.first() else {
        die(&format!("usage: {usage}"));
    };
    if !Path::new(script).is_file() {
        die(&format!("file not found: {script}"));
    }
    PathBuf::from(script)
}

fn abs_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        match env::current_dir() {
            Ok(cwd) => {
                let joined = cwd.join(path);
                fs::canonicalize(&joined).unwrap_or(joined)
            }
            Err(_) => path.to_path_buf(),
        }
    }
}

fn file_stem_raw(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "s".to_string())
}

fn sanitize_stem(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if out.len() >= 16 {
            break;
        }
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "s".to_string()
    } else {
        out
    }
}

fn parse_quoted(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let bytes = rest.as_bytes();
    let q = *bytes.first()?;
    if q != b'"' && q != b'\'' {
        return None;
    }
    let body = &rest[1..];
    let end = body.find(q as char)?;
    Some(body[..end].to_string())
}

fn is_name_assignment(line: &str) -> bool {
    let t = line.trim();
    let Some(after) = t.strip_prefix("name") else {
        return false;
    };
    let after = after.trim_start();
    after.starts_with('=')
}

fn package_section(toml: &str) -> &str {
    if let Some(idx) = toml.find("[package]") {
        let rest = &toml[idx + "[package]".len()..];
        match rest.find("\n[") {
            Some(end) => &rest[..end],
            None => rest,
        }
    } else {
        ""
    }
}

fn field_in_package(toml: &str, field: &str) -> Option<String> {
    for line in package_section(toml).lines() {
        let t = line.trim();
        let Some(after) = t.strip_prefix(field) else {
            continue;
        };
        let after = after.trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        if let Some(v) = parse_quoted(after) {
            return Some(v);
        }
    }
    None
}

/// 拆 shebang / front matter / 源码。toml 为 `---` 之间的内容（不含围栏）。
fn split_front_matter(src: &str) -> Option<(&str, &str, &str)> {
    let mut offset = 0usize;
    if src.starts_with("#!") {
        offset = src.find('\n')? + 1;
    }
    let body = &src[offset..];
    let first_end = body.find('\n').unwrap_or(body.len());
    let first = body[..first_end].trim_end_matches(['\r', '\n']);
    if first != "---" {
        return None;
    }
    let toml_start = offset + first_end + usize::from(first_end < body.len());
    let rest = &src[toml_start..];
    let mut rel = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let toml = &rest[..rel];
            let after = &rest[rel + line.len()..];
            return Some((&src[..offset], toml, after));
        }
        rel += line.len();
    }
    None
}

fn rewrite_package_name(src: &str, new_name: &str) -> Option<String> {
    let (prefix, toml, code) = split_front_matter(src)?;
    let pkg = package_section(toml);
    if pkg.is_empty() {
        return None;
    }
    let pkg_start = toml.find("[package]")? + "[package]".len();
    let pkg_in_toml = &toml[pkg_start..pkg_start + pkg.len()];
    let mut found = false;
    let mut new_pkg = String::new();
    for line in pkg_in_toml.split_inclusive('\n') {
        if !found && is_name_assignment(line) {
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            new_pkg.push_str(&indent);
            new_pkg.push_str("name = \"");
            new_pkg.push_str(new_name);
            new_pkg.push('"');
            if line.ends_with("\r\n") {
                new_pkg.push_str("\r\n");
            } else if line.ends_with('\n') {
                new_pkg.push('\n');
            }
            found = true;
        } else {
            new_pkg.push_str(line);
        }
    }
    if !found {
        return None;
    }
    let mut out = String::with_capacity(src.len() + new_name.len() + 8);
    out.push_str(prefix);
    out.push_str("---\n");
    out.push_str(&toml[..pkg_start]);
    out.push_str(&new_pkg);
    out.push_str(&toml[pkg_start + pkg.len()..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(code);
    Some(out)
}

fn inject_manifest(src: &str, new_name: &str) -> String {
    let block = format!("---\n[package]\nname = \"{new_name}\"\n---\n");
    if src.starts_with("#!") {
        if let Some((line, rest)) = src.split_once('\n') {
            return format!("{line}\n{block}{rest}");
        }
    }
    format!("{block}{src}")
}

struct ScriptMeta {
    name: String,
    version: String,
    has_manifest: bool,
    has_name_line: bool,
}

fn script_meta(src: &str, path: &Path) -> ScriptMeta {
    let stem = file_stem_raw(path);
    if let Some((_, toml, _)) = split_front_matter(src) {
        let name = field_in_package(toml, "name");
        let version = field_in_package(toml, "version");
        ScriptMeta {
            has_name_line: name.is_some(),
            name: name.unwrap_or(stem),
            version: version.unwrap_or_else(|| "0.0.0".to_string()),
            has_manifest: true,
        }
    } else {
        ScriptMeta {
            name: stem,
            version: "0.0.0".to_string(),
            has_manifest: false,
            has_name_line: false,
        }
    }
}

fn effective_profile() -> (String, String, String) {
    let rustflags = env::var("RUSTFLAGS").unwrap_or_default();
    let opt = env::var("CARGO_PROFILE_DEV_OPT_LEVEL").unwrap_or_else(|_| "3".to_string());
    let debug = env::var("CARGO_PROFILE_DEV_DEBUG").unwrap_or_else(|_| "0".to_string());
    (rustflags, opt, debug)
}

fn rustc_commit(rustc: &Path) -> Result<String, String> {
    let out = Command::new(rustc)
        .arg("-vV")
        .output()
        .map_err(|e| format!("failed to run rustc -vV: {e}"))?;
    if !out.status.success() {
        return Err("rustc -vV failed".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(hash) = line.strip_prefix("commit-hash: ") {
            return Ok(hash.trim().to_string());
        }
    }
    Err("rustc -vV: commit-hash missing".to_string())
}

fn cargo_version_line(cargo: &Path) -> Result<String, String> {
    let out = Command::new(cargo)
        .arg("--version")
        .output()
        .map_err(|e| format!("failed to run cargo --version: {e}"))?;
    if !out.status.success() {
        return Err("cargo --version failed".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn parse_fp_line(line: &str) -> Option<[&str; 9]> {
    let mut parts = [""; 9];
    let mut it = line.split('|');
    for slot in &mut parts {
        *slot = it.next()?;
    }
    if it.next().is_some() {
        return None;
    }
    Some(parts)
}

fn toolchain_fingerprint() -> Result<String, String> {
    let rustc =
        find_in_path("rustc").ok_or_else(|| "rustc not found on PATH (run ruz doctor)".to_string())?;
    let cargo =
        find_in_path("cargo").ok_or_else(|| "cargo not found on PATH (run ruz doctor)".to_string())?;
    let mode = read_mode().unwrap_or_else(|| "stable".to_string());
    let rs = stat_bin(&rustc).map_err(|e| format!("stat rustc: {e}"))?;
    let cs = stat_bin(&cargo).map_err(|e| format!("stat cargo: {e}"))?;

    if let Ok(existing) = fs::read_to_string(fp_file()) {
        let existing = existing.trim_end_matches('\n');
        if let Some(p) = parse_fp_line(existing) {
            if p[0] == rustc.to_string_lossy().as_ref()
                && p[1] == rs.mtime_ns.to_string()
                && p[2] == rs.size.to_string()
                && p[4] == cargo.to_string_lossy().as_ref()
                && p[5] == cs.mtime_ns.to_string()
                && p[6] == cs.size.to_string()
                && p[8] == mode
            {
                return Ok(existing.to_string());
            }
        }
    }

    let commit = rustc_commit(&rustc)?;
    let cver = cargo_version_line(&cargo)?;
    let line = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        rustc.display(),
        rs.mtime_ns,
        rs.size,
        commit,
        cargo.display(),
        cs.mtime_ns,
        cs.size,
        cver,
        mode
    );
    let _ = atomic_write(&fp_file(), line.as_bytes());
    Ok(line)
}

fn cache_key(script: &[u8], fp: &str, rustflags: &str, opt: &str, debug: &str) -> String {
    let mut h = Sha256::new();
    h.update(SCHEME);
    put_len(&mut h, script);
    put_len(&mut h, fp.as_bytes());
    put_len(&mut h, rustflags.as_bytes());
    put_len(&mut h, opt.as_bytes());
    put_len(&mut h, debug.as_bytes());
    hex_lower(&h.finalize())
}

fn put_len(h: &mut Sha256, data: &[u8]) {
    h.update(&(data.len() as u64).to_le_bytes());
    h.update(data);
}

fn exe_for_key(key: &str) -> PathBuf {
    bin_root().join(key).join("exe")
}

fn meta_for_key(key: &str) -> PathBuf {
    bin_root().join(key).join("meta")
}

fn exec_cached(exe: &Path, args: &[String], pkg_name: &str, pkg_ver: &str, manifest_dir: &Path) -> ! {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    cmd.env("CARGO_PKG_NAME", pkg_name);
    cmd.env("CARGO_PKG_VERSION", pkg_ver);
    cmd.env("CARGO_MANIFEST_DIR", manifest_dir);
    let err = cmd.exec();
    die(&format!("execve failed: {err}"));
}

fn publish_exe(artifact: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_file_name(format!("exe.{}.tmp", nonce()));
    if fs::hard_link(artifact, &tmp).is_err() {
        fs::copy(artifact, &tmp)?;
        let mut perms = fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmp, perms)?;
    }
    match fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn find_artifact(pkg_name: Option<&str>, started: SystemTime) -> Option<PathBuf> {
    let debug_dir = target_dir().join("debug");
    if let Some(name) = pkg_name {
        let primary = debug_dir.join(name);
        if is_executable(&primary) {
            return Some(primary);
        }
    }
    let slop = started
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap_or(started);
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let entries = fs::read_dir(&debug_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() == 0 || meta.permissions().mode() & 0o111 == 0 {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("d") {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if mtime < slop {
            continue;
        }
        let take = match &best {
            None => true,
            Some((t, _)) => mtime >= *t,
        };
        if take {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

fn lru_evict() {
    let root = bin_root();
    let Ok(rd) = fs::read_dir(&root) else {
        return;
    };
    let mut entries: Vec<(SystemTime, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let exe = path.join("exe");
        let mtime = fs::metadata(&exe)
            .and_then(|m| m.modified())
            .or_else(|_| e.metadata().and_then(|m| m.modified()))
            .unwrap_or(UNIX_EPOCH);
        entries.push((mtime, path));
    }
    if entries.len() <= BIN_CAP {
        return;
    }
    entries.sort_by_key(|(t, _)| *t);
    let excess = entries.len() - BIN_CAP;
    for (_, path) in entries.into_iter().take(excess) {
        let _ = fs::remove_dir_all(path);
    }
}

fn cmd_run(argv: Vec<String>) -> i32 {
    let script = require_script(&argv, "ruz run <file.rs> [args...]");
    let user_args = &argv[1..];
    ensure_dirs();

    if find_in_path("rustc").is_none() {
        die("rustc not found on PATH (run ruz doctor)");
    }
    if find_in_path("cargo").is_none() {
        die("cargo not found on PATH (run ruz doctor)");
    }

    let bytes = match fs::read(&script) {
        Ok(b) => b,
        Err(e) => die(&format!("failed to read {}: {e}", script.display())),
    };
    let src = String::from_utf8_lossy(&bytes);
    let meta = script_meta(&src, &script);
    let script_abs = abs_path(&script);
    let manifest_dir = script_abs.parent().unwrap_or(script_abs.as_path()).to_path_buf();

    let fp = match toolchain_fingerprint() {
        Ok(s) => s,
        Err(e) => die(&e),
    };
    let (rustflags, opt, debug) = effective_profile();
    let key = cache_key(&bytes, &fp, &rustflags, &opt, &debug);
    let exe = exe_for_key(&key);

    if is_executable(&exe) {
        exec_cached(&exe, user_args, &meta.name, &meta.version, &manifest_dir);
    }

    compile_and_run(
        &script,
        &src,
        &meta,
        &key,
        user_args,
        &manifest_dir,
        &exe,
    )
}

enum NameAction {
    Rewritten,
    Injected,
    Unchanged,
}

fn prepare_source(src: &str, new_name: &str, has_manifest: bool, has_name_line: bool) -> (String, NameAction) {
    if has_manifest && has_name_line {
        if let Some(rewritten) = rewrite_package_name(src, new_name) {
            return (rewritten, NameAction::Rewritten);
        }
    }
    if !has_manifest {
        return (inject_manifest(src, new_name), NameAction::Injected);
    }
    (src.to_string(), NameAction::Unchanged)
}

fn compile_and_run(
    script: &Path,
    src: &str,
    meta: &ScriptMeta,
    key: &str,
    user_args: &[String],
    manifest_dir: &Path,
    exe: &Path,
) -> i32 {
    let key12 = &key[..12.min(key.len())];
    let stem = sanitize_stem(&file_stem_raw(script));
    let pkg = format!("ruz_{stem}_{key12}");
    let (body, action) = prepare_source(src, &pkg, meta.has_manifest, meta.has_name_line);

    let script_dir = script.parent().unwrap_or(Path::new("."));
    let local_copy = script_dir.join(format!(".ruz.{key12}.rs"));
    let (copy_path, is_local) = match atomic_write(&local_copy, body.as_bytes()) {
        Ok(()) => (local_copy, true),
        Err(_) => {
            eprintln!(
                "ruz: script directory not writable; compiling from cache copy (relative include!/include_str! may fail)"
            );
            let fallback = src_cache().join(format!("{key}.rs"));
            if let Err(e) = atomic_write(&fallback, body.as_bytes()) {
                die(&format!("failed to write compile copy: {e}"));
            }
            (fallback, false)
        }
    };

    let started = SystemTime::now();
    let copy_s = copy_path.to_string_lossy().into_owned();
    let outcome = match run_cargo(&["-Zscript", "build", "--manifest-path", &copy_s], false) {
        Ok(o) => o,
        Err(e) => {
            if is_local {
                let _ = fs::remove_file(&copy_path);
            }
            die(&format!("failed to spawn cargo: {e}"));
        }
    };
    if !outcome.status.success() {
        return exit_code(outcome.status);
    }

    let want = match action {
        NameAction::Rewritten | NameAction::Injected => Some(pkg.as_str()),
        NameAction::Unchanged => None,
    };
    let Some(artifact) = find_artifact(want, started) else {
        if is_local {
            let _ = fs::remove_file(&copy_path);
        }
        die("compiled but could not locate cargo artifact");
    };
    if let Err(e) = publish_exe(&artifact, exe) {
        if is_local {
            let _ = fs::remove_file(&copy_path);
        }
        die(&format!("failed to publish cached exe: {e}"));
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta_line = format!("{}\t{}\t{ts}\n", meta.name, abs_path(script).display());
    let _ = atomic_write(&meta_for_key(key), meta_line.as_bytes());
    if is_local {
        let _ = fs::remove_file(&copy_path);
    }
    lru_evict();
    exec_cached(exe, user_args, &meta.name, &meta.version, manifest_dir);
}

fn cmd_check(argv: Vec<String>) -> i32 {
    let script = require_script(&argv, "ruz check <file.rs>");
    ensure_dirs();
    let path = script.to_string_lossy().into_owned();
    let outcome = match run_cargo(&["-Zscript", "check", "--manifest-path", &path], true) {
        Ok(o) => o,
        Err(e) => die(&format!("failed to spawn cargo: {e}")),
    };
    if outcome.status.success() {
        println!("ok: {} compiles clean", argv[0]);
        0
    } else {
        let err = if outcome.stderr.is_empty() {
            &outcome.stdout
        } else {
            &outcome.stderr
        };
        let _ = io::stderr().write_all(err);
        exit_code(outcome.status)
    }
}

fn cmd_warm(argv: Vec<String>) -> i32 {
    if argv.is_empty() {
        die("usage: ruz warm <crate[:feature]...>");
    }
    ensure_dirs();
    let mut deps_lines = Vec::new();
    for tok in &argv {
        if let Some((crate_name, feat)) = tok.split_once(':') {
            deps_lines.push(format!(
                "{crate_name} = {{ version = \"*\", features = [\"{feat}\"] }}"
            ));
        } else {
            deps_lines.push(format!("{tok} = \"*\""));
        }
    }
    let src = format!(
        "#!/usr/bin/env cargo\n---\n[package]\nname = \"warmup\"\n[dependencies]\n{}\n---\nfn main() {{}}\n",
        deps_lines.join("\n")
    );
    let path = cache_root().join(format!("warmup-{}.rs", nonce()));
    if let Err(e) = atomic_write(&path, src.as_bytes()) {
        die(&format!("failed to write warmup script: {e}"));
    }
    println!("warming: {} (first time can take a while)", argv.join(", "));
    let path_s = path.to_string_lossy().into_owned();
    let outcome = match run_cargo(&["-Zscript", &path_s], false) {
        Ok(o) => o,
        Err(e) => {
            let _ = fs::remove_file(&path);
            die(&format!("failed to spawn cargo: {e}"));
        }
    };
    let _ = fs::remove_file(&path);
    if outcome.status.success() {
        println!("warm ok - future scripts using these crates start fast");
    }
    exit_code(outcome.status)
}

fn cmd_new(argv: Vec<String>) -> i32 {
    let mut name = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "script.rs".to_string());
    if !name.ends_with(".rs") {
        name.push_str(".rs");
    }
    if Path::new(&name).exists() {
        die(&format!("refusing to overwrite {name}"));
    }
    let pkg = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().replace('-', "_"))
        .unwrap_or_else(|| "script".to_string());
    let body = TPL.replace("{name}", &pkg);
    if let Err(e) = fs::write(&name, body) {
        die(&format!("failed to write {name}: {e}"));
    }
    println!("created {name} - run with: ruz run {name}");
    0
}

fn du(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.metadata() {
                Ok(meta) if meta.is_dir() => stack.push(p),
                Ok(meta) => total = total.saturating_add(meta.len()),
                Err(_) => {}
            }
        }
    }
    total
}

fn format_size(n: u64) -> String {
    let x = n as f64;
    if x >= 1_000_000_000.0 {
        format!("{:.2} GB", x / 1_000_000_000.0)
    } else if x >= 1_000_000.0 {
        format!("{:.2} MB", x / 1_000_000.0)
    } else if x >= 1_000.0 {
        format!("{:.2} KB", x / 1_000.0)
    } else {
        format!("{n} B")
    }
}

fn cmd_cache() -> i32 {
    let root = cache_root();
    println!("cache root : {}", root.display());
    let bin = root.join("bin");
    let mut bin_n = 0usize;
    if let Ok(rd) = fs::read_dir(&bin) {
        bin_n = rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).count();
    }
    println!("  {:<10}: {} entries, {}", "bin", bin_n, format_size(du(&bin)));
    for sub in ["target", "cargo-home"] {
        let p = root.join(sub);
        println!("  {sub:<10}: {}", format_size(du(&p)));
    }
    println!(
        "cargo mode : {}",
        read_mode().unwrap_or_else(|| "stable (unprobed)".to_string())
    );
    0
}

fn cmd_doctor() -> i32 {
    let exe = env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "ruz".to_string());
    println!("ruz        : v{VERSION} ({exe})");
    let Some(cargo_bin) = find_in_path("cargo") else {
        println!("cargo      : MISSING (install with https://rustup.rs)");
        return 1;
    };
    match Command::new(&cargo_bin).arg("--version").output() {
        Ok(out) => {
            println!(
                "cargo      : {} ({})",
                String::from_utf8_lossy(&out.stdout).trim(),
                cargo_bin.display()
            );
        }
        Err(_) => {
            println!("cargo      : MISSING (install with https://rustup.rs)");
            return 1;
        }
    }

    let probe = invoke_cargo("stable", &["-Zscript", "--version"], true);
    match probe {
        Ok(r) if r.status.success() => {
            println!("stable -Zscript: OK (RUSTC_BOOTSTRAP hack works)");
            if read_mode().as_deref() != Some("stable") {
                write_mode("stable");
            }
        }
        Ok(_) | Err(_) => {
            match invoke_cargo("nightly", &["-Zscript", "--version"], true) {
                Ok(r2) if r2.status.success() => {
                    println!("stable -Zscript: rejected, nightly fallback: OK");
                    write_mode("nightly");
                }
                _ => {
                    println!(
                        "stable -Zscript: rejected; nightly: MISSING (rustup toolchain install nightly)"
                    );
                    return 1;
                }
            }
        }
    }
    println!("cache      : {}", cache_root().display());
    0
}

// ----- SHA-256（std-only，FIPS 180-4）-----

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    filled: usize,
    total: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            filled: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.saturating_add(data.len() as u64);
        if self.filled > 0 {
            let need = 64 - self.filled;
            if data.len() < need {
                self.buf[self.filled..self.filled + data.len()].copy_from_slice(data);
                self.filled += data.len();
                return;
            }
            self.buf[self.filled..].copy_from_slice(&data[..need]);
            self.process_block(&self.buf.clone());
            self.filled = 0;
            data = &data[need..];
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.process_block(&block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.filled = data.len();
        }
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate().take(16) {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = self.h[0];
        let mut b = self.h[1];
        let mut c = self.h[2];
        let mut d = self.h[3];
        let mut e = self.h[4];
        let mut f = self.h[5];
        let mut g = self.h[6];
        let mut hh = self.h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
        self.h[5] = self.h[5].wrapping_add(f);
        self.h[6] = self.h[6].wrapping_add(g);
        self.h[7] = self.h[7].wrapping_add(hh);
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total.saturating_mul(8);
        self.update(&[0x80]);
        if self.filled > 56 {
            let zeros = [0u8; 64];
            self.update(&zeros[..64 - self.filled]);
        }
        let pad = 56 - self.filled;
        if pad > 0 {
            let zeros = [0u8; 56];
            self.update(&zeros[..pad]);
        }
        self.update(&bit_len.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex_lower(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_abc() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_long_loop() {
        // NIST 百万次 'a' + 56 字节向量
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn front_matter_name_double_quotes() {
        let src = "#!/usr/bin/env cargo\n---\n[package]\nname = \"hello\"\nedition = \"2024\"\n---\nfn main() {}\n";
        let (_, toml, _) = split_front_matter(src).unwrap();
        assert_eq!(field_in_package(toml, "name").as_deref(), Some("hello"));
        let rewritten = rewrite_package_name(src, "ruz_hello_abc123def456").unwrap();
        let (_, toml2, _) = split_front_matter(&rewritten).unwrap();
        assert_eq!(
            field_in_package(toml2, "name").as_deref(),
            Some("ruz_hello_abc123def456")
        );
        assert!(rewritten.contains("fn main() {}"));
    }

    #[test]
    fn front_matter_name_single_quotes() {
        let src = "---\n[package]\nname = 'probe'\nversion = '1.2.3'\n---\nfn main() {}\n";
        let (_, toml, _) = split_front_matter(src).unwrap();
        assert_eq!(field_in_package(toml, "name").as_deref(), Some("probe"));
        assert_eq!(field_in_package(toml, "version").as_deref(), Some("1.2.3"));
        let rewritten = rewrite_package_name(src, "ruz_probe_deadbeefcafe").unwrap();
        let (_, toml2, _) = split_front_matter(&rewritten).unwrap();
        assert_eq!(
            field_in_package(toml2, "name").as_deref(),
            Some("ruz_probe_deadbeefcafe")
        );
    }

    #[test]
    fn front_matter_no_name_line_not_rewritten() {
        let src = "---\n[package]\nedition = \"2024\"\n---\nfn main() {}\n";
        assert!(rewrite_package_name(src, "ruz_x_aaaaaaaaaaaa").is_none());
        let meta = script_meta(src, Path::new("foo.rs"));
        assert!(meta.has_manifest);
        assert!(!meta.has_name_line);
        assert_eq!(meta.name, "foo");
        assert_eq!(meta.version, "0.0.0");
    }

    #[test]
    fn inject_manifest_after_shebang() {
        let src = "#!/usr/bin/env cargo\nfn main() {}\n";
        let out = inject_manifest(src, "ruz_s_0123456789ab");
        assert!(out.starts_with("#!/usr/bin/env cargo\n---\n[package]\nname = \"ruz_s_0123456789ab\"\n---\n"));
        assert!(out.contains("fn main() {}"));
        assert!(split_front_matter(&out).is_some());
    }

    #[test]
    fn sanitize_and_format_size() {
        assert_eq!(sanitize_stem("Hello-World"), "hello_world");
        assert_eq!(sanitize_stem(""), "s");
        assert_eq!(sanitize_stem("abcdefghijklmnopqrstuvwxyz"), "abcdefghijklmnop");
        assert_eq!(format_size(50_000_000), "50.00 MB");
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(12_345), "12.35 KB");
    }
}
