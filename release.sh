#!/bin/bash
set -e

cargo build --release

mkdir -p $HOME/.local/bin

cp target/release/ccu $HOME/.local/bin/ccu
cp target/release/pcu $HOME/.local/bin/pcu
cp target/release/ncu $HOME/.local/bin/ncu

echo "Installed ccu, pcu, ncu to ~/.local/bin/"
