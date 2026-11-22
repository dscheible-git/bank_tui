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

### Options

```bash
bank_tui --report        # write the report and exit (no TUI, e.g. from cron)
bank_tui --report --pdf  # additionally render a PDF
bank_tui --no-report     # start the TUI without touching the report
bank_tui --help
```

---

## Mobile Report (phone / tablet)

The app writes a self-contained HTML report on every start and after every
category mapping. Because it lives next to the CSVs, any folder sync
(Nextcloud, Syncthing, ...) puts it on your phone automatically.

- `report_path` (default `report.html`, relative to `csv_dir`, empty disables it)
- `report_pdf` (default `false`): additionally renders a PDF via headless
  Chromium in the background. Nextcloud can preview PDFs inline, which is the
  quickest way to look at the numbers on a phone.

The HTML report is mobile-first: a 12-month history chart, compact summary
cards, tap-to-expand category cards and monthly accordions, and a booking
list with sticky search, category filter chips, sorting and collapsible
transaction details. Desktop and PDF keep the full tables. It works offline
and can be added to the home screen. Both files contain only data already
present in your CSV exports.

---

## Keybindings

- `j` / `k` or `Down` / `Up`: Navigate through transaction records
- `h` / `l`: Scroll tables horizontally; the first column stays pinned in the category tables
- `Tab` / `Shift+Tab`: Switch tabs (e.g., Transactions, Categories, Analytics)
- `f`: Open the filter popup with all filters and their current values — text search, min/max amount, start/end date, category, and reset (`j`/`k` to choose, `Enter` to edit/apply, `Esc` to close)
- `r`: Regenerate the HTML/PDF report
- `m`: Map the selected transaction to a category
  - Choose the matching source (`a` = payee, `v` = purpose)
  - Pick an existing category from the list (`j`/`k`, `Enter`) or press `n` to create a new one
  - Confirm/adjust the keyword that should trigger the category
- `Esc`: Clear search / Back
- `q`: Quit application