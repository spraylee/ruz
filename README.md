# ruz

让 AI（和你）写 Rust 脚本像写 Python 一样：**单文件、内嵌依赖、零脚手架**。

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

```sh
ruz run fetch.rs
```

## 为什么

AI 写临时代码默认用 Python，因为快——但计算密集任务被 CPython 解释器拖累 10~70 倍。ruz 让「写 Rust」的成本降到和 Python 一样低：

- **单文件**：依赖清单内嵌在脚本里（RFC 3502 格式，未来 cargo stable 原生支持）
- **免项目**：不建目录、不写 Cargo.toml、不 git init
- **共享缓存**：所有脚本共用一个依赖编译缓存，A 脚本编过的 serde，B 脚本直接复用
- **热路径直跑**：内容哈希命中后 `execve` 缓存二进制，不再进 cargo
- **默认 O3**：躲开 debug profile 的 10-20x 性能陷阱
- **报错即教学**：rustc 诊断 0.2s 返回，带行号 + 修复建议，AI 迭代闭环极短

## 安装

一行 curl（Linux / macOS，需要 cargo）：

```sh
curl -fsSL https://ruz.spraylee.com/i | sh
```

或从 GitHub Release：

```sh
curl -fsSL https://github.com/spraylee/ruz/releases/latest/download/bootstrap.sh | sh
```

## 用法

```sh
ruz new demo.rs        # 生成模板
ruz run demo.rs args…  # 命中缓存则直跑，否则编译再执行
ruz check demo.rs      # 快速类型/借用检查，不产出二进制
ruz warm serde:derive reqwest:blocking regex  # 预热常用依赖
ruz doctor             # 环境体检
ruz cache              # 缓存位置与大小
```

## 实测（Xeon 8255C，Linux，rustc/cargo 1.98.0，2026-09-08）

计算任务（相对 CPython，数字沿用同机 2026-09 对照）：

| 场景 | python3 | ruz (O3) | 赢家 |
|---|---|---|---|
| Mandelbrot 纯计算 | 6.92s | **0.095s** | ruz 73x |
| 网络批量 20 HTTPS | 20.0s | **5.8s** | ruz 3.4x |
| 40MB 文本流水线 | 391ms | **130ms** | ruz 3x |
| SHA-256 / AES 大块 | **57ms** | 110ms | python（OpenSSL）|

启动器热路径（无依赖 `hello`，`scripts/selftest.sh` 本机实测）：

| 档 | 墙钟 | 说明 |
|---|---|---|
| 首次编译 | **192ms** | `cargo -Zscript build` + 登记缓存 + execve |
| 改一行重编 | **108ms** | 内容哈希变了，再走 cargo |
| 热直跑 p50 | **3.04ms** | 20 次；strace 只有一次对缓存二进制的 execve，无 cargo |
| v0.1.1 热路径（对照） | 71ms | 每次 Python + `cargo -Zscript` |

**AI 迭代循环**：没改文件再跑是 3ms 级；改一行重编约 0.11s。修复轮次实测与 Python 打平（rustc 报错自带答案）。

## 架构（v0.2.0）

`ruz` 是一份 std-only 的小静态二进制，不再经过 Python。

- **内容哈希主键**：`sha256(scheme ‖ 脚本全文 ‖ 工具链边车 ‖ RUSTFLAGS/opt/debug)`。改脚本任意字节、换 rustc、改 `RUSTFLAGS` 都会换 key。
- **工具链边车**：`~/.cache/ruz/toolchain.fp` 缓存 rustc/cargo 的路径、mtime、尺寸、commit-hash。热路径只 `stat` 二进制，不 spawn `rustc -vV`。
- **直跑**：key 命中 `~/.cache/ruz/bin/<key>/exe` 时 `execve`，参数原样透传，退出码即脚本退出码。
- **包名归一化**：未命中时把副本的 `package.name` 改成 `ruz_<stem>_<key12>`，再 `cargo -Zscript build`。共享 `CARGO_TARGET_DIR` 仍然复用依赖，但 fingerprint 不再因两个 `hello` 撞车。
- **相对路径**：编译副本写在脚本同目录 `.ruz.<key12>.rs`，`include_str!` / `include!` 语义与手写脚本一致。

## For AI agents

This repo ships a skill (`skills/ruz/SKILL.md`) and a live-docs endpoint, so
agents never rely on stale instructions. Installing pulls in **only** the thin
skill file — not the repo:

```sh
npx skills add spraylee/ruz -g            # install the thin-pointer skill
curl -fsSL https://ruz.spraylee.com/llms.txt     # always-fresh docs
```

The skill is deliberately **thin** (single file, rarely changes) and points
here for anything version-specific. `npx skills update` keeps it in sync with
the repo's main branch.

## 不支持

- Windows（设计边界，非缺陷）

## License

MIT
