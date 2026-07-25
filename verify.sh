#!/usr/bin/env bash
# Verify src/verified.rs with the Verus SMT verifier.
#
# Usage:
#   ./verify.sh                     # use `verus` from PATH, or download a release
#   ./verify.sh --from-source      # git-clone Verus and build it (no release artifacts)
#   VERUS=/path/to/verus ./verify.sh
#   VERUS_TAG=release/0.2026.07.12.xxxxxxx ./verify.sh   # pin a release (download mode)
#   VERUS_GIT_REV=<commit> ./verify.sh --from-source     # pin a commit (source mode)
#
# The cargo build does not need Verus — src/verified.rs compiles as plain
# Rust (ghost code erased). This script is for CI / developer machines and
# proves the requires/ensures/invariant annotations.
#
# Source mode is for environments where GitHub release downloads are
# unavailable or building is preferred. It still needs: rustup (fetches the
# toolchain pinned by Verus's rust-toolchain.toml) and a Z3 4.12.5 binary —
# taken from PATH if the version matches, else Verus's tools/get-z3.sh,
# else extracted from the PyPI z3-solver wheel.
#
# NOTE: the crates.io `vstd` pin in Cargo.toml (0.0.0-2026-07-12-0122)
# should track the Verus version used here; if verification fails after a
# version bump, align the two.
set -euo pipefail
cd "$(dirname "$0")"

FROM_SOURCE=0
if [ "${1:-}" = "--from-source" ]; then
    FROM_SOURCE=1
fi

# The crates.io `verus` crate is an empty placeholder that answers every
# invocation with an advertisement and exit code 0 — treat any candidate
# binary that identifies as a placeholder (or can't print a version) as
# absent, otherwise verification would falsely "pass".
is_real_verus() {
    local out
    out=$("$1" --version 2>&1) || return 1
    case "$out" in
        *placeholder*|*Playground*) return 1 ;;
        *Verus*) return 0 ;;
        *) return 1 ;;
    esac
}

VERUS_BIN="${VERUS:-}"

if [ -z "$VERUS_BIN" ] && [ "$FROM_SOURCE" = 0 ] && command -v verus >/dev/null 2>&1; then
    if is_real_verus "$(command -v verus)"; then
        VERUS_BIN="$(command -v verus)"
    else
        echo "note: 'verus' on PATH is the crates.io placeholder; ignoring it" >&2
    fi
fi

# Cached artifacts from previous runs of this script.
if [ -z "$VERUS_BIN" ]; then
    for cand in .verus/verus-*/verus .verus/src/source/target-verus/release/verus; do
        if [ -x "$cand" ] && is_real_verus "$cand"; then
            VERUS_BIN="$cand"
            break
        fi
    done
fi

get_z3() {
    # Prints an absolute path to a Z3 4.12.5 binary, arranging one if needed.
    local want="4.12.5"
    if command -v z3 >/dev/null 2>&1 && z3 --version 2>/dev/null | grep -q "$want"; then
        command -v z3
        return
    fi
    if [ -x .verus/z3 ] && .verus/z3 --version 2>/dev/null | grep -q "$want"; then
        echo "$PWD/.verus/z3"
        return
    fi
    mkdir -p .verus
    if (cd .verus/src/source 2>/dev/null && ./tools/get-z3.sh >/dev/null 2>&1); then
        cp .verus/src/source/z3 .verus/z3
        echo "$PWD/.verus/z3"
        return
    fi
    # GitHub releases unreachable — fall back to the PyPI z3-solver wheel,
    # which bundles the standalone binary.
    echo "get-z3.sh failed; extracting z3 from the PyPI z3-solver wheel" >&2
    python3 -m pip download "z3-solver==${want}.0" --no-deps -d .verus/z3pkg -q
    (cd .verus/z3pkg && unzip -oq ./*.whl -d unpacked)
    cp .verus/z3pkg/unpacked/z3_solver-*.data/data/bin/z3 .verus/z3
    chmod +x .verus/z3
    echo "$PWD/.verus/z3"
}

if [ -z "$VERUS_BIN" ] && [ "$FROM_SOURCE" = 1 ]; then
    # Build from source. The default rev is the last known good: the
    # commit this repo's proofs were verified against
    # (Verus 0.2026.07.25, "20 verified, 0 errors").
    rev="${VERUS_GIT_REV:-d64f7c416688cad31753a87af92ad69f7f4dcdc1}"
    mkdir -p .verus
    if [ ! -d .verus/src/.git ]; then
        git clone https://github.com/verus-lang/verus .verus/src
    fi
    if [ -n "$rev" ]; then
        (cd .verus/src && git checkout -q "$rev" 2>/dev/null \
            || { git fetch -q origin "$rev" && git checkout -q "$rev"; })
    fi
    Z3_PATH="$(get_z3)"
    cp "$Z3_PATH" .verus/src/source/z3 2>/dev/null || true
    (
        cd .verus/src
        rustup toolchain install
        (cd tools/vargo && cargo build --release)
        cd source
        PATH="$PWD/../tools/vargo/target/release:$PATH" \
            VERUS_Z3_PATH="$PWD/z3" vargo build --release
    )
    VERUS_BIN=".verus/src/source/target-verus/release/verus"
fi

if [ -z "$VERUS_BIN" ]; then
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64)  platform="x86-linux" ;;
        Linux-aarch64) platform="arm64-linux" ;;
        Darwin-arm64)  platform="arm64-macos" ;;
        Darwin-x86_64) platform="x86-macos" ;;
        *) echo "error: unsupported platform $(uname -s)-$(uname -m); set VERUS=" >&2; exit 1 ;;
    esac

    if [ -n "${VERUS_TAG:-}" ]; then
        api_url="https://api.github.com/repos/verus-lang/verus/releases/tags/${VERUS_TAG//\//%2F}"
    else
        api_url="https://api.github.com/repos/verus-lang/verus/releases/latest"
    fi
    echo "Downloading Verus ($platform) from ${VERUS_TAG:-latest release}..." >&2
    zip_url=$(curl -fsSL "$api_url" \
        | grep -o "\"browser_download_url\": *\"[^\"]*${platform}[^\"]*\.zip\"" \
        | head -1 | sed 's/.*"\(https[^"]*\)"/\1/')
    if [ -z "$zip_url" ]; then
        echo "error: could not find a ${platform} asset in the Verus release" >&2
        echo "hint: if GitHub release downloads are blocked here, try ./verify.sh --from-source" >&2
        exit 1
    fi
    mkdir -p .verus
    curl -fsSL -o .verus/verus.zip "$zip_url"
    (cd .verus && unzip -oq verus.zip && rm verus.zip)
    VERUS_BIN=$(ls -d .verus/verus-*/verus | head -1)
fi

if ! is_real_verus "$VERUS_BIN"; then
    echo "error: $VERUS_BIN does not look like a working Verus verifier" >&2
    exit 1
fi

echo "Using Verus: $VERUS_BIN" >&2
"$VERUS_BIN" --version >&2

exec "$VERUS_BIN" --crate-type=lib verify/verified_shim.rs
