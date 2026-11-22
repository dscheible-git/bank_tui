mod report;

use chrono::NaiveDate;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs},
    Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Config {
    #[serde(default = "default_csv_dir")]
    csv_dir: String,
    #[serde(default)]
    ignore_payees: Vec<String>,
    #[serde(default)]
    categories: BTreeMap<String, Vec<String>>,
    /// Path of the generated HTML report (relative to `csv_dir`). Empty disables it.
    #[serde(default = "default_report_path")]
    report_path: String,
    /// Additionally render a PDF next to the HTML report (requires Chromium).
    #[serde(default)]
    report_pdf: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            csv_dir: default_csv_dir(),
            ignore_payees: Vec::new(),
            categories: BTreeMap::new(),
            report_path: default_report_path(),
            report_pdf: false,
        }
    }
}

fn default_csv_dir() -> String {
    ".".to_string()
}

fn default_report_path() -> String {
    "report.html".to_string()
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("bank_tui").join("config.toml")
}

fn load_config() -> Config {
    let path = config_path();
    fs::read_to_string(&path)
        .or_else(|_| fs::read_to_string("config.toml"))
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(config)?;
    fs::write(&path, toml_str)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Transaction {
    buchung: String,
    wertstellung: String,
    payee: String,
    text: String,
    purpose: String,
    amount_cents: i64,
    currency: String,
    date: NaiveDate,
    category: String,
}

fn parse_german_amount(s: &str) -> Option<i64> {
    let s = s.trim().replace('.', "").replace(',', ".");
    let float_val: f64 = s.parse().ok()?;
    if float_val >= 0.0 {
        return None;
    }
    Some((float_val.abs() * 100.0).round() as i64)
}

fn assign_category(payee: &str, text: &str, purpose: &str, config: &Config) -> String {
    let combined = format!("{} {} {}", payee, text, purpose).to_lowercase();
    for (cat, keywords) in &config.categories {
        for kw in keywords {
            if combined.contains(&kw.to_lowercase()) {
                return cat.clone();
            }
        }
    }
    "Unkategorisiert".to_string()
}

fn load_transactions_from_folder<P: AsRef<Path>>(dir: P, config: &Config) -> Vec<Transaction> {
    let mut seen_records: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut transactions = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return transactions,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("csv") {
            continue;
        }

        let file_name = path.to_string_lossy().to_string();
        let mut file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };

        // Bytes komplett einlesen und für ISO-8859-1 / Windows-1252 / UTF-8 lossy decodieren
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        // 1. Dynamisch die Kopfzeile finden, die das Wort "buchung" enthält
        let header_idx = match lines.iter().position(|line| {
            let cleaned = line.trim().trim_start_matches('\u{feff}').to_lowercase();
            cleaned.contains("buchung")
        }) {
            Some(idx) => idx,
            None => {
                //eprintln!("[Übersprungen] Keine Zeile mit 'Buchung' in Datei {} gefunden.", file_name);
                continue;
            }
        };

        let csv_lines: Vec<String> = lines.into_iter().skip(header_idx).collect();
        if csv_lines.is_empty() {
            continue;
        }

        let csv_data = csv_lines.join("\n");
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(b';')
            .has_headers(true)
            .flexible(true)
            .from_reader(csv_data.as_bytes());

        // Header dynamisch auslesen
        let headers = match rdr.headers() {
            Ok(h) => h.clone(),
            Err(_) => {
                //eprintln!("[Fehler] Konnte Header in Datei {} nicht lesen: {}", file_name, e);
                continue;
            }
        };

        let find_col = |keywords: &[&str]| -> Option<usize> {
            for (i, h) in headers.iter().enumerate() {
                let h_lower = h.to_lowercase();
                if keywords.iter().any(|kw| h_lower.contains(&kw.to_lowercase())) {
                    return Some(i);
                }
            }
            None
        };

        let idx_buchung = find_col(&["buchung"]);
        let idx_wertstellung = find_col(&["wertstellung"]);
        let idx_payee = find_col(&["auftraggeber", "empfänger", "empfaenger"]);
        let idx_text = find_col(&["buchungstext"]);
        let idx_purpose = find_col(&["verwendungszweck"]);
        let idx_amount = find_col(&["betrag"]);
        let idx_currency = find_col(&["währung", "waehrung"]);

        if idx_buchung.is_none() || idx_amount.is_none() {
            //eprintln!(
            //    "[Übersprungen] Datei {} hat ein unbekanntes Format (Pflichtspalten 'Buchung' oder 'Betrag' fehlen). Vorhandene Header: {:?}",
            //    file_name, headers
            //);
            continue;
        }

        for result in rdr.records() {
            let record = match result {
                Ok(r) => r,
                Err(_) => {
                    //eprintln!("[Nicht hinzugefügt] Datei: {} | Grund: CSV-Parsing-Fehler ({})", file_name, e);
                    continue;
                }
            };

            let current_raw_line = record.iter().map(|s| s.to_string()).collect::<Vec<String>>().join(";");

            let get_col = |idx_opt: Option<usize>| -> String {
                idx_opt
                    .and_then(|i| record.get(i))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default()
            };

            let buchung = get_col(idx_buchung);
            let wertstellung = get_col(idx_wertstellung);
            let payee = get_col(idx_payee);
            let text = get_col(idx_text);
            let purpose = get_col(idx_purpose);
            let amount_str = get_col(idx_amount);
            let currency = get_col(idx_currency);

            if buchung.is_empty() || amount_str.is_empty() {
                //eprintln!("[Nicht hinzugefügt] Datei: {} | Grund: Pflichtfeld 'Buchung' oder 'Betrag' leer | Zeile: [{}]", file_name, current_raw_line);
                continue;
            }

            let dedup_key = format!("{}|{}|{}|{}|{}|{}", buchung, payee, text, purpose, amount_str, currency);
            
            if seen_records.contains_key(&dedup_key) {
                continue;
            }
            seen_records.insert(dedup_key, current_raw_line.clone());

            if config.ignore_payees.iter().any(|p| payee.to_lowercase().contains(&p.to_lowercase())) {
                continue;
            }

            let amount_cents = match parse_german_amount(&amount_str) {
                Some(amt) => amt,
                None => {
                    //eprintln!(
                    //    "[Nicht hinzugefügt] Datei: {} | Grund: Betrag '{}' ist positiv, 0 oder ungültig (nur Ausgaben erlaubt) | Zeile: [{}]",
                    //    file_name, amount_str, current_raw_line
                    //);
                    continue;
                }
            };

            let date = match NaiveDate::parse_from_str(&buchung, "%d.%m.%Y") {
                Ok(d) => d,
                Err(e) => {
                    eprintln!(
                        "[Nicht hinzugefügt] Datei: {} | Grund: Ungültiges Buchungsdatum '{}' (Fehler: {}) | Zeile: [{}]",
                        file_name, buchung, e, current_raw_line
                    );
                    continue;
                }
            };

            let category = assign_category(&payee, &text, &purpose, config);

            transactions.push(Transaction {
                buchung,
                wertstellung,
                payee,
                text,
                purpose,
                amount_cents,
                currency,
                date,
                category,
            });
        }
    }

    transactions.sort_by_key(|a| std::cmp::Reverse(a.date));
    transactions
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    SearchText,
    FilterMinAmount,
    FilterMaxAmount,
    FilterStartDate,
    FilterEndDate,
    FilterCategorySelect,
    MappingSourceSelect,
    MappingCategorySelect,
    MappingCategoryName,
    MappingKeyword,
}

