#!/bin/sh
# Fetch fx 0.0.17's static Linux x86 build for the [fx] arm, into $EGIN_HARNESS_DIR (default ~/harness).
set -eu
d="${EGIN_HARNESS_DIR:-$HOME/harness}/fx-0.0.17"
mkdir -p "$d"
curl -fsSL -o "$d/fx-linux-x86_64.tar.gz" https://releases.fx.sh/v0.0.17/fx-linux-x86_64.tar.gz
echo "78c155d59fa59e42fd56d93037fd75041868c9c67865e8c7f0ca6dcdfac23e75  $d/fx-linux-x86_64.tar.gz" | sha256sum -c -
