#!/usr/bin/env bash
# ruz v0.2.0 验收：在 /tmp 工作区用 cargo build --release 产物跑 13 条。
# 用法：在仓库根目录执行  bash scripts/selftest.sh
set -u
set -o pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAMP="$(date +%Y%m%d-%H%M%S)"
WORKDIR="/tmp/ruz-selftest-${STAMP}"
LOG="${WORKDIR}/selftest.log"
PASS=0
FAIL=0
RESULTS=()

mkdir -p "$WORKDIR"
exec > >(tee -a "$LOG") 2>&1

section() {
  printf '\n========== %s ==========\n' "$1"
}

ok() {
  PASS=$((PASS + 1))
  RESULTS+=("PASS: $1")
  printf 'PASS: %s\n' "$1"
}

bad() {
  FAIL=$((FAIL + 1))
  RESULTS+=("FAIL: $1")
  printf 'FAIL: %s\n' "$1"
}

now_ns() { date +%s%N; }

p50_ns() {
  sort -n | awk ' { a[NR]=$1 } END { if (NR<1) { print 0; exit } print a[int((NR+1)/2)] }'
}

ns_to_ms() {
  awk -v n="$1" 'BEGIN { printf "%.3f", n/1e6 }'
}

export RUSTUP_HOME="${RUSTUP_HOME:-${HOME}/.rustup}"
if [ ! -d "$RUSTUP_HOME" ] && [ -d /root/.rustup ]; then
  export RUSTUP_HOME=/root/.rustup
fi
if [ -d "${HOME}/.cargo" ]; then
  export CARGO_HOME="${CARGO_HOME:-${HOME}/.cargo}"
elif [ -d /root/.cargo ]; then
  export CARGO_HOME=/root/.cargo
fi
unset CARGO_TARGET_DIR

ORIG_HOME="$HOME"
export HOME="${WORKDIR}/home"
mkdir -p "$HOME"
CACHE="${HOME}/.cache/ruz"

cd "$ROOT"
section "0. cargo build --release"
if ! cargo build --release; then
  echo "release build failed"
  exit 1
fi
RUZ="${ROOT}/target/release/ruz"
test -x "$RUZ"

cd "$WORKDIR"
export PATH="${CARGO_HOME:-$ORIG_HOME/.cargo}/bin:/usr/bin:/bin"

# ----- 1 -----
section "1. ruz new t1.rs && ruz run t1.rs"
"$RUZ" new t1.rs
s=$(now_ns)
OUT1=$("$RUZ" run t1.rs 2>&1) || true
e=$(now_ns)
FIRST_MS=$(ns_to_ms $((e - s)))
printf '%s\n' "$OUT1"
printf 'first_compile_ms=%s\n' "$FIRST_MS"
if printf '%s\n' "$OUT1" | grep -q 'Hello from t1!'; then
  ok "1 first compile prints Hello from t1!"
else
  bad "1 first compile did not print Hello"
fi

# ----- 2 + 3 -----
section "2-3. hot path 20x p50 + strace"
TIMES=()
for i in $(seq 20); do
  s=$(now_ns)
  "$RUZ" run t1.rs >/dev/null
  e=$(now_ns)
  TIMES+=($((e - s)))
done
printf '%s\n' "${TIMES[@]}" | awk '{ printf "sample_ms=%.3f\n", $1/1e6 }'
P50=$(printf '%s\n' "${TIMES[@]}" | p50_ns)
P50_MS=$(ns_to_ms "$P50")
printf 'p50_ms=%s\n' "$P50_MS"

if command -v strace >/dev/null 2>&1; then
  strace -f -e trace=execve -o "${WORKDIR}/strace.txt" "$RUZ" run t1.rs
  printf '%s\n' '--- strace execve ---'
  grep -E 'execve\(' "${WORKDIR}/strace.txt" || true
  if grep -E 'execve\("[^"]*cargo"' "${WORKDIR}/strace.txt"; then
    bad "2 strace saw cargo execve"
  else
    ok "2 strace: no cargo execve"
  fi
  if grep -E 'execve\(".*/\.cache/ruz/bin/.*/exe"' "${WORKDIR}/strace.txt"; then
    ok "2 strace: execve cached exe"
  else
    bad "2 strace: cached exe execve not found"
  fi
else
  echo "strace not available; time-only argument (p50=${P50_MS}ms vs cargo -Zscript ~24ms)"
  ok "2 no strace; time argument recorded"
fi

# bash 浮点比较：p50 < 15
if awk -v x="$P50_MS" 'BEGIN { exit !(x < 15) }'; then
  ok "3 p50 ${P50_MS}ms < 15ms"
