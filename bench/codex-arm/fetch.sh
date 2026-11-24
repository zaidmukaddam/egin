#!/bin/sh
# Fetch Codex CLI 0.164.0's static Linux x86 build for the [codex] arm, into $EGIN_HARNESS_DIR (default ~/harness).
set -eu
d="${EGIN_HARNESS_DIR:-$HOME/harness}/codex-0.164.0"
mkdir -p "$d"
curl -fsSL -o "$d/codex.tar.gz" https://github.com/openai/codex/releases/download/rust-v0.164.0/codex-x86_64-unknown-linux-musl.tar.gz
echo "90dd0b03b28afcb7fa733a4f73b413db2916a47347efd43fb669cc1c774ee8e4  $d/codex.tar.gz" | sha256sum -c -
