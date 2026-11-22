//! Generates a standalone, mobile-friendly HTML report (and optionally a PDF)
//! from the parsed transactions. The report is written next to the CSV files so
//! that a folder sync (e.g. Nextcloud) makes it available on a phone.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{Config, Transaction};

pub struct ReportOutcome {
    pub html: PathBuf,
    pub pdf: Option<PathBuf>,
    pub warning: Option<String>,
}

/// Writes the report(s) configured in `config` synchronously.
/// Returns `Ok(None)` when reporting is disabled (empty `report_path`).
pub fn generate(
    config: &Config,
    transactions: &[Transaction],
) -> Result<Option<ReportOutcome>, String> {
    let Some(html) = generate_html(config, transactions)? else {
        return Ok(None);
    };

    let mut pdf = None;
    let mut warning = None;
    if config.report_pdf {
        let pdf_path = html.with_extension("pdf");
        match write_pdf(&html, &pdf_path) {
            Ok(()) => pdf = Some(pdf_path),
            Err(e) => warning = Some(e),
        }
    }

    Ok(Some(ReportOutcome {
        html,
        pdf,
        warning,
    }))
}

/// Writes only the HTML report (fast, typically a few milliseconds).
pub fn generate_html(
    config: &Config,
    transactions: &[Transaction],
) -> Result<Option<PathBuf>, String> {
    let raw_path = config.report_path.trim();
    if raw_path.is_empty() {
        return Ok(None);
    }

    let html_path = resolve_path(&config.csv_dir, raw_path);
    if let Some(parent) = html_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }

    let html = render(transactions);
    write_atomic(&html_path, &html)
        .map_err(|e| format!("{}: {}", html_path.display(), e))?;

    Ok(Some(html_path))
}

