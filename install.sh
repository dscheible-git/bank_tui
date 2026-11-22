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

# 4. Binary nach ~/.local/bin installieren, falls verfügbar
BIN_DIR="$HOME/.local/bin"
BINARY="target/release/bank_tui"

if [ -d "$BIN_DIR" ] && [ -w "$BIN_DIR" ]; then
    cp "$BINARY" "$BIN_DIR/bank_tui"
    chmod +x "$BIN_DIR/bank_tui"
    echo "[+] Binary installiert unter: $BIN_DIR/bank_tui"
    INSTALLED=1

    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *)
            echo "[!] Hinweis: $BIN_DIR ist nicht in PATH."
            echo "    Fügen Sie es zu PATH hinzu, um 'bank_tui' direkt auszuführen."
            ;;
    esac
else
    echo "[i] $BIN_DIR ist nicht verfügbar; das Binary bleibt unter $PWD/$BINARY."
    INSTALLED=0
fi

echo ""
echo "=== Installation erfolgreich abgeschlossen! ==="
echo "Sie können die Anwendung starten mit:"
if [ "$INSTALLED" -eq 1 ]; then
    echo "  bank_tui"
else
    echo "  cargo run --release"
fi