enum MappingSource {
    Payee,
    Purpose,
}

struct App {
    config: Config,
    tab_index: usize,
    all_transactions: Vec<Transaction>,
    filtered_transactions: Vec<Transaction>,
    table_state: TableState,

    // Scrolling für Tabellen (Horizontal & Vertikal)
    scroll_x_tab0: usize,
    scroll_y_tab1: usize,
    scroll_x_tab1: usize,
    scroll_y_tab2: usize,
    scroll_x_tab2: usize,

    // Filter-Zustände
    input_mode: InputMode,
    input_buffer: String,
    filter_search: String,
    filter_min_amount: Option<f64>,
    filter_max_amount: Option<f64>,
    filter_start_date: Option<NaiveDate>,
    filter_end_date: Option<NaiveDate>,
    filter_category: Option<String>,
    filter_category_state: ListState,

    // Interaktives Mapping
    mapping_source: Option<MappingSource>,
    mapping_category: String,
    mapping_keyword: String,
    mapping_category_state: ListState,

    // Kurze Statusmeldung (z. B. nach dem Schreiben des Reports)
    status: Option<String>,
    last_html: Option<PathBuf>,
    pdf_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    pdf_dirty: bool,
}

impl App {
    fn new(config: Config, transactions: Vec<Transaction>) -> Self {
        let mut app = Self {
            config,
            tab_index: 0,
            all_transactions: transactions.clone(),
            filtered_transactions: transactions,
            table_state: TableState::default(),
            scroll_x_tab0: 0,
            scroll_y_tab1: 0,
            scroll_x_tab1: 0,
            scroll_y_tab2: 0,
            scroll_x_tab2: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            filter_search: String::new(),
            filter_min_amount: None,
            filter_max_amount: None,
            filter_start_date: None,
            filter_end_date: None,
            filter_category: None,
            filter_category_state: ListState::default(),
            mapping_source: None,
            mapping_category: String::new(),
            mapping_keyword: String::new(),
            mapping_category_state: ListState::default(),
            status: None,
            last_html: None,
            pdf_rx: None,
            pdf_dirty: false,
        };
        if !app.filtered_transactions.is_empty() {
            app.table_state.select(Some(0));
        }
        app
    }