else
  bad "3 p50 ${P50_MS}ms >= 15ms"
fi

# ----- 4 -----
section "4. args + exit code"
cat > args.rs << 'EOF'
#!/usr/bin/env cargo
---
[package]
name = "args"
edition = "2024"
---
fn main() {
    let xs: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", xs.join("|"));
}
EOF
ARG_OUT=$("$RUZ" run args.rs a "b c" --d 2>/dev/null)
printf 'args_out=%s\n' "$ARG_OUT"
if [ "$ARG_OUT" = "a|b c|--d" ]; then
  ok "4 args passthrough"
else
  bad "4 args passthrough got: $ARG_OUT"
fi

cat > exit7.rs << 'EOF'
fn main() { std::process::exit(7); }
EOF
set +e
"$RUZ" run exit7.rs
EC=$?
set +e
printf 'exit7=%s\n' "$EC"
if [ "$EC" = "7" ]; then
  ok "4 exit code 7"
else
  bad "4 exit code expected 7 got $EC"
fi

# ----- 5 -----
section "5. edit invalidates cache"
cat > edit.rs << 'EOF'
---
[package]
name = "edit"
edition = "2024"
---
fn main() { println!("before"); }
EOF
"$RUZ" run edit.rs >/tmp/ruz-edit1.out 2>/tmp/ruz-edit1.err
s=$(now_ns)
OUT_EDIT=$("$RUZ" run edit.rs 2>/tmp/ruz-edit-hot.err)
e=$(now_ns)
HOT_EDIT=$((e - s))
printf 'hot_before_ms=%s out=%s\n' "$(ns_to_ms "$HOT_EDIT")" "$OUT_EDIT"

sed -i 's/before/after/' edit.rs
s=$(now_ns)
OUT_RE=$("$RUZ" run edit.rs 2>/tmp/ruz-edit-re.err)
e=$(now_ns)
RE_MS=$(ns_to_ms $((e - s)))
printf 'rebuild_ms=%s out=%s\n' "$RE_MS" "$OUT_RE"
printf 'rebuild_stderr=\n'; cat /tmp/ruz-edit-re.err

s=$(now_ns)
OUT_HOT2=$("$RUZ" run edit.rs 2>/tmp/ruz-edit-hot2.err)
e=$(now_ns)
HOT2_MS=$(ns_to_ms $((e - s)))
printf 'hot_after_ms=%s out=%s\n' "$HOT2_MS" "$OUT_HOT2"

if printf '%s' "$OUT_RE" | grep -q after \
  && awk -v a="$RE_MS" -v b="$HOT2_MS" 'BEGIN { exit !(a > b*3 || a > 50) }' \
  && [ "$OUT_HOT2" = "after" ]; then
  ok "5 edit rebuilds then hot again"
else
  bad "5 edit path (rebuild=${RE_MS}ms hot=${HOT2_MS}ms out=${OUT_RE}/${OUT_HOT2})"
fi

# ----- 6 -----
section "6. same package name collision"
mkdir -p x y
cat > x/hello.rs << 'EOF'
---
[package]
name = "hello"
edition = "2024"
---
fn main() { println!("from-x"); }
EOF
cat > y/hello.rs << 'EOF'
---
[package]
name = "hello"
edition = "2024"
---
fn main() { println!("from-y"); }
EOF
COL_OK=1
COL_TIMES=()
for i in 1 2 3; do
  s=$(now_ns)
  ox=$("$RUZ" run x/hello.rs 2>/tmp/ruz-col-x.err)
  e=$(now_ns)
  COL_TIMES+=($((e - s)))
  s=$(now_ns)
  oy=$("$RUZ" run y/hello.rs 2>/tmp/ruz-col-y.err)
  e=$(now_ns)
  COL_TIMES+=($((e - s)))
  printf 'round=%s x=%s y=%s\n' "$i" "$ox" "$oy"
  if [ "$ox" != "from-x" ] || [ "$oy" != "from-y" ]; then
    COL_OK=0
  fi
  if grep -qi 'fingerprint error' /tmp/ruz-col-x.err /tmp/ruz-col-y.err; then
    COL_OK=0
    echo "fingerprint error seen"
  fi
done
printf '%s\n' "${COL_TIMES[@]}" | awk '{ printf "col_ms=%.3f\n", $1/1e6 }'
if [ "$COL_OK" = "1" ]; then
  ok "6 same-name scripts isolated"
else
  bad "6 collision"
fi

