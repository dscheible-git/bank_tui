#!/usr/bin/env bash
set -e

echo "=== Bank TUI Setup & Installation ==="

# 1. Rust & Cargo installieren, falls nicht vorhanden
if ! command -v cargo &> /dev/null; then
    echo "[!] Rust/Cargo wurde nicht gefunden. Installiere Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    fi
else
    echo "[+] Rust/Cargo ist bereits installiert."
fi

# 2. In Projektordner wechseln
cd "$(dirname "$0")"

# 3. Projekt kompilieren
echo "[+] Kompiliere das Rust TUI-Projekt (cargo build --release)..."
cargo build --release

echo ""
echo "=== Installation erfolgreich abgeschlossen! ==="
echo "Sie können die Anwendung starten mit:"
echo "  cargo run --release"
