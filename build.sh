#!/usr/bin/env bash
# OpenTerm — unix build orchestrator (mac/linux). Mirror of build.bat.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

MODE="${1:-release}"
case "$MODE" in
  debug|release|regular|portable|small|run|clean) ;;
  *) echo "usage: $0 [regular|portable|debug|release|small|run|clean]"; exit 2 ;;
esac

PACKAGE=regular
[[ "$MODE" == "portable" ]] && PACKAGE=portable
[[ "$MODE" == "regular" || "$MODE" == "portable" ]] && MODE=release
STAGE="dist/$PACKAGE"

need() { command -v "$1" >/dev/null 2>&1 || { echo "[x] missing: $1"; return 1; }; }

if [[ "$MODE" == "clean" ]]; then
  rm -rf target dist
  echo "[clean] done."
  exit 0
fi

echo "[openterm] mode: $MODE"
echo "[openterm] package: $PACKAGE"

need cargo && need rustc && need zig && need go || {
  cat <<EOF

required toolchains:
  rust : https://rustup.rs
  zig  : https://ziglang.org/download/   (0.13+)
  go   : https://go.dev/dl/              (1.22+)
  cc   : clang or gcc

EOF
  exit 1
}

mkdir -p target/native "$STAGE"

# 1) zig
echo "[1/3] zig  : building libfastgrid.a ..."
ZIG_OPT="-O ReleaseFast"
[[ "$MODE" == "debug" ]] && ZIG_OPT="-O Debug"
( cd native && zig build-lib fastgrid.zig $ZIG_OPT -fPIC -femit-bin="../target/native/libfastgrid.a" )
rm -f native/fastgrid.o native/libfastgrid.a
[[ -f target/native/libfastgrid.a ]] || { echo "[x] zig output missing"; exit 1; }
echo "       ok."

# 2) go
echo "[2/3] go   : building otm-agent ..."
pushd agent >/dev/null
[[ -f go.sum ]] || go mod download
CGO_ENABLED=0 GOFLAGS=-trimpath go build -ldflags "-s -w" -o "../$STAGE/otm-agent" .
popd >/dev/null
echo "       ok."

# 3) rust
echo "[3/3] rust : building openterm ..."
FEATURES=()
[[ "$PACKAGE" == "portable" ]] && FEATURES=(--features portable)
case "$MODE" in
  debug)   cargo build "${FEATURES[@]}";                         OUT=target/debug/openterm ;;
  small)   cargo build --profile release-small "${FEATURES[@]}"; OUT=target/release-small/openterm ;;
  release|run) cargo build --release "${FEATURES[@]}";           OUT=target/release/openterm ;;
esac
cp "$OUT" "$STAGE/openterm"
echo "       ok."

if [[ "$PACKAGE" == "portable" ]]; then
  printf '; OpenTerm settings\n[appearance]\ntheme=dark\n\n[terminal]\ndefault_shell=bash\n\n[vault]\nunlock_grace_value=1\nunlock_grace_unit=day\n' > "$STAGE/openterm.ini"
fi

if [[ "$MODE" == "small" ]] && command -v upx >/dev/null 2>&1; then
  echo "[+] upx : compressing ..."
  upx --best --lzma "$STAGE/openterm"   >/dev/null
  upx --best --lzma "$STAGE/otm-agent"  >/dev/null
fi

echo
echo "===================================================="
echo "  openterm:    $ROOT/$STAGE/openterm"
echo "  otm-agent:   $ROOT/$STAGE/otm-agent"
[[ "$PACKAGE" == "portable" ]] && echo "  settings:    $ROOT/$STAGE/openterm.ini"
echo "===================================================="
du -h "$STAGE/openterm" "$STAGE/otm-agent"
echo

if [[ "$MODE" == "run" ]]; then
  echo "[run] launching ..."
  exec "$STAGE/openterm"
fi
