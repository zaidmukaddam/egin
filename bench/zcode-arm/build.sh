#!/bin/sh
# Build ZCode 0.17.0 for the [zcode] arm as FrontierHarness v1.1's suite installs it (commit 3c5b2276, CLI only, no
# type checking), then pack the bundled CLI with its Node into zcode-pack.tgz. Run it on x86-64 Linux with internet
# (a scratch Daytona sandbox from ubuntu:24.04 works), then copy the pack to $EGIN_HARNESS_DIR/zcode-0.17.0/ on the
# bench host. The CLI bundles into one zcode.cjs, so the pack needs no node_modules.
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q curl xz-utils ca-certificates git build-essential python3
mkdir -p /opt/egin-node
curl --retry 5 -fsSL https://nodejs.org/dist/v24.22.1/node-v24.22.1-linux-x64.tar.xz | tar -xJ -C /opt/egin-node --strip-components=1
export PATH=/opt/egin-node/bin:$PATH
npm install -g --prefix /opt/egin-node pnpm@10.33.2
mkdir -p /opt/zcode && cd /opt/zcode
git init -q
git remote add origin https://github.com/zai-org/ZCode.git 2>/dev/null || git remote set-url origin https://github.com/zai-org/ZCode.git
git fetch -q --depth 1 origin 3c5b227649371507ddc4eb46ac5133afe6db1cab
git checkout -q --force --detach FETCH_HEAD
pnpm install --frozen-lockfile --filter "@zcode/cli..." --os=linux --cpu=x64 --libc=glibc
TSC_FAST='--noCheck --declaration false --declarationMap false --composite false --incremental false'
pnpm --filter @zcode/formal-proof build
pnpm --filter "@zcode/cli^..." --filter "!@zcode/formal-proof" --filter "!@zcode/tui" build $TSC_FAST
pnpm --filter @zcode/tui exec tsc $TSC_FAST
pnpm --filter @zcode/tui exec node scripts/build.mjs
pnpm --filter @zcode/cli build
rm -rf /opt/zpack && mkdir -p /opt/zpack/bin
cp /opt/egin-node/bin/node /opt/zpack/bin/
cp -r apps/zcode-cli/packages/cli/dist/zcode.cjs apps/zcode-cli/packages/cli/dist/provider /opt/zpack/
/opt/zpack/bin/node /opt/zpack/zcode.cjs --version
tar -czf /root/zcode-pack.tgz -C /opt zpack
ls -la /root/zcode-pack.tgz
