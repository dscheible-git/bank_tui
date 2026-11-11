# bank_tui

A fast, robust terminal user interface (TUI) built in Rust using `ratatui` for parsing, categorizing, filtering, and exploring bank expenditure CSV exports.

---

## Features

- **Vim-Style Navigation:** Fast, keyboard-driven interface designed for a seamless terminal workflow.
- **Dynamic Header & Column Detection:** Automatically locates the header line in bank CSV exports (searching dynamically for the header starting with `"Buchung"`) and maps columns dynamically by key terms (`Buchung`, `Betrag`, `Empfänger`, `Verwendungszweck`, etc.), making it resilient to bank format changes and missing columns.
- **Encoding Resilient:** Seamlessly handles UTF-8 as well as legacy Windows-1252 / ISO-8859-1 encodings and strips BOM (`\u{feff}`) markers (ensuring full support for German umlauts).
- **Smart Duplicate Detection:** Tracks duplicate transactions across multiple CSV files using a unique record signature and logs collision details to `stderr`.
- **Detailed Log Diagnostics:** Emits exact reasons for any skipped or excluded entries (missing required fields, parse errors, ignored payees, positive amounts, etc.).
- **Configurable Rules (`config.toml`):** 
  - Define custom keyword-to-category mapping rules.
  - Exclude unwanted payees/senders via `ignore_payees`.
- **Expenditure Filtering:** Automatically focuses on financial outflows (parsing German monetary formats like `-12,34 €` and filtering out positive incomes or zero values).

---

## Configuration (`config.toml`)

Copy `config.toml` to `~/.config/bank_tui/config.toml` (or `$XDG_CONFIG_HOME/bank_tui/config.toml`) and adjust it to customize automatic category assignments and excluded payees:

```toml
# Directory containing the bank CSV exports
csv_dir = "."

# Senders/Payees to ignore completely during import
ignore_payees = [
    "Umbuchung",
    "Sparkonto"
]

# Keyword rules for automatic categorization (case-insensitive)
[categories]
"Lebensmittel" = ["rewe", "edeka", "aldi", "lidl", "kaufland"]
"Mobilität" = ["db vertrieb", "tanke", "shell", "aral"]
"Wohnen" = ["stadtwerke", "vodafone", "telekom"]
```

---

## Installation & Setup

### 1. Build the Release Binary
Clone the repository and compile the optimized release binary:
```bash
cargo build --release
```

### 2. Install to User Path
To run `bank_tui` from anywhere in your terminal, copy the binary to `~/.local/bin`:
```bash
mkdir -p ~/.local/bin
cp target/release/bank_tui ~/.local/bin/
```

Make sure `~/.local/bin` is in your shell's `$PATH` (e.g. in `~/.bashrc` or `~/.zshrc`):
```bash
export PATH="$HOME/.local/bin:$PATH"
```

---

## Usage

Navigate to any directory containing your bank CSV files, then launch the app:

```bash
bank_tui
```

---

## Keybindings

- `j` / `k` or `Down` / `Up`: Navigate through transaction records
- `Tab` / `Shift+Tab`: Switch tabs (e.g., Transactions, Categories, Analytics)
- `/`: Open dynamic filter/search
- `m`: Map the selected transaction to a category
  - Choose the matching source (`a` = payee, `v` = purpose)
  - Pick an existing category from the list (`j`/`k`, `Enter`) or press `n` to create a new one
  - Confirm/adjust the keyword that should trigger the category
- `Esc`: Clear search / Back
- `q`: Quit application