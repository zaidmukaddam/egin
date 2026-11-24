#!/bin/sh
# Build egin for every platform into the one `egin` package (npm/egin/bin/<platform>-<arch>/egin) and publish it.
# The version comes from Cargo.toml and is written into package.json first.
#
#   npm/release.sh            build and pack only (dry run)
#   npm/release.sh --publish  also publish to npm
#
# Linux targets build with `cargo zigbuild` (cargo install cargo-zigbuild; brew install zig).
set -eu
cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
publish=${1:-}

build() { # <rust target> <node platform-arch>
  case $1 in
    # microsandbox's build script installs the target's runtime into MSB_HOME (default ~/.microsandbox). A Linux
    # runtime there would sit next to the Mac's under the same `msb` name, and the script refuses the mixed pair.
    *linux*) MSB_HOME="$PWD/target/msb-home/$1" cargo zigbuild --release --target "$1" ;;
    *) cargo build --release --target "$1" ;;
  esac
  mkdir -p "npm/egin/bin/$2"
  cp "target/$1/release/egin" "npm/egin/bin/$2/egin"
  chmod 755 "npm/egin/bin/$2/egin"
}

build aarch64-apple-darwin darwin-arm64
build x86_64-unknown-linux-gnu linux-x64
build aarch64-unknown-linux-gnu linux-arm64

cd npm/egin
npm version "$version" --no-git-tag-version --allow-same-version >/dev/null
npm test
if [ "$publish" = "--publish" ]; then
  npm publish --access public
else
  npm pack --dry-run
fi