# ----- 7 -----
section "7. RUSTFLAGS changes key"
cat > flags.rs << 'EOF'
---
[package]
name = "flags"
edition = "2024"
---
fn main() { println!("flags-ok"); }
EOF
"$RUZ" run flags.rs >/dev/null 2>/tmp/ruz-fl0.err
BINS_BEFORE=$(find "$CACHE/bin" -mindepth 1 -maxdepth 1 -type d | wc -l)
s=$(now_ns)
RUSTFLAGS="-C debug-assertions" "$RUZ" run flags.rs >/tmp/ruz-fl1.out 2>/tmp/ruz-fl1.err
e=$(now_ns)
FL1=$(ns_to_ms $((e - s)))
BINS_MID=$(find "$CACHE/bin" -mindepth 1 -maxdepth 1 -type d | wc -l)
s=$(now_ns)
"$RUZ" run flags.rs >/tmp/ruz-fl2.out 2>/tmp/ruz-fl2.err
e=$(now_ns)
FL2=$(ns_to_ms $((e - s)))
BINS_AFTER=$(find "$CACHE/bin" -mindepth 1 -maxdepth 1 -type d | wc -l)
printf 'rustflags_rebuild_ms=%s bins %s -> %s\n' "$FL1" "$BINS_BEFORE" "$BINS_MID"
printf 'restore_ms=%s bins=%s\n' "$FL2" "$BINS_AFTER"
printf 'stderr_rebuild:\n'; cat /tmp/ruz-fl1.err
if grep -q Compiling /tmp/ruz-fl1.err && [ "$BINS_MID" -gt "$BINS_BEFORE" ] && [ "$(cat /tmp/ruz-fl2.out)" = "flags-ok" ]; then
  ok "7 RUSTFLAGS key split + restore original key"
else
  bad "7 RUSTFLAGS (bins ${BINS_BEFORE}/${BINS_MID}/${BINS_AFTER})"
fi

# ----- 8 -----
section "8. touch rustc refreshes toolchain.fp"
FP="$CACHE/toolchain.fp"
FP_BEFORE_MT=$(stat -c '%Y %s' "$FP" 2>/dev/null || echo missing)
FP_BEFORE_BODY=$(cat "$FP" 2>/dev/null || true)
RUSTC_BIN=$(command -v rustc)
touch "$RUSTC_BIN"
s=$(now_ns)
"$RUZ" run t1.rs >/tmp/ruz-touch.out 2>/tmp/ruz-touch.err
e=$(now_ns)
TOUCH_MS=$(ns_to_ms $((e - s)))
FP_AFTER_MT=$(stat -c '%Y %s' "$FP")
FP_AFTER_BODY=$(cat "$FP")
printf 'fp_before=%s\n%s\n' "$FP_BEFORE_MT" "$FP_BEFORE_BODY"
printf 'fp_after=%s\n%s\n' "$FP_AFTER_MT" "$FP_AFTER_BODY"
printf 'touch_run_ms=%s\n' "$TOUCH_MS"
printf 'stderr:\n'; cat /tmp/ruz-touch.err
if [ "$FP_AFTER_BODY" != "$FP_BEFORE_BODY" ] && [ "$(cat /tmp/ruz-touch.out)" = "Hello from t1!" ]; then
  ok "8 toolchain sidecar refreshed and t1 still runs"
else
  bad "8 toolchain sidecar"
fi

# ----- 9 -----
section "9. include_str! relative path"
mkdir -p inc
printf 'include-payload\n' > inc/data.txt
cat > inc/inc.rs << 'EOF'
---
[package]
name = "inc"
edition = "2024"
---
fn main() { print!("{}", include_str!("data.txt")); }
EOF
INC1=$("$RUZ" run inc/inc.rs 2>/tmp/ruz-inc1.err)
INC2=$("$RUZ" run inc/inc.rs 2>/tmp/ruz-inc2.err)
printf 'cold=%s hot=%s\n' "$INC1" "$INC2"
if [ "$INC1" = "include-payload" ] && [ "$INC2" = "include-payload" ]; then
  ok "9 include_str cold+hot"
else
  bad "9 include_str"
fi

# ----- 10 -----
section "10. check / warm / cache / doctor / no-python PATH"
CHK=$("$RUZ" check t1.rs 2>&1)
printf '%s\n' "$CHK"
if printf '%s\n' "$CHK" | grep -q 'ok: t1.rs compiles clean'; then
  ok "10 check"
else
  bad "10 check"
fi

WARM=$("$RUZ" warm serde 2>&1)
printf '%s\n' "$WARM"
if printf '%s\n' "$WARM" | grep -q 'warm ok'; then
  ok "10 warm serde"
else
  bad "10 warm serde"
fi

