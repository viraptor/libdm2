#!/usr/bin/env bash
# Verify src/verified.rs with the Verus SMT verifier.
#
# Usage:
#   ./verify.sh                # use `verus` from PATH, or download a release
#   VERUS=/path/to/verus ./verify.sh
#   VERUS_TAG=release/0.2026.07.12.xxxxxxx ./verify.sh   # pin a release
#
# The cargo build does not need Verus — src/verified.rs compiles as plain
# Rust (ghost code erased). This script is for CI / developer machines and
# proves the requires/ensures/invariant annotations.
#
# NOTE: the crates.io `vstd` pin in Cargo.toml (0.0.0-2026-07-12-0122)
# should track the Verus release used here; if verification fails after a
# release bump, align the two dates.
set -euo pipefail
cd "$(dirname "$0")"

VERUS_BIN="${VERUS:-}"

if [ -z "$VERUS_BIN" ] && command -v verus >/dev/null 2>&1; then
    VERUS_BIN="$(command -v verus)"
fi

if [ -z "$VERUS_BIN" ]; then
    # Cached previous download?
    cached=$(ls -d .verus/verus-*/verus 2>/dev/null | head -1 || true)
    if [ -n "$cached" ]; then
        VERUS_BIN="$cached"
    fi
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
        exit 1
    fi
    mkdir -p .verus
    curl -fsSL -o .verus/verus.zip "$zip_url"
    (cd .verus && unzip -oq verus.zip && rm verus.zip)
    VERUS_BIN=$(ls -d .verus/verus-*/verus | head -1)
fi

echo "Using Verus: $VERUS_BIN" >&2
"$VERUS_BIN" --version >&2 || true

exec "$VERUS_BIN" --crate-type=lib verify/verified_shim.rs