    /// Applies the active filters to the Buchungsliste and returns the matching
    /// transactions without mutating the app state.
    fn filter_buchungsliste(&self) -> Vec<Transaction> {
        let search_lower = self.filter_search.to_lowercase();
        self.all_transactions
            .iter()
            .filter(|tx| {
                if !search_lower.is_empty() {
                    let matches_payee = tx.payee.to_lowercase().contains(&search_lower);
                    let matches_text = tx.text.to_lowercase().contains(&search_lower);
                    let matches_purpose = tx.purpose.to_lowercase().contains(&search_lower);
                    let matches_cat = tx.category.to_lowercase().contains(&search_lower);
                    if !(matches_payee || matches_text || matches_purpose || matches_cat) {
                        return false;
                    }
                }
                if let Some(category) = &self.filter_category {
                    if &tx.category != category {
                        return false;
                    }
                }
                let val_eur = tx.amount_cents as f64 / 100.0;
                if let Some(min) = self.filter_min_amount {
                    if val_eur < min {
                        return false;
                    }
                }
                if let Some(max) = self.filter_max_amount {
                    if val_eur > max {
                        return false;
                    }
                }
                if let Some(start) = self.filter_start_date {
                    if tx.date < start {
                        return false;
                    }
                }
                if let Some(end) = self.filter_end_date {
                    if tx.date > end {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    fn apply_filters(&mut self) {
        self.filtered_transactions = self.filter_buchungsliste();

        if self.filtered_transactions.is_empty() {
            self.table_state.select(None);
        } else {
            let current = self.table_state.selected().unwrap_or(0);
            if current >= self.filtered_transactions.len() {
                self.table_state.select(Some(self.filtered_transactions.len() - 1));
            } else {
                self.table_state.select(Some(current));
            }
        }
    }

    fn category_filter_options(&self) -> Vec<String> {
        let mut categories: BTreeSet<String> = self.config.categories.keys().cloned().collect();
        categories.extend(self.all_transactions.iter().map(|tx| tx.category.clone()));
        if let Some(current) = &self.filter_category {
            categories.insert(current.clone());
        }
        categories.into_iter().collect()
    }

    fn start_category_filter(&mut self) {
        let options = self.category_filter_options();
        let selected = self
            .filter_category
            .as_ref()
            .and_then(|current| options.iter().position(|c| c == current))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        self.filter_category_state = ListState::default();
        self.filter_category_state.select(Some(selected));
        self.input_mode = InputMode::FilterCategorySelect;
    }

    fn next_filter_category(&mut self) {
        let len = self.category_filter_options().len() + 1; // + "Alle Kategorien"
        let i = match self.filter_category_state.selected() {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.filter_category_state.select(Some(i));
    }

    fn prev_filter_category(&mut self) {
        let len = self.category_filter_options().len() + 1;
        let i = match self.filter_category_state.selected() {
            Some(i) => if i == 0 { len - 1 } else { i - 1 },
            None => 0,
        };
        self.filter_category_state.select(Some(i));
    }

    fn select_filter_category(&mut self) {
        let options = self.category_filter_options();
        let selected = self.filter_category_state.selected().unwrap_or(0);
        self.filter_category = if selected == 0 {
            None
        } else {
            options.get(selected - 1).cloned()
        };
        self.input_mode = InputMode::Normal;
        self.apply_filters();
    }

    fn next_tab(&mut self) {
        self.tab_index = (self.tab_index + 1) % 3;
    }

    fn prev_tab(&mut self) {
        self.tab_index = if self.tab_index == 0 { 2 } else { self.tab_index - 1 };
    }

    fn next_row(&mut self) {
        match self.tab_index {
            0 => {
                if self.filtered_transactions.is_empty() { return; }
                let i = match self.table_state.selected() {
                    Some(i) => (i + 1) % self.filtered_transactions.len(),
                    None => 0,
                };
                self.table_state.select(Some(i));
            }
            1 => self.scroll_y_tab1 = self.scroll_y_tab1.saturating_add(1),
            2 => self.scroll_y_tab2 = self.scroll_y_tab2.saturating_add(1),
            _ => {}
        }
    }

    fn prev_row(&mut self) {
        match self.tab_index {
            0 => {
                if self.filtered_transactions.is_empty() { return; }
                let i = match self.table_state.selected() {
                    Some(i) => if i == 0 { self.filtered_transactions.len() - 1 } else { i - 1 },
                    None => 0,
                };
                self.table_state.select(Some(i));
            }
            1 => self.scroll_y_tab1 = self.scroll_y_tab1.saturating_sub(1),
            2 => self.scroll_y_tab2 = self.scroll_y_tab2.saturating_sub(1),
            _ => {}
        }
    }

    fn scroll_left(&mut self) {
        match self.tab_index {
            0 => self.scroll_x_tab0 = self.scroll_x_tab0.saturating_sub(1),
            1 => self.scroll_x_tab1 = self.scroll_x_tab1.saturating_sub(1),
            2 => self.scroll_x_tab2 = self.scroll_x_tab2.saturating_sub(1),
            _ => {}
        }
    }

    fn scroll_right(&mut self) {
        match self.tab_index {
            0 => self.scroll_x_tab0 = self.scroll_x_tab0.saturating_add(1),
            1 => self.scroll_x_tab1 = self.scroll_x_tab1.saturating_add(1),
            2 => self.scroll_x_tab2 = self.scroll_x_tab2.saturating_add(1),
            _ => {}
        }
    }

    fn save_and_reapply_mapping(&mut self) {
        let kw = self.mapping_keyword.trim().to_string();
        let cat = self.mapping_category.trim().to_string();
        if kw.is_empty() || cat.is_empty() { return; }

        let keywords = self.config.categories.entry(cat).or_default();
        if !keywords.iter().any(|k| k.eq_ignore_ascii_case(&kw)) {
            keywords.push(kw);
            let _ = save_config(&self.config);
            for tx in &mut self.all_transactions {
                tx.category = assign_category(&tx.payee, &tx.text, &tx.purpose, &self.config);
            }
            self.apply_filters();
            self.refresh_report();
        }
    }

    /// Regenerates the report from the currently loaded transactions. The HTML
    /// is written synchronously (milliseconds), the optional PDF is rendered in
    /// a background thread so the TUI stays responsive.
    fn refresh_report(&mut self) {
        match report::generate_html(&self.config, &self.all_transactions) {
            Ok(Some(html)) => {
                let time = chrono::Local::now().format("%H:%M");
                self.status = Some(format!("Report aktualisiert ({})", time));
                self.last_html = Some(html);
                if self.config.report_pdf {
                    self.start_pdf();
                }
            }
            Ok(None) => {
                self.last_html = None;
                self.pdf_dirty = false;
                self.status = None;
            }
            Err(err) => {
                eprintln!("[report] {}", err);
                self.status = Some(format!("Report-Fehler: {}", err));
            }
        }
    }

    fn start_pdf(&mut self) {
        let Some(html) = self.last_html.clone() else {
            return;
        };
        if self.pdf_rx.is_some() {
            // A render is already running: redo it once it has finished.
            self.pdf_dirty = true;
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pdf = html.with_extension("pdf");
            let _ = tx.send(report::write_pdf(&html, &pdf));
        });
        self.pdf_rx = Some(rx);
        self.status = Some("PDF wird erzeugt …".to_string());
    }

    fn poll_pdf(&mut self) {
        use std::sync::mpsc::TryRecvError;

        let Some(rx) = self.pdf_rx.as_ref() else {
            return;
        };
        let result = rx.try_recv();
        match result {
            Ok(Ok(())) => {
                self.pdf_rx = None;
                let time = chrono::Local::now().format("%H:%M");
                self.status = Some(format!("PDF erzeugt ({})", time));
            }
            Ok(Err(err)) => {
                eprintln!("[report] {}", err);
                self.pdf_rx = None;
                self.status = Some("PDF fehlgeschlagen (Chromium nicht verfügbar?)".to_string());
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.pdf_rx = None;
            }
        }

        if self.pdf_dirty {
            self.pdf_dirty = false;
            self.start_pdf();
        }
    }

    fn start_mapping(&mut self, source: MappingSource, keyword: String, current_category: &str) {
        self.mapping_source = Some(source);
        self.mapping_keyword = keyword;

        // Vorhandene Kategorie der Buchung vorauswählen, sonst die erste Kategorie.
        let selected = self
            .config
            .categories
            .keys()
            .position(|c| c == current_category)
            .unwrap_or(0);
        self.mapping_category_state = ListState::default();
        self.mapping_category_state.select(Some(selected));

        self.input_mode = InputMode::MappingCategorySelect;
    }

    fn next_mapping_category(&mut self) {
        let len = self.config.categories.len() + 1; // + "Neue Kategorie anlegen"
        let i = match self.mapping_category_state.selected() {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.mapping_category_state.select(Some(i));
    }

    fn prev_mapping_category(&mut self) {
        let len = self.config.categories.len() + 1;
        let i = match self.mapping_category_state.selected() {
            Some(i) => if i == 0 { len - 1 } else { i - 1 },
            None => 0,
        };
        self.mapping_category_state.select(Some(i));
    }
}

#[derive(Default)]
struct Args {
    report_only: bool,
    pdf: bool,
    no_report: bool,
    help: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--report" | "-r" => args.report_only = true,
            "--pdf" => args.pdf = true,
            "--no-report" => args.no_report = true,
            "--help" | "-h" => args.help = true,
            other => return Err(format!("Unbekannte Option: {other}")),
        }
    }
    Ok(args)
}

fn print_help() {
    println!(
        "bank_tui {}\n\
         \n\
         Aufruf: bank_tui [OPTIONEN]\n\
         \n\
         Ohne Optionen startet die interaktive TUI. Der HTML-Report wird beim\n\
         Start und nach jeder Kategorie-Zuordnung automatisch neu geschrieben\n\
         (siehe report_path in der Konfiguration).\n\
         \n\
         Optionen:\n\
           -r, --report    Report erzeugen und beenden (ohne TUI, z. B. für cron)\n\
               --pdf       Zusätzlich zum HTML-Report ein PDF erzeugen (Chromium)\n\
               --no-report TUI starten, aber keinen Report schreiben\n\
           -h, --help      Diese Hilfe anzeigen\n\
         \n\
         Konfiguration: ~/.config/bank_tui/config.toml",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    if args.help {
        print_help();
        return Ok(());
    }

    let mut config = load_config();
    if args.pdf {
        config.report_pdf = true;
    }
    if args.no_report {
        config.report_path.clear();
    }

    let transactions = load_transactions_from_folder(&config.csv_dir, &config);

    if args.report_only {
        match report::generate(&config, &transactions) {
            Ok(Some(outcome)) => {
                println!("HTML: {}", outcome.html.display());
                if let Some(pdf) = outcome.pdf {
                    println!("PDF:  {}", pdf.display());
                }
                if let Some(warning) = outcome.warning {
                    eprintln!("[report] {}", warning);
                }
            }
            Ok(None) => println!("Report ist deaktiviert (report_path ist leer)."),
            Err(err) => {
                eprintln!("[report] {}", err);
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    let mut app = App::new(config, transactions);
    app.refresh_report();

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        app.poll_pdf();
        terminal.draw(|f| ui(f, &mut app))?;

        if !event::poll(std::time::Duration::from_millis(200))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            match app.input_mode {
                InputMode::MappingSourceSelect => {
                    match key.code {
                        KeyCode::Char('a') | KeyCode::Char('1') => {
                            if let Some(idx) = app.table_state.selected() {
                                if let Some(tx) = app.filtered_transactions.get(idx).cloned() {
                                    app.start_mapping(MappingSource::Payee, tx.payee.clone(), &tx.category);
                                }
                            }
                        }
                        KeyCode::Char('v') | KeyCode::Char('2') => {
                            if let Some(idx) = app.table_state.selected() {
                                if let Some(tx) = app.filtered_transactions.get(idx).cloned() {
                                    app.start_mapping(MappingSource::Purpose, tx.purpose.clone(), &tx.category);
                                }
                            }
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    }
                }
                InputMode::MappingCategorySelect => {
                    match key.code {
                        KeyCode::Char('j') | KeyCode::Down => app.next_mapping_category(),
                        KeyCode::Char('k') | KeyCode::Up => app.prev_mapping_category(),
                        KeyCode::Char('n') => {
                            // Direkt eine neue Kategorie anlegen.
                            let new_idx = app.config.categories.len();
                            app.mapping_category_state.select(Some(new_idx));
                            app.mapping_category.clear();
                            app.input_buffer.clear();
                            app.input_mode = InputMode::MappingCategoryName;
                        }
                        KeyCode::Enter => {
                            let categories: Vec<String> = app.config.categories.keys().cloned().collect();
                            if let Some(idx) = app.mapping_category_state.selected() {
                                if idx < categories.len() {
                                    app.mapping_category = categories[idx].clone();
                                    app.input_buffer = app.mapping_keyword.clone();
                                    app.input_mode = InputMode::MappingKeyword;
                                } else {
                                    app.mapping_category.clear();
                                    app.input_buffer.clear();
                                    app.input_mode = InputMode::MappingCategoryName;
                                }
                            }
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                        }
                        _ => {}
                    }
                }
                InputMode::MappingCategoryName => {
                    match key.code {
                        KeyCode::Enter => {
                            let name = app.input_buffer.trim().to_string();
                            if !name.is_empty() {
                                app.mapping_category = name;
                                app.input_buffer = app.mapping_keyword.clone();
                                app.input_mode = InputMode::MappingKeyword;
                            }
                        }
                        KeyCode::Esc => {
                            app.input_buffer.clear();
                            app.input_mode = InputMode::MappingCategorySelect;
                        }
                        KeyCode::Backspace => { app.input_buffer.pop(); }
                        KeyCode::Char(c) => { app.input_buffer.push(c); }
                        _ => {}
                    }
                }
                InputMode::MappingKeyword => {
                    match key.code {
                        KeyCode::Enter => {
                            app.mapping_keyword = std::mem::take(&mut app.input_buffer);
                            app.save_and_reapply_mapping();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Esc => app.input_mode = InputMode::Normal,
                        KeyCode::Backspace => { app.input_buffer.pop(); }
                        KeyCode::Char(c) => { app.input_buffer.push(c); }
                        _ => {}
                    }
                }
                InputMode::FilterCategorySelect => {
                    match key.code {
                        KeyCode::Char('j') | KeyCode::Down => app.next_filter_category(),
                        KeyCode::Char('k') | KeyCode::Up => app.prev_filter_category(),
                        KeyCode::Enter => app.select_filter_category(),
                        KeyCode::Esc => app.input_mode = InputMode::Normal,
                        _ => {}
                    }
                }
                InputMode::SearchText | InputMode::FilterMinAmount | InputMode::FilterMaxAmount | InputMode::FilterStartDate | InputMode::FilterEndDate => {
                    match key.code {
                        KeyCode::Enter => {
                            let input = std::mem::take(&mut app.input_buffer);
                            match app.input_mode {
                                InputMode::SearchText => app.filter_search = input,
                                InputMode::FilterMinAmount => app.filter_min_amount = input.parse().ok(),
                                InputMode::FilterMaxAmount => app.filter_max_amount = input.parse().ok(),
                                InputMode::FilterStartDate => {
                                    app.filter_start_date = NaiveDate::parse_from_str(&input, "%d.%m.%Y").ok();
                                }
                                InputMode::FilterEndDate => {
                                    app.filter_end_date = NaiveDate::parse_from_str(&input, "%d.%m.%Y").ok();
                                }
                                _ => {}
                            }
                            app.apply_filters();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Esc => {
                            app.input_buffer.clear();
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Backspace => { app.input_buffer.pop(); }
                        KeyCode::Char(c) => { app.input_buffer.push(c); }
                        _ => {}
                    }
                }
                InputMode::Normal => {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('l') => app.scroll_right(),
                        KeyCode::Char('h') => app.scroll_left(),
                        KeyCode::Char('j') => app.next_row(),
                        KeyCode::Char('k') => app.prev_row(),
                        KeyCode::Tab | KeyCode::Right => app.next_tab(),
                        KeyCode::Left => app.prev_tab(),
                        KeyCode::Char(']') => app.next_tab(),
                        KeyCode::Char('[') => app.prev_tab(),
                        KeyCode::Char('/') => {
                            app.input_mode = InputMode::SearchText;
                            app.input_buffer = app.filter_search.clone();
                        }
                        KeyCode::Char('i') => {
                            app.input_mode = InputMode::FilterMinAmount;
                            app.input_buffer = app.filter_min_amount.map(|v| v.to_string()).unwrap_or_default();
                        }
                        KeyCode::Char('a') => {
                            app.input_mode = InputMode::FilterMaxAmount;
                            app.input_buffer = app.filter_max_amount.map(|v| v.to_string()).unwrap_or_default();
                        }
                        KeyCode::Char('s') => {
                            app.input_mode = InputMode::FilterStartDate;
                            app.input_buffer = app.filter_start_date.map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_default();
                        }
                        KeyCode::Char('e') => {
                            app.input_mode = InputMode::FilterEndDate;
                            app.input_buffer = app.filter_end_date.map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_default();
                        }
                        KeyCode::Char('f') => app.start_category_filter(),
                        KeyCode::Char('c') => {
                            app.filter_search.clear();
                            app.filter_min_amount = None;
                            app.filter_max_amount = None;
                            app.filter_start_date = None;
                            app.filter_end_date = None;
                            app.filter_category = None;
                            app.apply_filters();
                        }
                        KeyCode::Char('r') => app.refresh_report(),
                        KeyCode::Char('m')
                            if app.tab_index == 0 && !app.filtered_transactions.is_empty() =>
                        {
                            app.input_mode = InputMode::MappingSourceSelect;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Navigation Tabs
            Constraint::Length(3), // Filter-Bar / Status
            Constraint::Min(0),    // Hauptinhalt
        ].as_ref())
        .split(f.area());

    let titles = vec!["1. Buchungsliste", "2. Monat & Kategorie", "3. Jahressummen & Avg"];
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" Bank Ausgaben TUI (Vim: h/j/k/l | Tabs: Tab/Left/Right oder [/]) "))
        .select(app.tab_index)
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    f.render_widget(tabs, chunks[0]);

    let min_str = app.filter_min_amount.map(|v| format!("{:.2}€", v)).unwrap_or("-".to_string());
    let max_str = app.filter_max_amount.map(|v| format!("{:.2}€", v)).unwrap_or("-".to_string());
    let start_str = app.filter_start_date.map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or("-".to_string());
    let end_str = app.filter_end_date.map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or("-".to_string());
    let category_str = app.filter_category.clone().unwrap_or_else(|| "Alle".to_string());

    let filter_text = match app.input_mode {
        InputMode::SearchText => format!("SUCHE TEXT > {}_", app.input_buffer),
        InputMode::FilterMinAmount => format!("FILTER MIN BETRAG > {}_", app.input_buffer),
        InputMode::FilterMaxAmount => format!("FILTER MAX BETRAG > {}_", app.input_buffer),
        InputMode::FilterStartDate => format!("FILTER START DATUM (DD.MM.YYYY) > {}_", app.input_buffer),
        InputMode::FilterEndDate => format!("FILTER END DATUM (DD.MM.YYYY) > {}_", app.input_buffer),
        InputMode::FilterCategorySelect => "[FILTER] Kategorie wählen: j/k oder ↑/↓, Enter | [Esc] Abbrechen".to_string(),
        InputMode::MappingSourceSelect => "[MAPPING] Quelle wählen: [a] Auftraggeber/Empfänger | [v] Verwendungszweck | [Esc] Abbrechen".to_string(),
        InputMode::MappingCategorySelect => "[MAPPING] Kategorie wählen: j/k oder ↑/↓, Enter | [n] Neue Kategorie | [Esc] Abbrechen".to_string(),
        InputMode::MappingCategoryName => format!("[MAPPING] Neue Kategorie eingeben > {}_", app.input_buffer),
        InputMode::MappingKeyword => format!(
            "[MAPPING] Schlüsselwort für \"{}\" bestätigen/anpassen > {}_",
            app.mapping_category, app.input_buffer
        ),
        InputMode::Normal => format!(
            " [/]Suche: \"{}\" | [i]Min: {} | [a]Max: {} | [s]Start: {} | [e]Ende: {} | [f]Kategorie: {} | [c]Reset | [m]Kategorie Mappen | [r]Report",
            app.filter_search, min_str, max_str, start_str, end_str, category_str
        ),
    };

    let filter_text = match &app.status {
        Some(status) => format!("{} | {}", filter_text, status),
        None => filter_text,
    };

    let filter_p = Paragraph::new(filter_text)
        .block(Block::default().borders(Borders::ALL).title(" Filter & Mapping Steuerung "))
        .style(if app.input_mode != InputMode::Normal { Style::default().fg(Color::Yellow) } else { Style::default().fg(Color::Cyan) });
    f.render_widget(filter_p, chunks[1]);

    match app.tab_index {
        0 => render_list_tab(f, app, chunks[2]),
        1 => render_monthly_tab(f, app, chunks[2]),
        2 => render_annual_avg_tab(f, app, chunks[2]),
        _ => {}
    }

    if app.input_mode == InputMode::MappingCategorySelect {
        render_category_select_popup(f, app);
    }

    if app.input_mode == InputMode::FilterCategorySelect {
        render_filter_category_popup(f, app);
    }
}

fn render_filter_category_popup(f: &mut ratatui::Frame, app: &mut App) {
    let categories = app.category_filter_options();
    let area = f.area();

    let popup_width = area.width.saturating_sub(4).clamp(20, 64).min(area.width);
    let item_count = categories.len() + 1; // + "Alle Kategorien"
    let popup_height = (item_count as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(1);

    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    let mut items: Vec<ListItem> = vec![
        ListItem::new("Alle Kategorien")
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
    ];
    items.extend(categories.iter().map(|c| ListItem::new(c.clone())));

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Buchungsliste filtern | j/k Enter | Esc "))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_widget(Clear, popup_area);
    f.render_stateful_widget(list, popup_area, &mut app.filter_category_state);
}

fn render_category_select_popup(f: &mut ratatui::Frame, app: &mut App) {
    let categories: Vec<String> = app.config.categories.keys().cloned().collect();
    let area = f.area();

    let popup_width = area.width.saturating_sub(4).clamp(20, 64).min(area.width);
    let item_count = categories.len() + 1; // + "Neue Kategorie anlegen"
    let popup_height = (item_count as u16 + 2)
        .min(area.height.saturating_sub(2))
        .max(1);

    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    let mut items: Vec<ListItem> = categories
        .iter()
        .map(|c| ListItem::new(c.clone()))
        .collect();
    items.push(
        ListItem::new("+ Neue Kategorie anlegen")
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
    );

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Kategorie wählen | j/k Enter | n neu | Esc "))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_widget(Clear, popup_area);
    f.render_stateful_widget(list, popup_area, &mut app.mapping_category_state);
}

fn slice_constraints(constraints: &[Constraint], offset: usize) -> Vec<Constraint> {
    if offset >= constraints.len() {
        return vec![];
    }
    constraints[offset..].to_vec()
}

fn slice_cells<'a>(cells: Vec<Cell<'a>>, offset: usize) -> Vec<Cell<'a>> {
    if offset >= cells.len() {
        return vec![];
    }
    cells.into_iter().skip(offset).collect()
}

/// Hält die erste Spalte beim horizontalen Scrollen sichtbar und verschiebt
/// nur die übrigen Spalten um `offset`.
fn with_pinned_first<T>(items: impl IntoIterator<Item = T>, offset: usize) -> Vec<T> {
    let mut items = items.into_iter();
    let mut visible = Vec::new();
    if let Some(first) = items.next() {
        visible.push(first);
        visible.extend(items.skip(offset));
    }
    visible
}

fn render_list_tab(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let all_headers = ["Datum", "Empfänger/Auftraggeber", "Kategorie", "Betrag", "Buchungstext", "Verwendungszweck"];
    let all_constraints = [
        Constraint::Length(12),
        Constraint::Length(25),
        Constraint::Length(18),
        Constraint::Length(14),
        Constraint::Length(25),
        Constraint::Min(30),
    ];

    let header_cells: Vec<Cell> = all_headers
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
        .collect();

    let visible_headers = slice_cells(header_cells, app.scroll_x_tab0);
    let visible_constraints = slice_constraints(&all_constraints, app.scroll_x_tab0);

    let header = Row::new(visible_headers).height(1);

    let rows = app.filtered_transactions.iter().map(|tx| {
        let amount_str = format!("-{:.2} €", tx.amount_cents as f64 / 100.0).replace('.', ",");
        let all_row_cells = vec![
            Cell::from(tx.buchung.clone()),
            Cell::from(tx.payee.clone()),
            Cell::from(tx.category.clone()).style(Style::default().fg(Color::Yellow)),
            Cell::from(amount_str).style(Style::default().fg(Color::Red)),
            Cell::from(tx.text.clone()),
            Cell::from(tx.purpose.clone()),
        ];
        Row::new(slice_cells(all_row_cells, app.scroll_x_tab0))
    });

    let table = Table::new(rows, visible_constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(" Ausgaben Liste ({}) | Vertikal: j/k, Horizontal: h/l ", app.filtered_transactions.len())))
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_monthly_tab(f: &mut ratatui::Frame, app: &App, area: Rect) {
    if app.filtered_transactions.is_empty() {
        let p = Paragraph::new("Keine Ausgaben vorhanden.").block(Block::default().borders(Borders::ALL));
        f.render_widget(p, area);
        return;
    }

    let mut monthly_cat: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut categories_set: BTreeSet<String> = BTreeSet::new();

    for tx in &app.filtered_transactions {
        let month_key = tx.date.format("%Y-%m").to_string();
        categories_set.insert(tx.category.clone());
        *monthly_cat.entry(month_key).or_default().entry(tx.category.clone()).or_insert(0) += tx.amount_cents;
    }

    let categories: Vec<String> = categories_set.into_iter().collect();

    let mut header_titles = vec!["Monat (YYYY-MM)".to_string()];
    header_titles.extend(categories.clone());
    header_titles.push("Monatssumme".to_string());

    let mut all_constraints = vec![Constraint::Length(16)];
    for _ in 0..categories.len() {
        all_constraints.push(Constraint::Length(15));
    }
    all_constraints.push(Constraint::Length(18));

    let header_cells: Vec<Cell> = header_titles
        .into_iter()
        .map(|h| Cell::from(h).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
        .collect();

    let visible_headers = with_pinned_first(header_cells, app.scroll_x_tab1);
    let visible_constraints = with_pinned_first(all_constraints, app.scroll_x_tab1);

    let header = Row::new(visible_headers).height(1);

    let mut rows = Vec::new();

    for (month, cat_map) in monthly_cat.into_iter().skip(app.scroll_y_tab1) {
        let mut row_cells = vec![Cell::from(month)];
        let mut month_total = 0i64;

        for cat in &categories {
            let val = cat_map.get(cat).copied().unwrap_or(0);
            month_total += val;
            row_cells.push(Cell::from(format!("-{:.2}", val as f64 / 100.0).replace('.', ",")));
        }

        row_cells.push(Cell::from(format!("-{:.2} €", month_total as f64 / 100.0).replace('.', ",")).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
        rows.push(Row::new(with_pinned_first(row_cells, app.scroll_x_tab1)));
    }

    let table = Table::new(rows, visible_constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Ausgaben pro Monat & Kategorie | Vertikal: j/k, Horizontal: h/l, 1. Spalte fix "));

    f.render_widget(table, area);
}

fn render_annual_avg_tab(f: &mut ratatui::Frame, app: &App, area: Rect) {
    if app.filtered_transactions.is_empty() {
        let p = Paragraph::new("Keine Ausgaben vorhanden.").block(Block::default().borders(Borders::ALL));
        f.render_widget(p, area);
        return;
    }

    let mut cat_annual: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut years_set: BTreeSet<String> = BTreeSet::new();
    let mut months_set: BTreeSet<String> = BTreeSet::new();

    for tx in &app.filtered_transactions {
        let year_key = tx.date.format("%Y").to_string();
        let month_key = tx.date.format("%Y-%m").to_string();
        years_set.insert(year_key.clone());
        months_set.insert(month_key);
        *cat_annual.entry(tx.category.clone()).or_default().entry(year_key).or_insert(0) += tx.amount_cents;
    }

    let years: Vec<String> = years_set.into_iter().collect();
    let num_months = months_set.len().max(1) as f64;

    let mut header_titles = vec!["Kategorie".to_string()];
    header_titles.extend(years.clone());
    header_titles.push("Gesamtsumme".to_string());
    header_titles.push("Ø Mtl. Ausgaben".to_string());

    let mut all_constraints = vec![Constraint::Length(20)];
    for _ in 0..years.len() {
        all_constraints.push(Constraint::Length(14));
    }
    all_constraints.push(Constraint::Length(18));
    all_constraints.push(Constraint::Length(18));

    let header_cells: Vec<Cell> = header_titles
        .into_iter()
        .map(|h| Cell::from(h).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
        .collect();

    let visible_headers = with_pinned_first(header_cells, app.scroll_x_tab2);
    let visible_constraints = with_pinned_first(all_constraints, app.scroll_x_tab2);

    let header = Row::new(visible_headers).height(1);

    let mut rows = Vec::new();
    let mut year_totals: BTreeMap<String, i64> = BTreeMap::new();
    let mut grand_total = 0i64;

    for (cat, yr_map) in cat_annual.into_iter().skip(app.scroll_y_tab2) {
        let mut row_cells = vec![Cell::from(cat)];
        let mut cat_total = 0i64;

        for y in &years {
            let val = yr_map.get(y).copied().unwrap_or(0);
            cat_total += val;
            *year_totals.entry(y.clone()).or_default() += val;
            row_cells.push(Cell::from(format!("-{:.2}", val as f64 / 100.0).replace('.', ",")));
        }

        grand_total += cat_total;
        let avg_monthly = (cat_total as f64 / num_months) / 100.0;

        row_cells.push(Cell::from(format!("-{:.2} €", cat_total as f64 / 100.0).replace('.', ",")).style(Style::default().fg(Color::Red)));
        row_cells.push(Cell::from(format!("-{:.2} €", avg_monthly).replace('.', ",")).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));

        rows.push(Row::new(with_pinned_first(row_cells, app.scroll_x_tab2)));
    }

    let mut total_cells = vec![Cell::from("GESAMT").style(Style::default().add_modifier(Modifier::BOLD))];
    for y in &years {
        let val = year_totals.get(y).copied().unwrap_or(0);
        total_cells.push(Cell::from(format!("-{:.2}", val as f64 / 100.0).replace('.', ",")).style(Style::default().add_modifier(Modifier::BOLD)));
    }
    let total_avg_monthly = (grand_total as f64 / num_months) / 100.0;
    total_cells.push(Cell::from(format!("-{:.2} €", grand_total as f64 / 100.0).replace('.', ",")).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
    total_cells.push(Cell::from(format!("-{:.2} €", total_avg_monthly).replace('.', ",")).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    
    rows.push(Row::new(with_pinned_first(total_cells, app.scroll_x_tab2)).style(Style::default().bg(Color::Rgb(30, 30, 30))));

    let table = Table::new(rows, visible_constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Ausgaben pro Jahr & Mtl. Durchschnitt pro Kategorie | Vertikal: j/k, Horizontal: h/l, 1. Spalte fix "));

    f.render_widget(table, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(date: &str, payee: &str, category: &str, amount_cents: i64) -> Transaction {
        Transaction {
            buchung: date.to_string(),
            wertstellung: date.to_string(),
            payee: payee.to_string(),
            text: String::new(),
            purpose: String::new(),
            amount_cents,
            currency: "EUR".to_string(),
            date: NaiveDate::parse_from_str(date, "%d.%m.%Y").unwrap(),
            category: category.to_string(),
        }
    }

    fn test_app() -> App {
        App::new(
            Config::default(),
            vec![
                tx("05.01.2025", "REWE Markt", "Lebensmittel", 3210),
                tx("15.02.2025", "Shell Tankstelle", "Mobilität", 5000),
                tx("20.03.2025", "REWE Markt", "Lebensmittel", 1250),
            ],
        )
    }

    #[test]
    fn filter_buchungsliste_by_category() {
        let mut app = test_app();
        app.filter_category = Some("Lebensmittel".to_string());

        let filtered = app.filter_buchungsliste();

        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|t| t.category == "Lebensmittel"));
    }

    #[test]
    fn filter_buchungsliste_combines_search_amount_and_date() {
        let mut app = test_app();
        app.filter_search = "rewe".to_string();
        app.filter_min_amount = Some(20.0);
        app.filter_start_date = Some(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        app.filter_end_date = Some(NaiveDate::from_ymd_opt(2025, 2, 1).unwrap());

        let filtered = app.filter_buchungsliste();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].amount_cents, 3210);
    }

    #[test]
    fn select_filter_category_updates_list() {
        let mut app = test_app();

        app.filter_category_state.select(Some(0)); // "Alle Kategorien"
        app.select_filter_category();
        assert_eq!(app.filtered_transactions.len(), 3);

        app.filter_category_state.select(Some(1)); // "Lebensmittel"
        app.select_filter_category();
        assert_eq!(app.filter_category.as_deref(), Some("Lebensmittel"));
        assert_eq!(app.filtered_transactions.len(), 2);
    }

mod table_tests {
    use super::with_pinned_first;

    #[test]
    fn pinned_first_column_survives_horizontal_scroll() {
        let columns = vec!["Kategorie", "2024", "2025", "2026"];
        assert_eq!(with_pinned_first(columns.clone(), 0), columns);
        assert_eq!(
            with_pinned_first(columns.clone(), 1),
            vec!["Kategorie", "2025", "2026"]
        );
        assert_eq!(with_pinned_first(columns, 99), vec!["Kategorie"]);
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_config_enables_report_next_to_csvs() {
        let config = Config::default();
        assert_eq!(config.report_path, "report.html");
        assert!(!config.report_pdf);
    }

    #[test]
    fn config_roundtrip_keeps_report_settings() {
        let config = Config {
            csv_dir: "/data".to_string(),
            ignore_payees: vec!["Umbuchung".to_string()],
            categories: BTreeMap::from([(
                "Lebensmittel".to_string(),
                vec!["rewe".to_string()],
            )]),
            report_path: "report.html".to_string(),
            report_pdf: true,
        };

        let serialized = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&serialized).unwrap();

        assert_eq!(parsed.report_path, "report.html");
        assert!(parsed.report_pdf);
        assert_eq!(parsed.categories["Lebensmittel"], vec!["rewe"]);
        // TOML requires plain keys before tables, otherwise parsing breaks.
        assert!(serialized.find("report_path").unwrap() < serialized.find("[categories]").unwrap());
    }
}
