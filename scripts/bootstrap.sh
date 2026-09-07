#!/bin/sh
# 一键安装 ruz 到 ~/.local/bin（curl 拉 GitHub Release → 落位 → chmod）
#
#   curl -fsSL https://ruz.spraylee.com/i | sh
#   curl -fsSL https://github.com/spraylee/ruz/releases/latest/download/bootstrap.sh | sh
#
# 环境变量：
#   RUZ_VERSION     钉死 tag，如 v0.1.0；空则 302 探测 latest
#   RUZ_INSTALL_DIR 默认 ~/.local/bin

set -eu

REPOSITORY="spraylee/ruz"
VERSION="${RUZ_VERSION:-}"
INSTALL_DIR="${RUZ_INSTALL_DIR:-${HOME}/.local/bin}"

fail() {
  printf '%s\n' "[ruz] bootstrap: $*" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || fail "需要 curl"
command -v uname >/dev/null 2>&1 || fail "需要 uname"
command -v awk >/dev/null 2>&1 || fail "需要 awk"

# 不支持 Windows（git-bash/WSL 里跑会在这里被拦下或自然失败）
OS=$(uname -s)
case "$OS" in
  Linux|Darwin) ;;
  *) fail "不支持的平台 $OS（ruz 支持 Linux/macOS）" ;;
esac

if ! command -v python3 >/dev/null 2>&1; then
  fail "需要 python3（ruz 是单文件 Python 工具）"
fi

query_latest_version() {
  loc=$(curl -fsSI "https://github.com/${REPOSITORY}/releases/latest" 2>/dev/null \
    | awk 'tolower($1)=="location:" { print $2; exit }' \
    | tr -d '\r')
  [ -n "$loc" ] || return 0
  # 刻意不用 awk 正则（gsub(/[/?#]/…)）——BSD awk 字符类不允许 /，
  # 报 nonterminated character class。POSIX 参数展开剥 tag，全 awk 通用。
  case "$loc" in
    */releases/tag/*)
      loc="${loc#*releases/tag/}"
      loc="${loc%%[/?#]*}"
      printf '%s\n' "$loc"
      ;;
  esac
}

resolve_version() {
  if [ -n "$VERSION" ]; then
    printf '%s\n' "$VERSION"
    return 0
  fi
  v=$(query_latest_version)
  [ -n "$v" ] || fail "无法探测 latest tag（可 export RUZ_VERSION=v0.1.0 钉死）"
  printf '%s\n' "$v"
}

TAG=$(resolve_version)
BASE="https://github.com/${REPOSITORY}/releases/download/${TAG}"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

printf '%s\n' "[ruz] installing ${TAG} -> ${INSTALL_DIR}"

# 单文件资产，无平台矩阵
curl -fsSL "${BASE}/ruz" -o "${TMP}/ruz"
if [ -f "${BASE}/SHA256SUMS" ] 2>/dev/null; then :; fi
curl -fsSL "${BASE}/SHA256SUMS" -o "${TMP}/SHA256SUMS" 2>/dev/null || true
if [ -s "${TMP}/SHA256SUMS" ]; then
  want=$(awk '$2=="ruz" {print $1}' "${TMP}/SHA256SUMS")
  if [ -n "$want" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      got=$(sha256sum "${TMP}/ruz" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
      got=$(shasum -a 256 "${TMP}/ruz" | awk '{print $1}')
    else
      got=""
      printf '%s\n' "[ruz] 无 sha256 工具，跳过校验" >&2
    fi
    if [ -n "$got" ] && [ "$got" != "$want" ]; then
      fail "SHA256 校验失败（want=$want got=$got）"
    fi
    [ -n "$got" ] && printf '%s\n' "[ruz] sha256 verified"
  fi
fi

mkdir -p "$INSTALL_DIR"
# 原子落位：先写临时名再 mv
cp "${TMP}/ruz" "${INSTALL_DIR}/ruz.new"
chmod +x "${INSTALL_DIR}/ruz.new"
mv "${INSTALL_DIR}/ruz.new" "${INSTALL_DIR}/ruz"

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    printf '%s\n' "[ruz] 注意: ${INSTALL_DIR} 不在 PATH，建议加一行到 shell rc：" >&2
    printf '       export PATH="%s:$PATH"\n' "$INSTALL_DIR" >&2
    ;;
esac

# 装完自检
if "${INSTALL_DIR}/ruz" --version >/dev/null 2>&1; then
  printf '%s\n' "[ruz] $(${INSTALL_DIR}/ruz --version) installed ok"
else
  fail "安装后自检失败"
fi

printf '%s\n' "[ruz] done. try: ruz new demo.rs && ruz run demo.rs"
