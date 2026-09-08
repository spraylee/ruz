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
- **默认 O3**：躲开 debug profile 的 10-20x 性能陷阱
- **报错即教学**：rustc 诊断 0.2s 返回，带行号 + 修复建议，AI 迭代闭环极短

## 安装

一行 curl（Linux / macOS，需要 python3 + cargo）：

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
ruz run demo.rs args…  # 编译+执行（未缓存才编译）
ruz check demo.rs      # 快速类型/借用检查，不产出二进制
ruz warm serde:derive reqwest:blocking regex  # 预热常用依赖
ruz doctor             # 环境体检
ruz cache              # 缓存位置与大小
```

## 实测（Xeon 8255C，Linux，2026-09）

| 场景 | python3 | ruz (O3) | 赢家 |
|---|---|---|---|
| Mandelbrot 纯计算 | 6.92s | **0.095s** | ruz 73x |
| 网络批量 20 HTTPS | 20.0s | **5.8s** | ruz 3.4x |
| 40MB 文本流水线 | 391ms | **130ms** | ruz 3x |
| SHA-256 / AES 大块 | **57ms** | 110ms | python（OpenSSL）|

**AI 迭代循环**：改几行重跑 0.13~0.6s；全新脚本（依赖已预热）0.7s。修复轮次实测与 Python 打平（rustc 报错自带答案）。

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