fn resolve_path(csv_dir: &str, report_path: &str) -> PathBuf {
    let path = PathBuf::from(report_path);
    if path.is_absolute() {
        path
    } else {
        PathBuf::from(csv_dir).join(path)
    }
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

/// Renders the HTML report with headless Chromium. Uses an isolated profile so
/// it does not interfere with a running browser session.
pub fn write_pdf(html_path: &Path, pdf_path: &Path) -> Result<(), String> {
    let html_abs = fs::canonicalize(html_path).map_err(|e| e.to_string())?;
    let pdf_abs = if pdf_path.is_absolute() {
        pdf_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(pdf_path)
    };
    if let Some(parent) = pdf_abs.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let profile = std::env::temp_dir().join(format!("bank_tui_pdf_{}", std::process::id()));
    let _ = fs::create_dir_all(&profile);

    let mut last_error = String::from("kein Chromium gefunden (chromium/google-chrome)");
    for bin in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        let result = Command::new(bin)
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-sandbox")
            .arg("--no-first-run")
            .arg("--disable-extensions")
            .arg("--virtual-time-budget=5000")
            .arg("--no-pdf-header-footer")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--print-to-pdf={}", pdf_abs.display()))
            .arg(format!("file://{}", html_abs.display()))
            .output();

        match result {
            Ok(out) if out.status.success() && pdf_abs.exists() => {
                let _ = fs::remove_dir_all(&profile);
                return Ok(());
            }
            Ok(out) => {
                last_error = format!(
                    "{}: {}",
                    bin,
                    String::from_utf8_lossy(&out.stderr).trim().to_string()
                );
            }
            Err(e) => last_error = format!("{}: {}", bin, e),
        }
    }

    let _ = fs::remove_dir_all(&profile);
    Err(format!("PDF konnte nicht erzeugt werden ({})", last_error))
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Formats cents as German currency without the currency symbol, e.g. `1.234,56`.
fn amount(cents: i64) -> String {
    let negative = cents < 0;
    let abs = cents.unsigned_abs();
    let euros = abs / 100;
    let rest = abs % 100;
    let digits = euros.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    format!(
        "{}{},{:02} €",
        if negative { "-" } else { "" },
        grouped,
        rest
    )
}

fn percent(part: i64, total: i64) -> f64 {
    if total <= 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

struct Aggregates {
    total_cents: i64,
    count: usize,
    first_date: String,
    last_date: String,
    month_count: usize,
    categories: Vec<CategoryRow>,
    years: Vec<String>,
    year_totals: BTreeMap<String, i64>,
    monthly: Vec<MonthRow>,
}

struct CategoryRow {
    name: String,
    total: i64,
    per_year: BTreeMap<String, i64>,
}

struct MonthRow {
    key: String,
    per_category: BTreeMap<String, i64>,
    total: i64,
    count: usize,
}

fn aggregate(transactions: &[Transaction]) -> Aggregates {
    let mut total_cents = 0i64;
    let mut cat_year: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut month_cat: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut month_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut years: BTreeSet<String> = BTreeSet::new();
    let mut months: BTreeSet<String> = BTreeSet::new();
    let mut first: Option<chrono::NaiveDate> = None;
    let mut last: Option<chrono::NaiveDate> = None;

    for tx in transactions {
        total_cents += tx.amount_cents;
        let year = tx.date.format("%Y").to_string();
        let month = tx.date.format("%Y-%m").to_string();
        years.insert(year.clone());
        months.insert(month.clone());
        *cat_year
            .entry(tx.category.clone())
            .or_default()
            .entry(year)
            .or_insert(0) += tx.amount_cents;
        *month_cat
            .entry(month)
            .or_default()
            .entry(tx.category.clone())
            .or_insert(0) += tx.amount_cents;
        *month_counts.entry(tx.date.format("%Y-%m").to_string()).or_insert(0) += 1;

        if first.map_or(true, |d| tx.date < d) {
            first = Some(tx.date);
        }
        if last.map_or(true, |d| tx.date > d) {
            last = Some(tx.date);
        }
    }

    let mut categories: Vec<CategoryRow> = cat_year
        .into_iter()
        .map(|(name, per_year)| CategoryRow {
            total: per_year.values().sum(),
            name,
            per_year,
        })
        .collect();
    categories.sort_by(|a, b| b.total.cmp(&a.total));

    let mut year_totals = BTreeMap::new();
    for cat in &categories {
        for (year, sum) in &cat.per_year {
            *year_totals.entry(year.clone()).or_insert(0) += *sum;
        }
    }

    let monthly = month_cat
        .into_iter()
        .map(|(key, per_category)| MonthRow {
            total: per_category.values().sum(),
            count: month_counts.get(&key).copied().unwrap_or(0),
            key,
            per_category,
        })
        .collect();

    Aggregates {
        total_cents,
        count: transactions.len(),
        first_date: first
            .map(|d| d.format("%d.%m.%Y").to_string())
            .unwrap_or_default(),
        last_date: last
            .map(|d| d.format("%d.%m.%Y").to_string())
            .unwrap_or_default(),
        month_count: months.len(),
        categories,
        years: years.into_iter().collect(),
        year_totals,
        monthly,
    }
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

const STYLE: &str = r#"
:root{
  --bg:#f6f7f9; --card:#ffffff; --fg:#1c2128; --muted:#66707c;
  --line:#e3e7ed; --accent:#c0392b; --accent-soft:#f7e4e1; --chip:#eef1f5;
  --shadow:0 1px 2px rgba(16,24,40,.06),0 1px 3px rgba(16,24,40,.1);
  --radius:12px;
}
@media (prefers-color-scheme:dark){
  :root{ --bg:#12151a; --card:#191d24; --fg:#e6e9ee; --muted:#98a2b3;
    --line:#272c35; --accent:#ff7b6b; --accent-soft:#3a1f1c; --chip:#232833;
    --shadow:none; }
}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%;scroll-behavior:smooth}
body{margin:0;background:var(--bg);color:var(--fg);
  font:15px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  padding:0 max(12px,env(safe-area-inset-left)) calc(44px + env(safe-area-inset-bottom)) max(12px,env(safe-area-inset-right));
  -webkit-print-color-adjust:exact;print-color-adjust:exact}
header{padding:18px 0 6px}
h1{font-size:22px;margin:0}
h2{font-size:16px;margin:0 0 8px}
.sub{color:var(--muted);font-size:13px;margin:4px 0 0}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:8px;margin:14px 0 0}
.card{background:var(--card);border:1px solid var(--line);border-radius:var(--radius);padding:10px 12px;box-shadow:var(--shadow);min-width:0}
.card span{display:block;color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.03em}
.card strong{display:block;font-size:19px;margin-top:2px;font-variant-numeric:tabular-nums;overflow-wrap:anywhere}
.card small{display:block;color:var(--muted);margin-top:2px;overflow-wrap:anywhere}
nav{display:flex;gap:8px;margin:14px 0 0;overflow-x:auto;padding-bottom:2px;scrollbar-width:none}
nav::-webkit-scrollbar{display:none}
nav a{background:var(--chip);color:var(--fg);text-decoration:none;padding:7px 14px;border-radius:999px;font-size:13px;flex:0 0 auto}
section{margin:22px 0 0;scroll-margin-top:8px}
.card-block{background:var(--card);border:1px solid var(--line);border-radius:var(--radius);padding:12px;box-shadow:var(--shadow);overflow-x:auto}
.only-narrow{display:none}
table{border-collapse:collapse;width:100%;font-size:13.5px;font-variant-numeric:tabular-nums}
th,td{text-align:left;padding:7px 8px;border-bottom:1px solid var(--line);vertical-align:top;white-space:nowrap}
td.wrap,th.wrap{white-space:normal}
thead th{position:sticky;top:0;background:var(--card);color:var(--muted);font-weight:600;font-size:12px;z-index:1}
tbody tr:last-child td{border-bottom:none}
.num{text-align:right;font-variant-numeric:tabular-nums}
.spent{color:var(--accent);font-weight:600}
tfoot td{border-top:2px solid var(--line);font-weight:700}
.bar{position:relative;height:7px;border-radius:99px;background:var(--chip);min-width:70px;margin-top:6px;overflow:hidden}
.bar span{position:absolute;inset:0 auto 0 0;background:var(--accent);border-radius:99px;opacity:.75}
.matrix td:first-child,.matrix th:first-child{position:sticky;left:0;background:var(--card);z-index:2}
.cats td:first-child,.cats th:first-child{position:sticky;left:0;background:var(--card);z-index:2}
/* history chart */
.chart{display:flex;align-items:flex-end;gap:6px;height:150px;padding-top:6px}
.bar-item{flex:1 1 0;min-width:16px;height:100%;display:grid;grid-template-rows:1fr 16px;align-items:end}
.bar-item .col{height:var(--h);min-height:3px;width:100%;background:var(--accent);opacity:.7;border-radius:5px 5px 2px 2px;align-self:end}
.bar-item .lbl{font-size:9.5px;color:var(--muted);text-align:center;white-space:nowrap;padding-top:3px}
/* toolbar */
.toolbar{display:flex;flex-wrap:wrap;gap:8px;align-items:center;margin-bottom:10px}
.toolbar input[type=search],.toolbar select{font:inherit;font-size:14px;padding:10px;line-height:1.2;border-radius:10px;border:1px solid var(--line);background:var(--bg);color:var(--fg);min-width:0;flex:1 1 150px}
.chips{display:flex;flex-wrap:wrap;gap:6px;width:100%}
.chip-btn{font:inherit;font-size:13px;line-height:1.2;padding:7px 12px;border-radius:999px;border:1px solid var(--line);background:var(--bg);color:var(--fg);cursor:pointer;white-space:nowrap;-webkit-tap-highlight-color:transparent}
.chip-btn .n{color:var(--muted);font-size:11.5px;margin-left:2px}
.chip-btn[aria-pressed=true]{background:var(--accent-soft);border-color:var(--accent);color:var(--accent);font-weight:600}
.chip-btn[aria-pressed=true] .n{color:var(--accent)}
.hint{color:var(--muted);font-size:12.5px;width:100%;margin:0}
.payee{font-weight:600;white-space:normal}
.meta{color:var(--muted);font-size:12.5px;white-space:normal}
.chip{display:inline-block;background:var(--chip);color:var(--fg);border-radius:999px;padding:1px 8px;font-size:12px;white-space:nowrap}
.chip.hot{background:var(--accent-soft);color:var(--accent)}
#tx tbody tr[hidden]{display:none}
footer{margin-top:26px;color:var(--muted);font-size:12px}
/* narrow-screen card views */
.cat-cards{display:flex;flex-direction:column}
.cat-card{padding:11px 2px;border-bottom:1px solid var(--line)}
.cat-card:last-child{border-bottom:none}
.cat-head{display:flex;align-items:baseline;justify-content:space-between;gap:12px}
.cat-name{font-weight:600;min-width:0;overflow-wrap:anywhere}
.cat-sum{white-space:nowrap;font-variant-numeric:tabular-nums}
.cat-meta{color:var(--muted);font-size:12px;margin-top:5px}
.cat-card .bar{min-width:0;margin-top:7px}
.cat-card.total{padding-top:12px;border-top:2px solid var(--line)}
.cat-card.total .cat-name{text-transform:uppercase;font-size:11.5px;letter-spacing:.04em;color:var(--muted)}
.month-acc{display:flex;flex-direction:column;gap:6px}
.month{border:1px solid var(--line);border-radius:10px;overflow:hidden}
.month summary{display:flex;align-items:baseline;justify-content:space-between;gap:10px;padding:11px 12px;cursor:pointer;list-style:none;background:var(--card);-webkit-tap-highlight-color:transparent}
.month summary::-webkit-details-marker{display:none}
.month summary .m-label{font-weight:600;font-variant-numeric:tabular-nums}
.month summary .m-label small{display:block;color:var(--muted);font-weight:400;font-size:11.5px}
.month summary .m-sum{white-space:nowrap;font-variant-numeric:tabular-nums}
.month summary::after{content:"▾";color:var(--muted);font-size:11px;flex:0 0 auto}
.month[open] summary::after{transform:rotate(180deg)}
.m-body{padding:2px 12px 10px;border-top:1px solid var(--line)}
.m-row{position:relative;display:flex;justify-content:space-between;align-items:baseline;gap:10px;padding:6px;border-radius:6px;overflow:hidden;font-size:13.5px}
.m-row + .m-row{margin-top:2px}
.m-fill{position:absolute;inset:0 auto 0 0;background:var(--accent-soft);border-radius:6px}
.m-cat,.m-amt{position:relative;min-width:0}
.m-cat{overflow-wrap:anywhere}
.m-amt{white-space:nowrap;font-variant-numeric:tabular-nums}
.m-pct{color:var(--muted);font-size:11.5px;margin-left:5px}
.totop{position:fixed;right:max(14px,env(safe-area-inset-right));bottom:calc(16px + env(safe-area-inset-bottom));width:44px;height:44px;display:grid;place-items:center;border-radius:50%;background:var(--card);border:1px solid var(--line);color:var(--fg);text-decoration:none;box-shadow:0 2px 8px rgba(0,0,0,.15);opacity:0;pointer-events:none;transition:opacity .2s;z-index:20;font-size:18px}
.totop.show{opacity:1;pointer-events:auto}
@media (max-width:720px){
  .only-wide{display:none}
  .only-narrow{display:block}
  header{padding-top:14px}
  h1{font-size:20px}
  h2{font-size:15px;margin-bottom:7px}
  section{margin-top:18px}
  .cards{grid-template-columns:1fr 1fr;gap:7px;margin-top:12px}
  .card{padding:9px 11px}
  .card strong{font-size:18px}
  .card.top{grid-column:1/-1}
  .card-block{padding:10px 12px}
  #liste .card-block{overflow:visible}
  #liste .toolbar{position:sticky;top:0;z-index:5;background:var(--card);margin:-10px -12px 8px;padding:8px 12px;border-bottom:1px solid var(--line)}
  .toolbar input[type=search],.toolbar select{font-size:16px}
  .toolbar input[type=search]{flex:1 1 150px}
  .toolbar select{flex:0 0 auto;max-width:46%}
  .chips{flex-wrap:nowrap;overflow-x:auto;scrollbar-width:none;margin:0 -12px;padding:0 12px 2px;width:calc(100% + 24px)}
  .chips::-webkit-scrollbar{display:none}
  .chip-btn{padding:8px 13px;font-size:13.5px}
  .hint{font-size:12px}
  #tx thead{display:none}
  #tx,#tx tbody,#tx tr,#tx td{display:block;width:auto;white-space:normal}
  #tx tr{border-bottom:1px solid var(--line);padding:11px 0;display:grid;
    grid-template-columns:minmax(0,1fr) auto;align-items:baseline;
    grid-template-areas:"date amount" "payee cat" "detail detail";
    grid-gap:2px 10px;cursor:pointer}
  #tx tr:last-child{border-bottom:none}
  #tx td{border:none;padding:0}
  #tx td.c-date{grid-area:date;color:var(--muted);font-size:12px}
  #tx td.c-amount{grid-area:amount;text-align:right;font-size:15.5px}
  #tx td.c-cat{grid-area:cat;text-align:right;justify-self:end}
  #tx td.c-payee{grid-area:payee;min-width:0;font-size:15px;line-height:1.3}
  #tx td.c-detail{grid-area:detail;margin-top:3px;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
  #tx tr.expanded td.c-detail{-webkit-line-clamp:unset;display:block}
  #tx tr.expanded{background:var(--chip)}
  .chart{height:132px;gap:5px}
  .bar-item .lbl{font-size:9px}
}
@media print{
  @page{size:A4 landscape;margin:10mm}
  body{background:#fff;color:#000;padding:0;font-size:11px}
  .cards{grid-template-columns:repeat(3,1fr)}
  nav,.toolbar,footer,.totop,.only-narrow{display:none!important}
  .only-wide{display:block!important}
  .card-block,section{box-shadow:none;break-inside:auto}
  section{break-inside:avoid}
  table{font-size:10px}
  tr[hidden]{display:table-row!important}
  thead th{position:static}
  .matrix th,.matrix td{padding:4px 5px}
  h1,h2{break-after:avoid}
  .chart{height:120px}
}
"#;

fn render(transactions: &[Transaction]) -> String {
    let agg = aggregate(transactions);
    let generated = chrono::Local::now().format("%d.%m.%Y %H:%M").to_string();
    let avg_month = if agg.month_count > 0 {
        agg.total_cents / agg.month_count as i64
    } else {
        0
    };

    let mut html = String::with_capacity(96 * 1024);
    html.push_str("<!doctype html>\n<html lang=\"de\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    html.push_str("<meta name=\"theme-color\" content=\"#f6f7f9\" media=\"(prefers-color-scheme: light)\">\n");
    html.push_str("<meta name=\"theme-color\" content=\"#12151a\" media=\"(prefers-color-scheme: dark)\">\n");
    let _ = write!(html, "<title>Ausgaben – Stand {}</title>\n", escape(&generated));
    html.push_str("<style>");
    html.push_str(STYLE);
    html.push_str("</style>\n</head>\n<body id=\"top\">\n");

    html.push_str("<header>\n<h1>Ausgaben</h1>\n");
    let _ = write!(
        html,
        "<p class=\"sub\">Stand {} · {} · {} Buchungen</p>\n",
        escape(&generated),
        if agg.count == 0 {
            "keine Daten".to_string()
        } else {
            format!("{} – {}", escape(&agg.first_date), escape(&agg.last_date))
        },
        agg.count
    );

    html.push_str("<div class=\"cards\">\n");
    let _ = write!(
        html,
        "<div class=\"card\"><span>Gesamt</span><strong class=\"spent\">{}</strong></div>\n",
        amount(agg.total_cents)
    );
    let _ = write!(
        html,
        "<div class=\"card\"><span>Ø pro Monat</span><strong class=\"spent\">{}</strong><small>{} Monate</small></div>\n",
        amount(avg_month),
        agg.month_count
    );
    match agg
        .categories
        .iter()
        .find(|c| c.name != "Unkategorisiert")
        .or_else(|| agg.categories.first())
    {
        Some(top) => {
            let _ = write!(
                html,
                "<div class=\"card top\"><span>Top-Kategorie</span><strong>{}</strong><small>{} · {:.0}&nbsp;%</small>",
                escape(&top.name),
                amount(top.total),
                percent(top.total, agg.total_cents)
            );
            if top.name != "Unkategorisiert" {
                if let Some(uncat) = agg.categories.iter().find(|c| c.name == "Unkategorisiert") {
                    let _ = write!(
                        html,
                        "<small>Unkategorisiert: {} ({:.0}&nbsp;%)</small>",
                        amount(uncat.total),
                        percent(uncat.total, agg.total_cents)
                    );
                }
            }
            html.push_str("</div>\n");
        }
        None => html.push_str(
            "<div class=\"card top\"><span>Top-Kategorie</span><strong>–</strong></div>\n",
        ),
    }
    html.push_str("</div>\n");
    html.push_str("</header>\n");

    html.push_str("<nav><a href=\"#verlauf\">Verlauf</a><a href=\"#kategorien\">Kategorien</a><a href=\"#monate\">Monate</a><a href=\"#liste\">Buchungen</a></nav>\n");

    render_history_chart(&mut html, &agg);
    render_category_table(&mut html, &agg);
    render_month_table(&mut html, &agg);
    render_transaction_list(&mut html, transactions, &agg);

    let _ = write!(
        html,
        "<footer>bank_tui · automatisch erzeugt am {}</footer>\n",
        escape(&generated)
    );
    html.push_str("<a href=\"#top\" class=\"totop\" id=\"totop\" aria-label=\"Nach oben\">↑</a>\n");
    html.push_str("<script>");
    html.push_str(SCRIPT);
    html.push_str("</script>\n</body>\n</html>\n");
    html
}

fn month_label(key: &str) -> String {
    match (key.get(0..4), key.get(5..7)) {
        (Some(year), Some(month)) => format!("{}/{}", month, year),
        _ => key.to_string(),
    }
}

fn render_history_chart(html: &mut String, agg: &Aggregates) {
    if agg.monthly.is_empty() {
        return;
    }

    let window: Vec<&MonthRow> = agg.monthly.iter().rev().take(12).collect();
    let window: Vec<&MonthRow> = window.into_iter().rev().collect();
    let max = window.iter().map(|m| m.total).max().unwrap_or(0);
    let peak = window.iter().max_by_key(|m| m.total).copied();

    let range = if window.len() > 1 {
        format!(
            "{} – {}",
            month_label(&window[0].key),
            month_label(&window[window.len() - 1].key)
        )
    } else {
        month_label(&window[0].key)
    };
    let peak_text = peak
        .map(|m| format!(" · Höchster Monat: {} ({})", month_label(&m.key), amount(m.total)))
        .unwrap_or_default();

    html.push_str("<section id=\"verlauf\"><h2>Verlauf</h2><div class=\"card-block\">\n");
    let _ = write!(
        html,
        "<p class=\"hint\">Letzte {} Monate · {}{}</p>\n",
        window.len(),
        range,
        peak_text
    );
    html.push_str("<div class=\"chart\" role=\"img\" aria-label=\"Monatlicher Ausgabenverlauf\">\n");
    for month in &window {
        let height = if max > 0 {
            month.total as f64 / max as f64 * 100.0
        } else {
            0.0
        };
        let _ = write!(
            html,
            "<div class=\"bar-item\" title=\"{} · {}\"><div class=\"col\" style=\"--h:{:.1}%\"></div><span class=\"lbl\">{}</span></div>\n",
            month_label(&month.key),
            amount(month.total),
            height,
            month.key.get(5..7).unwrap_or("")
        );
    }
    html.push_str("</div>\n</div></section>\n");
}

fn render_category_table(html: &mut String, agg: &Aggregates) {
    html.push_str("<section id=\"kategorien\"><h2>Kategorien</h2><div class=\"card-block only-wide\"><table class=\"cats\">\n<thead><tr><th>Kategorie</th>");
    for year in &agg.years {
        let _ = write!(html, "<th class=\"num\">{}</th>", escape(year));
    }
    html.push_str("<th class=\"num\">Gesamt</th><th class=\"num\">Ø/Monat</th><th>Anteil</th></tr></thead>\n<tbody>\n");

    for cat in &agg.categories {
        let _ = write!(
            html,
            "<tr><td class=\"wrap\">{}</td>",
            escape(&cat.name)
        );
        for year in &agg.years {
            let v = cat.per_year.get(year).copied().unwrap_or(0);
            let _ = write!(html, "<td class=\"num\">{}</td>", amount(v));
        }
        let avg = if agg.month_count > 0 {
            cat.total / agg.month_count as i64
        } else {
            0
        };
        let share = percent(cat.total, agg.total_cents);
        let _ = write!(
            html,
            "<td class=\"num spent\">{}</td><td class=\"num\">{}</td><td><div class=\"bar\" title=\"{:.1} %\"><span style=\"width:{:.1}%\"></span></div></td></tr>\n",
            amount(cat.total),
            amount(avg),
            share,
            share
        );
    }

    html.push_str("</tbody>\n<tfoot><tr><td>Gesamt</td>");
    for year in &agg.years {
        let _ = write!(
            html,
            "<td class=\"num\">{}</td>",
            amount(agg.year_totals.get(year).copied().unwrap_or(0))
        );
    }
    let _ = write!(
        html,
        "<td class=\"num\">{}</td><td class=\"num\">{}</td><td></td></tr></tfoot>\n</table></div>\n",
        amount(agg.total_cents),
        amount(if agg.month_count > 0 {
            agg.total_cents / agg.month_count as i64
        } else {
            0
        })
    );
    render_category_cards(html, agg);
    html.push_str("</section>\n");
}

fn render_category_cards(html: &mut String, agg: &Aggregates) {
    html.push_str("<div class=\"card-block only-narrow\"><div class=\"cat-cards\">\n");
    for cat in &agg.categories {
        let share = percent(cat.total, agg.total_cents);
        let avg = if agg.month_count > 0 {
            cat.total / agg.month_count as i64
        } else {
            0
        };
        let mut meta = format!("{:.0} % · Ø {} /Monat", share, amount(avg));
        for year in &agg.years {
            if let Some(value) = cat.per_year.get(year) {
                if *value != 0 {
                    let _ = write!(meta, " · {}: {}", escape(year), amount(*value));
                }
            }
        }
        let _ = write!(
            html,
            "<div class=\"cat-card\"><div class=\"cat-head\"><span class=\"cat-name\">{}</span><span class=\"cat-sum spent\">{}</span></div>\
<div class=\"bar\"><span style=\"width:{:.1}%\"></span></div>\
<div class=\"cat-meta\">{}</div></div>\n",
            escape(&cat.name),
            amount(cat.total),
            share,
            escape(&meta)
        );
    }
    let _ = write!(
        html,
        "<div class=\"cat-card total\"><div class=\"cat-head\"><span class=\"cat-name\">Gesamt</span><span class=\"cat-sum spent\">{}</span></div>\
<div class=\"cat-meta\">{} Kategorien · {} Buchungen</div></div>\n",
        amount(agg.total_cents),
        agg.categories.len(),
        agg.count
    );
    html.push_str("</div></div>\n");
}

fn render_month_table(html: &mut String, agg: &Aggregates) {
    if agg.monthly.is_empty() {
        return;
    }

    // Newest month first; categories ordered by total so the important columns
    // stay on the left on a phone.
    let cat_names: Vec<&str> = agg.categories.iter().map(|c| c.name.as_str()).collect();

    html.push_str("<section id=\"monate\"><h2>Monate &amp; Kategorien</h2><div class=\"card-block only-wide\"><table class=\"matrix\">\n<thead><tr><th>Monat</th>");
    for name in &cat_names {
        let _ = write!(html, "<th class=\"num wrap\">{}</th>", escape(name));
    }
    html.push_str("<th class=\"num\">Summe</th></tr></thead>\n<tbody>\n");

    for row in agg.monthly.iter().rev() {
        let _ = write!(html, "<tr><td>{}</td>", escape(&row.key));
        for name in &cat_names {
            let v = row.per_category.get(*name).copied().unwrap_or(0);
            if v == 0 {
                html.push_str("<td class=\"num\" style=\"color:var(--muted)\">·</td>");
            } else {
                let _ = write!(html, "<td class=\"num\">{}</td>", amount(v));
            }
        }
        let _ = write!(
            html,
            "<td class=\"num spent\">{}</td></tr>\n",
            amount(row.total)
        );
    }

    html.push_str("</tbody></table></div>\n");
    render_month_cards(html, agg);
    html.push_str("</section>\n");
}

fn render_month_cards(html: &mut String, agg: &Aggregates) {
    if agg.monthly.is_empty() {
        return;
    }

    html.push_str("<div class=\"card-block only-narrow\"><div class=\"month-acc\">\n");
    for (index, row) in agg.monthly.iter().rev().enumerate() {
        let open = if index == 0 { " open" } else { "" };
        let _ = write!(
            html,
            "<details class=\"month\"{open}><summary><span class=\"m-label\">{}<small>{} Buchungen</small></span><span class=\"m-sum spent\">{}</span></summary><div class=\"m-body\">\n",
            month_label(&row.key),
            row.count,
            amount(row.total)
        );

        let mut entries: Vec<(&str, i64)> = row
            .per_category
            .iter()
            .filter(|(_, value)| **value != 0)
            .map(|(name, value)| (name.as_str(), *value))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        let max = entries.first().map(|e| e.1).unwrap_or(1).max(1);

        for (name, value) in entries {
            let width = value as f64 / max as f64 * 100.0;
            let _ = write!(
                html,
                "<div class=\"m-row\"><span class=\"m-fill\" style=\"width:{:.0}%\"></span><span class=\"m-cat\">{}</span><span class=\"m-amt\">{}<span class=\"m-pct\">{:.0}%</span></span></div>\n",
                width,
                escape(name),
                amount(value),
                percent(value, row.total)
            );
        }

        html.push_str("</div></details>\n");
    }
    html.push_str("</div></div>\n");
}

fn render_transaction_list(html: &mut String, transactions: &[Transaction], agg: &Aggregates) {
    html.push_str("<section id=\"liste\"><h2>Buchungen</h2><div class=\"card-block\">\n");
    html.push_str("<div class=\"toolbar\">");
    html.push_str("<input id=\"q\" type=\"search\" inputmode=\"search\" enterkeyhint=\"search\" autocomplete=\"off\" placeholder=\"Suchen…\" aria-label=\"Suchen (Empfänger, Zweck, Kategorie)\">");
    html.push_str("<select id=\"sort\" aria-label=\"Sortierung\">");
    html.push_str("<option value=\"date-desc\">Neueste</option>");
    html.push_str("<option value=\"date-asc\">Älteste</option>");
    html.push_str("<option value=\"amount-desc\">Betrag ↓</option>");
    html.push_str("<option value=\"amount-asc\">Betrag ↑</option>");
    html.push_str("</select>");
    html.push_str("<div class=\"chips\" role=\"group\" aria-label=\"Kategorie filtern\">");
    let _ = write!(
        html,
        "<button type=\"button\" class=\"chip-btn\" data-cat=\"\" aria-pressed=\"true\">Alle<span class=\"n\">{}</span></button>",
        transactions.len()
    );
    for cat in &agg.categories {
        let count = transactions
            .iter()
            .filter(|tx| tx.category == cat.name)
            .count();
        let _ = write!(
            html,
            "<button type=\"button\" class=\"chip-btn\" data-cat=\"{}\" aria-pressed=\"false\">{}<span class=\"n\">{}</span></button>",
            escape(&cat.name),
            escape(&cat.name),
            count
        );
    }
    html.push_str("</div>");
    let _ = write!(
        html,
        "<span class=\"hint\" id=\"count\">{} Buchungen · Summe <strong class=\"spent\" id=\"sum\">{}</strong></span>",
        transactions.len(),
        amount(agg.total_cents)
    );
    html.push_str("</div>\n");

    html.push_str("<table id=\"tx\">\n<thead><tr><th>Datum</th><th>Empfänger / Auftraggeber</th><th>Kategorie</th><th class=\"num\">Betrag</th></tr></thead>\n<tbody>\n");

    for tx in transactions {
        let search = format!(
            "{} {} {} {}",
            tx.payee, tx.text, tx.purpose, tx.category
        )
        .to_lowercase();
        let detail = match (tx.text.trim(), tx.purpose.trim()) {
            ("", "") => String::new(),
            (text, "") => text.to_string(),
            ("", purpose) => purpose.to_string(),
            (text, purpose) => format!("{} · {}", text, purpose),
        };

        let _ = write!(
            html,
            "<tr data-c=\"{cat}\" data-s=\"{search}\" data-date=\"{date}\" data-cents=\"{cents}\">\
<td class=\"c-date\">{date_de}</td>\
<td class=\"c-payee payee\">{payee}</td>\
<td class=\"c-cat\"><span class=\"chip{junk}\">{cat_raw}</span></td>\
<td class=\"c-amount num spent\">-{amt}</td>\
<td class=\"c-detail meta wrap\" title=\"{detail}\">{detail}</td></tr>\n",
            cat = escape(&tx.category),
            search = escape(&search),
            date = tx.date.format("%Y-%m-%d"),
            cents = tx.amount_cents,
            date_de = tx.date.format("%d.%m.%Y"),
            payee = escape(if tx.payee.trim().is_empty() {
                tx.text.trim()
            } else {
                tx.payee.trim()
            }),
            junk = if tx.category == "Unkategorisiert" {
                " hot"
            } else {
                ""
            },
            cat_raw = escape(&tx.category),
            amt = amount(tx.amount_cents),
            detail = escape(&detail),
        );
    }

    html.push_str("</tbody>\n</table>\n</div></section>\n");
}

const SCRIPT: &str = r#"
(function(){
  var q=document.getElementById('q'),
      sort=document.getElementById('sort'),
      count=document.getElementById('count'),
      sum=document.getElementById('sum'),
      table=document.getElementById('tx');
  if(!table) return;
  var body=table.tBodies[0],
      rows=Array.prototype.slice.call(body.rows),
      total=rows.length,
      activeCat='',
      chips=Array.prototype.slice.call(document.querySelectorAll('.chip-btn')),
      fmt=new Intl.NumberFormat('de-DE',{minimumFractionDigits:2,maximumFractionDigits:2});

  function apply(){
    var term=(q.value||'').trim().toLowerCase(),
        shown=0,total_cents=0;
    rows.forEach(function(r){
      var ok=(!term||r.dataset.s.indexOf(term)!==-1)&&(!activeCat||r.dataset.c===activeCat);
      r.hidden=!ok;
      if(ok){shown++;total_cents+=parseInt(r.dataset.cents||'0',10);}
    });
    count.textContent=shown+(shown===total?'':' von '+total)+' Buchungen · Summe ';
    count.appendChild(sum);
    sum.textContent='-'+fmt.format(total_cents/100)+' €';
  }

  function doSort(){
    var mode=sort.value.split('-'),
        key=mode[0],
        dir=mode[1]==='asc'?1:-1;
    rows.sort(function(a,b){
      var x,y;
      if(key==='date'){x=a.dataset.date;y=b.dataset.date;}
      else{x=parseInt(a.dataset.cents,10);y=parseInt(b.dataset.cents,10);}
      return x<y?-1*dir:x>y?1*dir:0;
    });
    rows.forEach(function(r){body.appendChild(r);});
  }

  q.addEventListener('input',apply);
  sort.addEventListener('change',doSort);

  chips.forEach(function(button){
    button.addEventListener('click',function(){
      activeCat=button.dataset.cat||'';
      chips.forEach(function(other){other.setAttribute('aria-pressed',other===button?'true':'false');});
      apply();
    });
  });

  rows.forEach(function(row){
    row.addEventListener('click',function(){row.classList.toggle('expanded');});
  });

  var top=document.getElementById('totop');
  if(top){
    var updateTop=function(){top.classList.toggle('show',window.scrollY>700);};
    window.addEventListener('scroll',updateTop,{passive:true});
    updateTop();
  }

  apply();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_german_currency() {
        assert_eq!(amount(0), "0,00 €");
        assert_eq!(amount(5), "0,05 €");
        assert_eq!(amount(123_456), "1.234,56 €");
        assert_eq!(amount(1_000_000_00), "1.000.000,00 €");
        assert_eq!(amount(-12_345), "-123,45 €");
    }

    #[test]
    fn escapes_html() {
        assert_eq!(
            escape("<b>Tom & \"Jerry\"</b>"),
            "&lt;b&gt;Tom &amp; &quot;Jerry&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn aggregates_category_totals() {
        let tx = |date: &str, cents: i64, cat: &str| Transaction {
            buchung: date.to_string(),
            wertstellung: String::new(),
            payee: String::new(),
            text: String::new(),
            purpose: String::new(),
            amount_cents: cents,
            currency: "EUR".to_string(),
            date: chrono::NaiveDate::parse_from_str(date, "%d.%m.%Y").unwrap(),
            category: cat.to_string(),
        };
        let transactions = vec![
            tx("01.01.2026", 100, "A"),
            tx("02.02.2026", 200, "A"),
            tx("03.03.2026", 400, "B"),
            tx("04.03.2025", 50, "B"),
        ];
        let agg = aggregate(&transactions);
        assert_eq!(agg.total_cents, 750);
        assert_eq!(agg.count, 4);
        assert_eq!(agg.month_count, 4);
        assert_eq!(agg.categories[0].name, "B");
        assert_eq!(agg.categories[0].total, 450);
        assert_eq!(agg.year_totals["2026"], 700);
        assert_eq!(agg.years, vec!["2025", "2026"]);
    }
}