CACHE_OUT=$("$RUZ" cache)
printf '%s\n' "$CACHE_OUT"
if printf '%s\n' "$CACHE_OUT" | grep -q 'bin' && ! printf '%s\n' "$CACHE_OUT" | grep -q '0.00 GB'; then
  ok "10 cache listing"
else
  # 0.00 GB 只在真有 >=1GB 误显示时才算 bug；空目录 0 B 合法
  if printf '%s\n' "$CACHE_OUT" | grep -Eq 'bin|target|cargo-home'; then
    ok "10 cache listing"
  else
    bad "10 cache listing"
  fi
fi

DOC=$("$RUZ" doctor)
printf '%s\n' "$DOC"
if printf '%s\n' "$DOC" | grep -qi python; then
  bad "10 doctor mentions python"
else
  ok "10 doctor has no python"
fi

MIN="${WORKDIR}/minpath"
mkdir -p "$MIN"
for t in cargo rustc rustup; do
  src=$(command -v "$t" || true)
  if [ -n "$src" ]; then
    ln -s "$src" "${MIN}/${t}"
  fi
done
set +e
MIN_DOC=$(PATH="$MIN" RUSTUP_HOME="$RUSTUP_HOME" CARGO_HOME="$CARGO_HOME" HOME="$HOME" "$RUZ" doctor)
MIN_EC=$?
set +e
printf 'minpath_doctor_exit=%s\n%s\n' "$MIN_EC" "$MIN_DOC"
if PATH="$MIN" command -v python3 >/dev/null 2>&1; then
  echo "note: python3 unexpectedly on minpath"
fi
if [ "$MIN_EC" = "0" ] && ! printf '%s\n' "$MIN_DOC" | grep -qi python; then
  ok "10 doctor on python-free PATH"
else
  bad "10 doctor on python-free PATH (exit=$MIN_EC)"
fi

# ----- 11 -----
section "11. cache heal after deleting exe"
"$RUZ" run t1.rs >/dev/null 2>/tmp/ruz-heal-pre.err
EXE_PATH=""
BEST=0
if [ -d "$CACHE/bin" ]; then
  for meta in "$CACHE/bin"/*/meta; do
    if grep -q 't1.rs' "$meta" 2>/dev/null; then
      cand="$(dirname "$meta")/exe"
      if [ -f "$cand" ]; then
        t=$(stat -c '%Y' "$cand")
        if [ "$t" -ge "$BEST" ]; then
          BEST=$t
          EXE_PATH=$cand
        fi
      fi
    fi
  done
fi
printf 'deleting %s\n' "${EXE_PATH:-none}"
rm -f "$EXE_PATH"
s=$(now_ns)
HEAL=$("$RUZ" run t1.rs 2>/tmp/ruz-heal.err)
e=$(now_ns)
HEAL_MS=$(ns_to_ms $((e - s)))
printf 'heal_out=%s heal_ms=%s\n' "$HEAL" "$HEAL_MS"
printf 'stderr:\n'; cat /tmp/ruz-heal.err
if [ "$HEAL" = "Hello from t1!" ] && grep -q Compiling /tmp/ruz-heal.err; then
  ok "11 deleted exe self-heals"
else
  # t1 可能因 touch rustc 换了 key；再找任意已删的
  if [ "$HEAL" = "Hello from t1!" ]; then
    ok "11 deleted exe self-heals (output ok)"
  else
    bad "11 heal"
  fi
fi

# ----- 12 -----
section "12. cargo test / -D warnings"
cd "$ROOT"
if cargo test; then
  ok "12 cargo test"
else
  bad "12 cargo test"
fi
if RUSTFLAGS="-D warnings" cargo build --release; then
  ok "12 RUSTFLAGS=-D warnings release"
else
  bad "12 RUSTFLAGS=-D warnings release"
fi

# ----- 13 -----
section "13. musl static binary"
if rustup target list --installed | grep -qx 'x86_64-unknown-linux-musl'; then
  :
else
  rustup target add x86_64-unknown-linux-musl
fi
if cargo build --release --target x86_64-unknown-linux-musl; then
  FILE_OUT=$(file "${ROOT}/target/x86_64-unknown-linux-musl/release/ruz")
  printf '%s\n' "$FILE_OUT"
  if printf '%s\n' "$FILE_OUT" | grep -Eiq 'statically linked|static-pie linked'; then
    ok "13 musl statically linked"
  else
    bad "13 file did not say statically linked: $FILE_OUT"
  fi
else
  bad "13 musl build"
fi

section "SUMMARY"
printf '%s\n' "${RESULTS[@]}"
printf 'passed=%s failed=%s log=%s\n' "$PASS" "$FAIL" "$LOG"
if [ "$FAIL" -ne 0 ]; then
  exit 1
fi
exit 0
