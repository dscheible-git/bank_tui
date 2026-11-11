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

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
struct Config {
    #[serde(default = "default_csv_dir")]
    csv_dir: String,
    #[serde(default)]
    ignore_payees: Vec<String>,
    #[serde(default)]
    categories: BTreeMap<String, Vec<String>>,
}

fn default_csv_dir() -> String {
    ".".to_string()
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

    // Interaktives Mapping
    mapping_source: Option<MappingSource>,
    mapping_category: String,
    mapping_keyword: String,
    mapping_category_state: ListState,
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
            mapping_source: None,
            mapping_category: String::new(),
            mapping_keyword: String::new(),
            mapping_category_state: ListState::default(),
        };
        if !app.filtered_transactions.is_empty() {
            app.table_state.select(Some(0));
        }
        app
    }

    fn apply_filters(&mut self) {
        let search_lower = self.filter_search.to_lowercase();
        self.filtered_transactions = self
            .all_transactions
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
            .collect();

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();
    let transactions = load_transactions_from_folder(&config.csv_dir, &config);

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config, transactions);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

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
                        KeyCode::Char('c') => {
                            app.filter_search.clear();
                            app.filter_min_amount = None;
                            app.filter_max_amount = None;
                            app.filter_start_date = None;
                            app.filter_end_date = None;
                            app.apply_filters();
                        }
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

    let filter_text = match app.input_mode {
        InputMode::SearchText => format!("SUCHE TEXT > {}_", app.input_buffer),
        InputMode::FilterMinAmount => format!("FILTER MIN BETRAG > {}_", app.input_buffer),
        InputMode::FilterMaxAmount => format!("FILTER MAX BETRAG > {}_", app.input_buffer),
        InputMode::FilterStartDate => format!("FILTER START DATUM (DD.MM.YYYY) > {}_", app.input_buffer),
        InputMode::FilterEndDate => format!("FILTER END DATUM (DD.MM.YYYY) > {}_", app.input_buffer),
        InputMode::MappingSourceSelect => "[MAPPING] Quelle wählen: [a] Auftraggeber/Empfänger | [v] Verwendungszweck | [Esc] Abbrechen".to_string(),
        InputMode::MappingCategorySelect => "[MAPPING] Kategorie wählen: j/k oder ↑/↓, Enter | [n] Neue Kategorie | [Esc] Abbrechen".to_string(),
        InputMode::MappingCategoryName => format!("[MAPPING] Neue Kategorie eingeben > {}_", app.input_buffer),
        InputMode::MappingKeyword => format!(
            "[MAPPING] Schlüsselwort für \"{}\" bestätigen/anpassen > {}_",
            app.mapping_category, app.input_buffer
        ),
        InputMode::Normal => format!(
            " [/]Suche: \"{}\" | [i]Min: {} | [a]Max: {} | [s]Start: {} | [e]Ende: {} | [c]Reset | [m]Kategorie Mappen",
            app.filter_search, min_str, max_str, start_str, end_str
        ),
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

    let visible_headers = slice_cells(header_cells, app.scroll_x_tab1);
    let visible_constraints = slice_constraints(&all_constraints, app.scroll_x_tab1);

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
        rows.push(Row::new(slice_cells(row_cells, app.scroll_x_tab1)));
    }

    let table = Table::new(rows, visible_constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Ausgaben pro Monat & Kategorie | Vertikal: j/k, Horizontal: h/l "));

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

    let visible_headers = slice_cells(header_cells, app.scroll_x_tab2);
    let visible_constraints = slice_constraints(&all_constraints, app.scroll_x_tab2);

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

        rows.push(Row::new(slice_cells(row_cells, app.scroll_x_tab2)));
    }

    let mut total_cells = vec![Cell::from("GESAMT").style(Style::default().add_modifier(Modifier::BOLD))];
    for y in &years {
        let val = year_totals.get(y).copied().unwrap_or(0);
        total_cells.push(Cell::from(format!("-{:.2}", val as f64 / 100.0).replace('.', ",")).style(Style::default().add_modifier(Modifier::BOLD)));
    }
    let total_avg_monthly = (grand_total as f64 / num_months) / 100.0;
    total_cells.push(Cell::from(format!("-{:.2} €", grand_total as f64 / 100.0).replace('.', ",")).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
    total_cells.push(Cell::from(format!("-{:.2} €", total_avg_monthly).replace('.', ",")).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    
    rows.push(Row::new(slice_cells(total_cells, app.scroll_x_tab2)).style(Style::default().bg(Color::Rgb(30, 30, 30))));

    let table = Table::new(rows, visible_constraints)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Ausgaben pro Jahr & Mtl. Durchschnitt pro Kategorie | Vertikal: j/k, Horizontal: h/l "));

    f.render_widget(table, area);
}
