#!/usr/bin/env bash
set -e
echo "==============================================="
echo "  NeuroStrata: 3-Tier Memory Installer        "
echo "==============================================="

echo "Building Rust backend..."
cargo build --release

echo "Installing Rust binary to ~/.local/bin..."
mkdir -p ~/.local/bin
cp target/release/neurostrata-mcp ~/.local/bin/

echo "Building Obsidian plugin..."
cd plugins/obsidian/obsidian-neurostrata
npm install
npm run build
cd -

echo "Setting up NeuroVault Obsidian Vault..."
mkdir -p "$HOME/Documents/NeuroVault/.obsidian/plugins"
ln -sfn "$(pwd)/plugins/obsidian/obsidian-neurostrata" "$HOME/Documents/NeuroVault/.obsidian/plugins/obsidian-neurostrata"

echo "==============================================="
echo "  Installation Complete!"
echo "  Ensure ~/.local/bin is in your PATH."
echo "  You can start the backend by running:"
echo "    neurostrata-mcp"
echo "==============================================="



