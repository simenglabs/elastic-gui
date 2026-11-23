//! Discover: jelajah dokumen dengan waktu, query, filter, histogram, dan tabel.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Bar, BarChart, HoverPosition, Plot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Ctx, Nav};
use crate::es::Conn;
use crate::es::catalog::Field;
use crate::es::query::{self, Filter, QueryMode, TimeBounds};
use crate::i18n::{t, tf};
use crate::task::{Pending, poll};
use crate::widgets::{self, filters, time::TimeRange};

const PAGE: usize = 100;
const TOTAL_LIMIT: u64 = 10_000;
const PIT_KEEP_ALIVE: &str = "5m";
const EXPORT_MAX: usize = 100_000;
const EXPORT_PAGE: usize = 1_000;

struct Export {
    rows: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    pending: Option<Pending<Result<(usize, String), String>>>,
}

struct Request {
    pattern: String,
    body: Value,
    pit: Option<String>,
    append: bool,
    /// DIS-54: pakai `_async_search` (rentang waktu lebar).
    asynchronous: bool,
    cancel: Arc<AtomicBool>,
}

/// Rentang di atas ini dijalankan sebagai async search.
const ASYNC_SPAN_MS: i64 = 30 * 86_400_000;

struct Page {
    hits: Vec<Value>,
    total: Option<(u64, bool)>,
    buckets: Vec<(f64, f64)>,
    /// DIS-23: seri breakdown (nilai field, titik [x, jumlah]).
    series: Vec<(String, Vec<(f64, f64)>)>,
    pit: Option<String>,
    append: bool,
    failed_shards: u64,
    took: u64,
}

#[derive(Default)]
pub struct Discover {
    pattern: String,
    pattern_input: String,
    pattern_open: bool,
    time_field: Option<String>,
    time_field_chosen: bool,
    range: TimeRange,
    refresh_secs: u64,
    last_search: Option<Instant>,
    mode: QueryMode,
    query: String,
    filters: Vec<Filter>,
    filter_draft: filters::Draft,
    columns: Vec<String>,
    /// (field, descending)
    sort: Option<(String, bool)>,
    hits: Vec<Value>,
    total: Option<(u64, bool)>,
    buckets: Vec<(f64, f64)>,
    interval: (i64, &'static str),
    took: u64,
    pit: Option<String>,
    pending: Option<Pending<Result<Page, String>>>,
    error: Option<String>,
    warning: Option<String>,
    needs_search: bool,
    last_body: Option<Value>,
    selected: Option<usize>,
    detail_json: bool,
    field_search: String,
    field_open: Option<String>,
    drag_from: Option<f64>,
    /// DIS-34: `track_total_hits: true` (lebih mahal).
    exact_total: bool,
    /// DIS-01: preview index yang cocok dengan pola yang sedang diketik.
    preview: Option<Keyed<Vec<String>>>,
    preview_pending: Option<Pending<Keyed<Vec<String>>>>,
    preview_typed: Option<(String, Instant)>,
    export: Option<Export>,
    export_result: Option<Result<String, String>>,
    /// DIS-12: mode ES|QL (`POST /_query`), hasil berupa kolom + baris.
    esql: bool,
    esql_result: Option<EsqlTable>,
    esql_pending: Option<Pending<Result<EsqlTable, String>>>,
    loaded: bool,
    saved: Vec<SavedSearch>,
    save_name: String,
    history: Vec<String>,
    /// DIS-43: top values dari aggregation di seluruh rentang.
    top_request: Option<(String, bool)>,
    top_accurate: Option<TopValues>,
    top_pending: Option<Pending<TopValues>>,
    /// DIS-38: dokumen sekitar (id anchor, lebih baru, lebih lama).
    around: Option<Around>,
    around_pending: Option<Pending<Around>>,
    /// DIS-14: saran query bar.
    suggest: Vec<(String, String)>,
    suggest_selected: usize,
    /// Token yang sedang dilengkapi: (awal byte, akhir byte).
    suggest_span: (usize, usize),
    terms_cache: HashMap<(String, String), Vec<String>>,
    terms_pending: Option<Pending<TermsEnum>>,
    /// DIS-44: daftar field dikelompokkan per objek.
    field_tree: bool,
    conn: Option<Arc<Conn>>,
    views: HashMap<String, DataView>,
    breakdown: Option<String>,
    series: Vec<(String, Vec<(f64, f64)>)>,
    search_cancel: Arc<AtomicBool>,
    searching_async: bool,
    /// DIS-39: edit dokumen (indeks baris, teks JSON).
    doc_edit: Option<(usize, String)>,
    doc_delete: Option<usize>,
    doc_job: Option<Pending<DocJob>>,
    read_only: bool,
    prod: bool,
}

/// Hasil edit/hapus dokumen: (baris, Ok(Some(source baru, seq_no, primary_term)) / Ok(None)=dihapus).
type DocJob = (usize, Result<Option<(Value, Value, Value)>, String>);

const VIEWS_FILE: &str = "data_views.json";

/// DIS-03: data view ringan per pola index: field waktu + kolom terakhir.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct DataView {
    time_field: Option<String>,
    columns: Vec<String>,
}

pub(crate) const SAVED_FILE: &str = "saved_searches.json";
const HISTORY_FILE: &str = "query_history.json";
const HISTORY_MAX: usize = 50;

/// DIS-50: pencarian tersimpan per profile.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedSearch {
    pub name: String,
    pub pattern: String,
    pub mode: QueryMode,
    pub esql: bool,
    pub query: String,
    pub filters: Vec<Filter>,
    pub columns: Vec<String>,
    pub sort: Option<(String, bool)>,
    pub time_field: Option<String>,
}

/// Hasil yang dikaitkan dengan kunci (pola/field) saat diminta.
type Keyed<T> = (String, Result<T, String>);
type TopValues = Keyed<TopResult>;

/// DIS-43: hasil mode akurat: terms, plus stats untuk field numeric.
#[derive(Debug, Clone, Default)]
struct TopResult {
    terms: Vec<(String, u64)>,
    stats: Option<Stats>,
}

/// DIS-43: ringkasan aggregation `stats`; min/max/avg `None` bila tanpa dokumen.
#[derive(Debug, Clone, PartialEq)]
struct Stats {
    count: u64,
    min: Option<f64>,
    max: Option<f64>,
    avg: Option<f64>,
    sum: f64,
}

/// DIS-43: parse `aggregations.st` dari response; `None` bila aggregation tidak ada.
fn parse_stats(v: &Value) -> Option<Stats> {
    let st = v["aggregations"].get("st")?;
    Some(Stats {
        count: st["count"].as_u64()?,
        min: st["min"].as_f64(),
        max: st["max"].as_f64(),
        avg: st["avg"].as_f64(),
        sum: st["sum"].as_f64().unwrap_or(0.0),
    })
}

/// Bilangan bulat diformat ribuan; pecahan 2 desimal.
fn format_stat(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e18 {
        if x < 0.0 {
            format!("-{}", widgets::format_count(x.abs() as u64))
        } else {
            widgets::format_count(x as u64)
        }
    } else {
        format!("{x:.2}")
    }
}
/// (id anchor, (lebih baru, lebih lama))
type Around = Keyed<(Vec<Value>, Vec<Value>)>;
/// ((field, awalan), nilai)
type TermsEnum = ((String, String), Vec<String>);

struct EsqlTable {
    columns: Vec<(String, String)>,
    rows: Vec<Vec<Value>>,
    took: u128,
}

impl Discover {
    pub fn open_pattern(&mut self, pattern: &str) {
        self.set_pattern(pattern);
    }

    fn set_pattern(&mut self, pattern: &str) {
        let pattern = pattern.trim().to_owned();
        if pattern != self.pattern {
            self.columns.clear();
            self.sort = None;
            self.time_field = None;
            self.time_field_chosen = false;
            self.field_open = None;
        }
        self.pattern_input = pattern.clone();
        self.pattern = pattern.clone();
        self.pattern_open = false;
        self.needs_search = true;
        self.apply_view(&pattern);
    }

    fn apply_view(&mut self, pattern: &str) {
        if let Some(view) = self.views.get(pattern).cloned() {
            self.columns = view.columns;
            if view.time_field.is_some() {
                self.time_field = view.time_field;
                self.time_field_chosen = true;
            }
        }
    }

    /// Simpan field waktu + kolom pola aktif bila berubah.
    fn remember_view(&mut self, cx: &Ctx) {
        if self.pattern.is_empty() || !self.time_field_chosen {
            return;
        }
        let view = DataView {
            time_field: self.time_field.clone(),
            columns: self.columns.clone(),
        };
        if self.views.get(&self.pattern) != Some(&view) {
            self.views.insert(self.pattern.clone(), view);
            cx.store.save(&cx.profile.id, VIEWS_FILE, &self.views);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let ctx = ui.ctx().clone();
        self.conn = Some(cx.conn.clone());
        self.read_only = cx.conn.read_only;
        self.prod = cx.profile.is_prod();
        if let Some((row, result)) = poll(&mut self.doc_job) {
            match result {
                Ok(Some((source, seq, term))) => {
                    if let Some(hit) = self.hits.get_mut(row) {
                        hit["_source"] = source;
                        hit["_seq_no"] = seq;
                        hit["_primary_term"] = term;
                    }
                    self.doc_edit = None;
                    self.export_result = Some(Ok(t("Dokumen disimpan.").into()));
                }
                Ok(None) => {
                    if row < self.hits.len() {
                        self.hits.remove(row);
                    }
                    self.selected = None;
                    if let Some((n, exact)) = self.total {
                        self.total = Some((n.saturating_sub(1), exact));
                    }
                    self.export_result = Some(Ok(t("Dokumen dihapus.").into()));
                }
                Err(err) => self.export_result = Some(Err(err)),
            }
        }
        if let Some(r) = poll(&mut self.around_pending) {
            self.around = Some(r);
        }
        if let Some((key, values)) = poll(&mut self.terms_pending) {
            self.terms_cache.insert(key, values);
        }
        if let Some(result) = poll(&mut self.pending) {
            self.apply(result);
        }
        if let Some(result) = poll(&mut self.esql_pending) {
            match result {
                Ok(table) => self.esql_result = Some(table),
                Err(err) => self.error = Some(err),
            }
        }
        if self.esql && !cx.caps.esql() {
            self.esql = false;
        }
        if !self.loaded {
            self.loaded = true;
            self.saved = cx
                .store
                .load(&cx.profile.id, SAVED_FILE)
                .unwrap_or_default();
            self.history = cx
                .store
                .load(&cx.profile.id, HISTORY_FILE)
                .unwrap_or_default();
            self.views = cx
                .store
                .load(&cx.profile.id, VIEWS_FILE)
                .unwrap_or_default();
            if !self.pattern.is_empty() {
                let p = self.pattern.clone();
                self.apply_view(&p);
            }
        }
        if let Some(r) = poll(&mut self.top_pending) {
            self.top_accurate = Some(r);
        }
        if let Some((field, numeric)) = self.top_request.take() {
            self.load_top(&ctx, cx.conn, field, numeric);
        }
        let fields: Vec<Field> = match cx.catalog.fields(&ctx, cx.conn, &self.pattern) {
            Some(Ok(fields)) => fields.clone(),
            Some(Err(err)) => {
                if self.error.is_none() && !self.pattern.is_empty() {
                    self.error = Some(err.clone());
                }
                Vec::new()
            }
            None => Vec::new(),
        };
        let fields_ready =
            self.pattern.is_empty() || cx.catalog.fields(&ctx, cx.conn, &self.pattern).is_some();
        if fields_ready && !self.time_field_chosen {
            self.time_field = fields
                .iter()
                .find(|f| f.name == "@timestamp" && f.is_date())
                .or_else(|| fields.iter().find(|f| f.is_date()))
                .map(|f| f.name.clone());
            self.time_field_chosen = !fields.is_empty();
        }
        if self.refresh_secs > 0 && !self.pattern.is_empty() {
            let every = Duration::from_secs(self.refresh_secs);
            if self.last_search.is_none_or(|t| t.elapsed() >= every) {
                self.needs_search = true;
            } else {
                ctx.request_repaint_after(every);
            }
        }
        if self.needs_search && fields_ready && !self.pattern.is_empty() && self.pending.is_none() {
            self.needs_search = false;
            if self.esql {
                self.esql_search(&ctx, cx.conn);
            } else {
                self.search(&ctx, cx.conn, false);
            }
        }

        self.query_bar(ui, cx, &fields);
        egui::Panel::left("discover_fields")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| self.sidebar(ui, cx, &fields));
        if self.selected.is_some() {
            egui::Panel::bottom("discover_doc")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| self.detail(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.results(ui, cx, &fields));
        self.remember_view(cx);
    }

    fn query_bar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, fields: &[Field]) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("query_mode")
                .width(80.0)
                .selected_text(if self.esql {
                    "ES|QL"
                } else {
                    self.mode.label()
                })
                .show_ui(ui, |ui| {
                    for mode in QueryMode::ALL {
                        if ui
                            .selectable_label(!self.esql && self.mode == mode, mode.label())
                            .clicked()
                        {
                            self.mode = mode;
                            self.esql = false;
                        }
                    }
                    if cx.caps.esql()
                        && ui.selectable_label(self.esql, "ES|QL").clicked()
                        && !self.esql
                    {
                        self.esql = true;
                        if !self
                            .query
                            .trim_start()
                            .to_ascii_uppercase()
                            .starts_with("FROM")
                        {
                            self.query = format!("FROM {} | LIMIT 100", self.pattern);
                        }
                    }
                });
            let width = (ui.available_width() - 320.0).max(200.0);
            let submitted = match self.mode {
                _ if self.esql => {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.query)
                            .desired_width(width)
                            .desired_rows(3)
                            .code_editor()
                            .hint_text(
                                "FROM logs-* | WHERE status >= 500 | STATS c = COUNT(*) BY host",
                            ),
                    );
                    false
                }
                QueryMode::Kql | QueryMode::Lucene => {
                    let hint = if self.mode == QueryMode::Kql {
                        "status:500 and host:web-* and not level:debug"
                    } else {
                        "status:500 AND host:web-*"
                    };
                    self.query_input(ui, cx, fields, width, hint)
                }
                QueryMode::Dsl => {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.query)
                            .desired_width(width)
                            .desired_rows(3)
                            .code_editor()
                            .hint_text("{\"match\": {\"message\": \"error\"}}"),
                    );
                    false
                }
            };
            if self.time_field.is_some()
                && widgets::time::time_picker(ui, &mut self.range, &mut self.refresh_secs)
            {
                self.needs_search = true;
            }
            let busy = self.pending.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new(t("Cari")))
                .clicked()
                || submitted
            {
                cx.catalog.refresh_fields(&self.pattern);
                self.needs_search = true;
                let q = self.query.trim().to_owned();
                if !q.is_empty() {
                    self.history.retain(|h| *h != q);
                    self.history.insert(0, q);
                    self.history.truncate(HISTORY_MAX);
                    cx.store.save(&cx.profile.id, HISTORY_FILE, &self.history);
                }
            }
            if !self.history.is_empty() {
                ui.menu_button("🕘", |ui| {
                    ui.set_max_width(500.0);
                    for h in self.history.clone() {
                        if ui
                            .button(RichText::new(truncate(&h, 80)).monospace())
                            .on_hover_text(&h)
                            .clicked()
                        {
                            self.query = h;
                            self.needs_search = true;
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("History query");
            }
            if busy {
                ui.spinner();
            }
            if let Some(body) = &self.last_body
                && ui
                    .small_button("Inspect")
                    .on_hover_text(t("Buka request ini di Dev Tools"))
                    .clicked()
            {
                *cx.nav = Some(Nav::Console {
                    method: "POST".into(),
                    path: format!("/{}/_search", self.pattern),
                    body: Some(strip_pit(body)),
                });
            }
        });
        if filters::filter_bar(ui, &mut self.filters, fields, &mut self.filter_draft) {
            self.needs_search = true;
        }
        ui.separator();
    }

    /// DIS-14: input query dengan saran nama field dan nilai (`_terms_enum`).
    fn query_input(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut Ctx,
        fields: &[Field],
        width: f32,
        hint: &str,
    ) -> bool {
        use egui::text::{CCursor, CCursorRange};
        use egui::widgets::text_edit::TextEditState;
        let id = egui::Id::unique("discover_query");
        let focused = ui.memory(|m| m.has_focus(id));
        let mut accept = None;
        if focused && !self.suggest.is_empty() {
            let n = self.suggest.len();
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.suggest_selected = (self.suggest_selected + 1) % n;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.suggest_selected = (self.suggest_selected + n - 1) % n;
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                    accept = Some(self.suggest_selected);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.suggest.clear();
                }
            });
        }
        let output = egui::TextEdit::singleline(&mut self.query)
            .id(id)
            .desired_width(width)
            .font(egui::TextStyle::Monospace)
            .hint_text(hint)
            .show(ui);
        let submitted =
            output.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if submitted || !focused {
            self.suggest.clear();
        }
        if let Some(range) = output.cursor_range
            && (output.response.changed()
                || accept.is_some()
                || (focused && !self.suggest.is_empty()))
        {
            let cursor = crate::ui::console::char_to_byte(&self.query, range.primary.index.0);
            self.update_suggest(ui.ctx(), cx, fields, cursor);
        }
        // Popup di bawah input.
        let mut clicked = None;
        if focused && !self.suggest.is_empty() {
            egui::Area::new(id.with("popup"))
                .order(egui::Order::Foreground)
                .fixed_pos(output.response.rect.left_bottom())
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(260.0);
                        for (i, (label, detail)) in self.suggest.iter().enumerate() {
                            let r = ui.horizontal(|ui| {
                                let r = ui.selectable_label(
                                    i == self.suggest_selected,
                                    RichText::new(label).monospace(),
                                );
                                ui.weak(detail);
                                r
                            });
                            if r.inner.clicked() {
                                clicked = Some(i);
                            }
                        }
                        ui.weak(t("↑↓ pilih · Tab terima · Enter cari"));
                    });
                });
        }
        if let Some(i) = accept.or(clicked)
            && let Some((label, _)) = self.suggest.get(i).cloned()
        {
            let (start, end) = self.suggest_span;
            let insert = if label.ends_with(':') || label.starts_with('"') {
                label
            } else {
                format!("{label} ")
            };
            if start <= end && end <= self.query.len() {
                self.query.replace_range(start..end, &insert);
                let cursor = crate::ui::console::byte_to_char(&self.query, start + insert.len());
                if let Some(mut state) = TextEditState::load(ui.ctx(), id) {
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
                    state.store(ui.ctx(), id);
                }
                ui.memory_mut(|m| m.request_focus(id));
            }
            self.suggest.clear();
        }
        submitted
    }

    fn update_suggest(
        &mut self,
        ctx: &egui::Context,
        cx: &mut Ctx,
        fields: &[Field],
        cursor: usize,
    ) {
        let before = &self.query[..cursor.min(self.query.len())];
        let start = before
            .rfind(|c: char| c.is_whitespace() || c == '(')
            .map_or(0, |i| i + 1);
        let token = &before[start..];
        let previous = self.suggest.get(self.suggest_selected).map(|s| s.0.clone());
        self.suggest.clear();
        if let Some((field, partial)) = token.split_once(':') {
            let partial = partial.trim_start_matches('"');
            let Some(f) = fields.iter().find(|f| f.name == field) else {
                return;
            };
            if !matches!(
                f.es_type(),
                "keyword" | "constant_keyword" | "ip" | "version" | "flattened"
            ) {
                return;
            }
            let key = (field.to_owned(), partial.to_owned());
            match self.terms_cache.get(&key) {
                Some(values) => {
                    self.suggest_span = (start + field.len() + 1, cursor);
                    for v in values {
                        let quoted =
                            if v.contains(char::is_whitespace) || v.contains([':', '(', ')']) {
                                format!("\"{v}\"")
                            } else {
                                v.clone()
                            };
                        self.suggest.push((quoted, t("nilai").into()));
                    }
                }
                None if self.terms_pending.is_none() => {
                    let conn = cx.conn.clone();
                    let path = format!("/{}/_terms_enum", self.pattern);
                    let body = json!({"field": field, "string": partial, "size": 10});
                    self.terms_pending = Some(Pending::spawn(ctx, move || {
                        let values = conn
                            .json("POST", &path, Some(&body))
                            .map(|v| {
                                v["terms"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|t| t.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        (key, values)
                    }));
                }
                None => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        } else if !token.is_empty() {
            let needle = token.to_lowercase();
            self.suggest_span = (start, cursor);
            let mut matches: Vec<&Field> = fields
                .iter()
                .filter(|f| f.name.to_lowercase().contains(&needle) && f.name != token)
                .collect();
            matches.sort_by_key(|f| (!f.name.to_lowercase().starts_with(&needle), f.name.len()));
            for f in matches.into_iter().take(12) {
                self.suggest
                    .push((format!("{}:", f.name), f.es_type().to_owned()));
            }
        }
        self.suggest_selected = previous
            .and_then(|p| self.suggest.iter().position(|s| s.0 == p))
            .unwrap_or(0);
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, fields: &[Field]) {
        self.saved_ui(ui, cx);
        ui.label(RichText::new(t("Sumber data")).strong());
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.pattern_input)
                .hint_text("logs-*, alias, data stream")
                .desired_width(f32::INFINITY),
        );
        if response.gained_focus() {
            self.pattern_open = true;
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let p = self.pattern_input.clone();
            self.set_pattern(&p);
        }
        self.pattern_preview(ui, cx);
        if self.pattern_open {
            let needle = self.pattern_input.to_lowercase();
            let targets: Vec<(String, &'static str)> = cx
                .catalog
                .targets(ui.ctx(), cx.conn)
                .iter()
                .filter(|t| needle.is_empty() || t.name.to_lowercase().contains(&needle))
                .take(60)
                .map(|t| (t.name.clone(), t.kind.label()))
                .collect();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .id_salt("targets")
                    .show(ui, |ui| {
                        if !self.pattern_input.trim().is_empty()
                            && ui
                                .selectable_label(
                                    false,
                                    tf("Gunakan pola \"{0}\"", &[&self.pattern_input.trim()]),
                                )
                                .clicked()
                        {
                            let p = self.pattern_input.clone();
                            self.set_pattern(&p);
                        }
                        for (name, kind) in targets {
                            if ui
                                .selectable_label(false, format!("{name}  ·  {kind}"))
                                .clicked()
                            {
                                self.set_pattern(&name);
                            }
                        }
                        if cx.catalog.loading_targets() {
                            ui.spinner();
                        }
                    });
                if ui.small_button(t("Tutup")).clicked() {
                    self.pattern_open = false;
                }
            });
        }
        if self.pattern.is_empty() {
            ui.weak(t("Pilih index, alias, atau data stream."));
            return;
        }
        let dates: Vec<&Field> = fields.iter().filter(|f| f.is_date()).collect();
        ui.horizontal(|ui| {
            ui.label(t("Field waktu"));
            egui::ComboBox::from_id_salt("time_field")
                .selected_text(
                    self.time_field
                        .clone()
                        .unwrap_or_else(|| t("tanpa waktu").into()),
                )
                .show_ui(ui, |ui| {
                    let before = self.time_field.clone();
                    ui.selectable_value(&mut self.time_field, None, t("tanpa waktu"));
                    for f in &dates {
                        ui.selectable_value(&mut self.time_field, Some(f.name.clone()), &f.name);
                    }
                    if before != self.time_field {
                        self.time_field_chosen = true;
                        self.needs_search = true;
                    }
                });
        });
        ui.separator();
        ui.add(egui::TextEdit::singleline(&mut self.field_search).hint_text(t("Cari field…")));
        let needle = self.field_search.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("field_list")
            .show(ui, |ui| {
                if !self.columns.is_empty() {
                    ui.label(RichText::new(t("Kolom terpilih")).weak());
                    for name in self.columns.clone() {
                        if let Some(f) = fields.iter().find(|f| f.name == name) {
                            self.field_row(ui, f, true);
                        }
                    }
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tf("Field ({0})", &[&fields.len()])).weak());
                    ui.checkbox(&mut self.field_tree, "tree");
                });
                let available: Vec<&Field> = fields
                    .iter()
                    .filter(|f| !self.columns.contains(&f.name))
                    .filter(|f| needle.is_empty() || f.name.to_lowercase().contains(&needle))
                    .collect();
                if !self.field_tree {
                    for f in available {
                        self.field_row(ui, f, false);
                    }
                    return;
                }
                // Kelompokkan per segmen pertama (objek/nested), mis. `host.*`.
                let mut groups: std::collections::BTreeMap<&str, Vec<&Field>> = Default::default();
                for f in available {
                    let head = f.name.split('.').next().unwrap_or(&f.name);
                    groups.entry(head).or_default().push(f);
                }
                for (head, members) in groups {
                    if members.len() == 1 && members[0].name == head {
                        self.field_row(ui, members[0], false);
                        continue;
                    }
                    egui::CollapsingHeader::new(format!("{head} ({})", members.len()))
                        .id_salt(("field_group", head))
                        .default_open(!needle.is_empty())
                        .show(ui, |ui| {
                            for f in members {
                                self.field_row(ui, f, false);
                            }
                        });
                }
            });
    }

    fn saved_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        ui.menu_button(
            tf("⭐ Pencarian tersimpan ({0})", &[&self.saved.len()]),
            |ui| {
                ui.set_min_width(280.0);
                let mut remove = None;
                let mut load = None;
                for (i, s) in self.saved.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .button(&s.name)
                            .on_hover_text(format!("{} · {}", s.pattern, s.query))
                            .clicked()
                        {
                            load = Some(s.clone());
                            ui.close();
                        }
                        if ui.small_button("🗑").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if self.saved.is_empty() {
                    ui.weak(t("Belum ada pencarian tersimpan."));
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_name)
                            .hint_text(t("Nama"))
                            .desired_width(160.0),
                    );
                    let ok = !self.save_name.trim().is_empty() && !self.pattern.is_empty();
                    if ui.add_enabled(ok, egui::Button::new(t("Simpan"))).clicked() {
                        let name = self.save_name.trim().to_owned();
                        self.saved.retain(|s| s.name != name);
                        self.saved.push(SavedSearch {
                            name,
                            pattern: self.pattern.clone(),
                            mode: self.mode,
                            esql: self.esql,
                            query: self.query.clone(),
                            filters: self.filters.clone(),
                            columns: self.columns.clone(),
                            sort: self.sort.clone(),
                            time_field: self.time_field.clone(),
                        });
                        self.save_name.clear();
                        cx.store.save(&cx.profile.id, SAVED_FILE, &self.saved);
                        ui.close();
                    }
                });
                if let Some(i) = remove {
                    self.saved.remove(i);
                    cx.store.save(&cx.profile.id, SAVED_FILE, &self.saved);
                }
                if let Some(s) = load {
                    self.set_pattern(&s.pattern);
                    self.mode = s.mode;
                    self.esql = s.esql;
                    self.query = s.query;
                    // DIS-18: filter yang dipin tetap dipakai.
                    let mut filters: Vec<Filter> =
                        self.filters.iter().filter(|f| f.pinned).cloned().collect();
                    let pinned: Vec<String> = filters.iter().map(Filter::label).collect();
                    filters.extend(
                        s.filters
                            .into_iter()
                            .filter(|f| !pinned.contains(&f.label())),
                    );
                    self.filters = filters;
                    self.columns = s.columns;
                    self.sort = s.sort;
                    if s.time_field.is_some() {
                        self.time_field = s.time_field;
                        self.time_field_chosen = true;
                    }
                }
            },
        );
        ui.separator();
    }

    fn load_top(&mut self, ctx: &egui::Context, conn: &Arc<Conn>, field: String, numeric: bool) {
        let query = self
            .last_body
            .as_ref()
            .map(|b| b["query"].clone())
            .unwrap_or_else(|| json!({"match_all": {}}));
        let mut body = json!({"size": 0, "query": query, "aggs": {"top": {"terms": {"field": field, "size": 5}}}});
        if numeric {
            // DIS-43: stats dalam request yang sama untuk field numeric.
            body["aggs"]["st"] = json!({"stats": {"field": field}});
        }
        let (conn, pattern) = (conn.clone(), self.pattern.clone());
        self.top_accurate = None;
        self.top_pending = Some(Pending::spawn(ctx, move || {
            let result = conn
                .json("POST", &format!("/{pattern}/_search"), Some(&body))
                .map(|v| TopResult {
                    terms: v["aggregations"]["top"]["buckets"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|b| {
                            (
                                query::display(&b["key"]),
                                b["doc_count"].as_u64().unwrap_or(0),
                            )
                        })
                        .collect(),
                    stats: parse_stats(&v),
                });
            (field, result)
        }));
    }

    /// DIS-51: ekspor CSV kolom terpilih (atau semua field) lewat PIT + search_after.
    fn export_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, fields: &[Field]) {
        if let Some(export) = &mut self.export {
            if let Some(result) = poll(&mut export.pending) {
                self.export_result = Some(result.map(|(n, path)| {
                    tf(
                        "{0} baris diekspor ke {1}",
                        &[&widgets::format_count(n as u64), &path],
                    )
                }));
                self.export = None;
                return;
            }
            ui.spinner();
            ui.weak(tf(
                "Ekspor {0} baris…",
                &[&widgets::format_count(
                    export.rows.load(Ordering::Relaxed) as u64
                )],
            ));
            if ui.small_button(t("Batal")).clicked() {
                export.cancel.store(true, Ordering::Relaxed);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(200));
            return;
        }
        let Some(body) = &self.last_body else { return };
        if !ui
            .small_button(t("Ekspor CSV"))
            .on_hover_text(tf(
                "Maksimal {0} baris",
                &[&widgets::format_count(EXPORT_MAX as u64)],
            ))
            .clicked()
        {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!(
                "{}.csv",
                self.pattern.replace(['*', ',', '/'], "_")
            ))
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let mut columns: Vec<String> = self.time_field.iter().cloned().collect();
        if self.columns.is_empty() {
            columns.push("_id".into());
            columns.extend(
                fields
                    .iter()
                    .filter(|f| {
                        !f.name.starts_with('_') && Some(&f.name) != self.time_field.as_ref()
                    })
                    .map(|f| f.name.clone()),
            );
        } else {
            columns.extend(self.columns.iter().cloned());
        }
        let mut body = strip_pit(body);
        if let Some(o) = body.as_object_mut() {
            o.remove("aggs");
            o.insert("size".into(), json!(EXPORT_PAGE));
            o.insert("track_total_hits".into(), json!(false));
        }
        let rows = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (conn, pattern) = (cx.conn.clone(), self.pattern.clone());
        let (r, c) = (rows.clone(), cancel.clone());
        self.export_result = None;
        self.export = Some(Export {
            rows,
            cancel,
            pending: Some(Pending::spawn(ui.ctx(), move || {
                export_csv(&conn, &pattern, body, &columns, &path, &r, &c)
                    .map(|n| (n, path.display().to_string()))
            })),
        });
    }

    /// Preview `_resolve/index` untuk pola yang diketik (debounce 300 ms).
    fn pattern_preview(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if let Some(result) = poll(&mut self.preview_pending) {
            self.preview = Some(result);
        }
        let input = self.pattern_input.trim().to_owned();
        let shown = self.preview.as_ref().map(|(p, _)| p.as_str());
        if !input.is_empty() && !input.contains(' ') && shown != Some(input.as_str()) {
            match &self.preview_typed {
                Some((typed, at)) if *typed == input => {
                    if at.elapsed() >= Duration::from_millis(300) && self.preview_pending.is_none()
                    {
                        let conn = cx.conn.clone();
                        let pattern = input.clone();
                        self.preview_pending = Some(Pending::spawn(ui.ctx(), move || {
                            let result = resolve_pattern(&conn, &pattern);
                            (pattern, result)
                        }));
                    } else {
                        ui.ctx().request_repaint_after(Duration::from_millis(300));
                    }
                }
                _ => {
                    self.preview_typed = Some((input.clone(), Instant::now()));
                    ui.ctx().request_repaint_after(Duration::from_millis(300));
                }
            }
        }
        match &self.preview {
            Some((pattern, Ok(names))) if *pattern == input => {
                let list = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                let more = if names.len() > 5 { ", …" } else { "" };
                let text = if names.is_empty() {
                    t("Tidak ada index yang cocok.").to_owned()
                } else {
                    tf("Cocok: {0} ({1}{2})", &[&names.len(), &list, &more])
                };
                let r = ui.weak(text);
                if names.len() > 5 {
                    r.on_hover_text(names.join("\n"));
                }
            }
            Some((pattern, Err(err))) if *pattern == input => {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            _ => {}
        }
    }

    fn field_row(&mut self, ui: &mut egui::Ui, f: &Field, selected: bool) {
        ui.horizontal(|ui| {
            let icon = type_icon(f);
            ui.label(RichText::new(icon).monospace().weak())
                .on_hover_text(f.types.join(" | "));
            if f.types.len() > 1 {
                ui.label(RichText::new("⚠").color(ui.visuals().warn_fg_color))
                    .on_hover_text(tf("Konflik tipe antar index: {0}", &[&f.types.join(", ")]));
            }
            let open = self.field_open.as_deref() == Some(f.name.as_str());
            if ui.selectable_label(open, &f.name).clicked() {
                self.field_open = if open { None } else { Some(f.name.clone()) };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if selected {
                    if ui
                        .small_button("−")
                        .on_hover_text(t("Hapus kolom"))
                        .clicked()
                    {
                        self.columns.retain(|c| c != &f.name);
                    }
                    // DIS-31: urutan kolom (right_to_left, jadi ↓ ditambah lebih dulu).
                    if let Some(i) = self.columns.iter().position(|c| c == &f.name) {
                        let last = i + 1 == self.columns.len();
                        if ui
                            .add_enabled(!last, egui::Button::new("↓").small())
                            .on_hover_text(t("Geser kolom ke bawah"))
                            .clicked()
                        {
                            move_column(&mut self.columns, i, 1);
                        }
                        if ui
                            .add_enabled(i > 0, egui::Button::new("↑").small())
                            .on_hover_text(t("Geser kolom ke atas"))
                            .clicked()
                        {
                            move_column(&mut self.columns, i, -1);
                        }
                    }
                } else if ui
                    .small_button("+")
                    .on_hover_text(t("Tambah kolom"))
                    .clicked()
                {
                    self.columns.push(f.name.clone());
                }
            });
        });
        if self.field_open.as_deref() == Some(f.name.as_str()) {
            self.top_values(ui, f);
        }
    }

    fn top_values(&mut self, ui: &mut egui::Ui, f: &Field) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut present = 0;
        for hit in &self.hits {
            if let Some(v) = hit_value(hit, &f.name) {
                present += 1;
                let values: Vec<String> = match v {
                    Value::Array(items) => items.iter().map(query::display).collect(),
                    other => vec![query::display(other)],
                };
                for value in values {
                    *counts.entry(value).or_default() += 1;
                }
            }
        }
        let mut top: Vec<(String, usize)> = counts.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.weak(tf(
                "Top 5 dari {0} dokumen termuat ({1} memiliki field ini)",
                &[&self.hits.len(), &present],
            ));
            if top.is_empty() {
                ui.weak(t("Tidak ada nilai di dokumen termuat."));
            }
            for (value, count) in top.into_iter().take(5) {
                let pct = count as f32 / self.hits.len().max(1) as f32;
                ui.horizontal(|ui| {
                    if ui
                        .small_button("+")
                        .on_hover_text(t("Filter nilai ini"))
                        .clicked()
                    {
                        self.filters.push(Filter::is(&f.name, &value, false));
                        self.needs_search = true;
                    }
                    if ui
                        .small_button("−")
                        .on_hover_text(t("Kecualikan nilai ini"))
                        .clicked()
                    {
                        self.filters.push(Filter::is(&f.name, &value, true));
                        self.needs_search = true;
                    }
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .desired_width(60.0)
                            .text(format!("{:.0}%", pct * 100.0)),
                    );
                    ui.label(truncate(&value, 40)).on_hover_text(&value);
                });
            }
            if f.aggregatable {
                if self.top_pending.is_some() {
                    ui.spinner();
                } else if ui
                    .small_button(t("Hitung akurat (seluruh rentang)"))
                    .on_hover_text(t("terms aggregation dengan query dan filter saat ini"))
                    .clicked()
                {
                    self.top_request = Some((f.name.clone(), f.is_numeric()));
                }
                if let Some((name, result)) = &self.top_accurate
                    && *name == f.name
                {
                    match result {
                        Ok(res) => {
                            if let Some(st) = &res.stats {
                                let opt = |x: Option<f64>| x.map_or("-".to_string(), format_stat);
                                ui.weak(format!(
                                    "min {} · max {} · avg {} · sum {} · count {}",
                                    opt(st.min),
                                    opt(st.max),
                                    st.avg.map_or("-".to_string(), |a| format!("{a:.2}")),
                                    format_stat(st.sum),
                                    widgets::format_count(st.count)
                                ));
                            }
                            ui.weak(t("Top 5 akurat:"));
                            for (value, count) in res.terms.clone() {
                                ui.horizontal(|ui| {
                                    if ui.small_button("+").clicked() {
                                        self.filters.push(Filter::is(&f.name, &value, false));
                                        self.needs_search = true;
                                    }
                                    ui.label(format!(
                                        "{} · {}",
                                        truncate(&value, 40),
                                        widgets::format_count(count)
                                    ));
                                });
                            }
                        }
                        Err(err) => widgets::error_box(ui, err),
                    }
                }
            }
            if ui.small_button(t("Filter: field ada (exists)")).clicked() {
                self.filters.push(Filter {
                    op: query::FilterOp::Exists,
                    ..Filter::is(&f.name, "", false)
                });
                self.needs_search = true;
            }
        });
    }

    fn results(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, fields: &[Field]) {
        if let Some(err) = &self.error {
            widgets::error_box(ui, err);
        }
        if let Some(warn) = &self.warning {
            widgets::warn_box(ui, warn);
        }
        if self.pattern.is_empty() {
            ui.weak(t("Pilih sumber data di panel kiri untuk mulai."));
            return;
        }
        if self.esql {
            self.esql_table(ui);
            return;
        }
        ui.horizontal(|ui| {
            match self.total {
                Some((n, true)) => ui.strong(format!("{} hit", widgets::format_count(n))),
                Some((n, false)) => ui.strong(format!("{}+ hit", widgets::format_count(n))),
                None => ui.weak("…"),
            };
            ui.weak(tf(
                "· {0} termuat · {1} ms",
                &[&self.hits.len(), &self.took],
            ));
            self.export_ui(ui, cx, fields);
            if ui
                .checkbox(&mut self.exact_total, t("hitung total akurat"))
                .on_hover_text(t("track_total_hits: true. Lebih lambat untuk data besar."))
                .changed()
            {
                self.needs_search = true;
            }
            if let Some(tfield) = &self.time_field {
                ui.weak(tf("· interval {0} · {1}", &[&self.interval.1, tfield]));
            }
        });
        match &self.export_result {
            Some(Ok(text)) => {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xa0, 0x43), text);
            }
            Some(Err(err)) => widgets::error_box(ui, err),
            None => {}
        }
        if self.searching_async && self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(t(
                    "Rentang waktu lebar: query dijalankan sebagai async search di server…",
                ));
            });
        }
        if self.time_field.is_some() && !self.buckets.is_empty() {
            // DIS-23: breakdown histogram per nilai field.
            ui.horizontal(|ui| {
                ui.weak("Breakdown");
                let before = self.breakdown.clone();
                egui::ComboBox::from_id_salt("breakdown")
                    .selected_text(self.breakdown.clone().unwrap_or_else(|| t("tanpa").into()))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.breakdown, None, t("tanpa"));
                        for f in fields
                            .iter()
                            .filter(|f| f.aggregatable && !f.is_date() && !f.name.starts_with('_'))
                        {
                            ui.selectable_value(&mut self.breakdown, Some(f.name.clone()), &f.name);
                        }
                    });
                if before != self.breakdown {
                    self.needs_search = true;
                }
            });
            self.histogram(ui);
        }
        self.table(ui, cx, fields);
    }

    fn histogram(&mut self, ui: &mut egui::Ui) {
        let width = self.interval.0 as f64;
        let bars: Vec<Bar> = self
            .buckets
            .iter()
            .map(|(x, y)| Bar::new(*x + width / 2.0, *y).width(width * 0.9))
            .collect();
        let chart = BarChart::new("hits", bars).color(ui.visuals().selection.bg_fill);
        // Seri breakdown ditumpuk; sisa (di luar top 5) tampil abu-abu.
        let mut stacked: Vec<BarChart> = Vec::new();
        if self.breakdown.is_some() && !self.series.is_empty() {
            for (i, (key, points)) in self.series.iter().enumerate() {
                let bars = points
                    .iter()
                    .map(|(x, y)| Bar::new(*x + width / 2.0, *y).width(width * 0.9))
                    .collect();
                let refs: Vec<&BarChart> = stacked.iter().collect();
                let color = BREAKDOWN_COLORS[i % BREAKDOWN_COLORS.len()];
                stacked.push(
                    BarChart::new(key.clone(), bars)
                        .color(color)
                        .stack_on(&refs),
                );
            }
            let other: Vec<Bar> = self
                .buckets
                .iter()
                .map(|(x, total)| {
                    let shown: f64 = self
                        .series
                        .iter()
                        .flat_map(|(_, pts)| pts.iter().filter(|(px, _)| px == x).map(|(_, y)| *y))
                        .sum();
                    Bar::new(*x + width / 2.0, (total - shown).max(0.0)).width(width * 0.9)
                })
                .collect();
            let refs: Vec<&BarChart> = stacked.iter().collect();
            stacked.push(
                BarChart::new("lainnya", other)
                    .color(egui::Color32::GRAY)
                    .stack_on(&refs),
            );
        }
        let response = Plot::new("discover_histogram")
            .height(110.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .show_axes([true, true])
            .y_axis_min_width(36.0)
            .x_axis_formatter(|mark, _| short_time(mark.value as i64))
            .label_formatter(|pos| match pos {
                HoverPosition::NearDataPoint { position, .. } => Some(format!(
                    "{}\n{:.0} hit",
                    widgets::format_time_ms(position.x as i64),
                    position.y
                )),
                HoverPosition::Elsewhere { .. } => None,
            })
            .legend(egui_plot::Legend::default())
            .show(ui, |plot| {
                if stacked.is_empty() {
                    plot.bar_chart(chart);
                } else {
                    for c in stacked {
                        plot.bar_chart(c);
                    }
                }
            });
        let r = &response.response;
        let pointer_x = r
            .interact_pointer_pos()
            .or(r.hover_pos())
            .map(|p| response.transform.value_from_position(p).x);
        if r.drag_started() {
            self.drag_from = pointer_x;
        }
        if let (Some(from), Some(to)) = (self.drag_from, pointer_x) {
            let a = response.transform.position_from_point_x(from);
            let b = response.transform.position_from_point_x(to);
            let rect = egui::Rect::from_x_y_ranges(a.min(b)..=a.max(b), r.rect.y_range());
            ui.painter().rect_filled(
                rect,
                0.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.3),
            );
        }
        if r.drag_stopped() {
            if let (Some(from), Some(to)) = (self.drag_from.take(), pointer_x)
                && (to - from).abs() > width / 4.0
            {
                self.zoom(from.min(to) as i64, from.max(to) as i64);
            }
        } else if r.clicked()
            && let Some(x) = pointer_x
        {
            let start = ((x as i64) / self.interval.0) * self.interval.0;
            if let Some((bx, _)) = self
                .buckets
                .iter()
                .find(|(bx, _)| (*bx as i64) <= start && start < *bx as i64 + self.interval.0)
            {
                self.zoom(*bx as i64, *bx as i64 + self.interval.0);
            }
        }
    }

    fn zoom(&mut self, from: i64, to: i64) {
        self.range = TimeRange::new(&iso_local(from), &iso_local(to));
        self.needs_search = true;
    }

    fn table(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, fields: &[Field]) {
        let time_field = self.time_field.clone();
        let columns = self.columns.clone();
        let mut clicked_row = None;
        let mut sort_click = None;
        let mut move_click: Option<(usize, isize)> = None;
        let selected = self.selected;
        let height = ui.available_height() - 30.0;
        let mut builder = TableBuilder::new(ui)
            .id_salt("discover_table")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .max_scroll_height(height.max(100.0))
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        if time_field.is_some() {
            builder = builder.column(Column::initial(190.0).clip(true));
        }
        for _ in &columns {
            builder = builder.column(Column::initial(200.0).clip(true).at_least(60.0));
        }
        if columns.is_empty() {
            builder = builder.column(Column::remainder().clip(true));
        } else {
            builder = builder.column(Column::remainder().clip(true).at_least(40.0));
        }
        let sort = self.sort.clone();
        let header_label = |name: &str| {
            let same = |f: &String| f == name || *f == format!("{name}.keyword");
            let arrow = match &sort {
                Some((f, true)) if same(f) => " ↓",
                Some((f, false)) if same(f) => " ↑",
                _ => "",
            };
            format!("{name}{arrow}")
        };
        builder
            .header(22.0, |mut header| {
                if let Some(tf) = &time_field {
                    header.col(|ui| {
                        let label = if self.sort.is_none() {
                            format!("{tf} ↓")
                        } else {
                            header_label(tf)
                        };
                        if ui.button(RichText::new(label).strong()).clicked() {
                            sort_click = Some(tf.clone());
                        }
                    });
                }
                for (idx, c) in columns.iter().enumerate() {
                    header.col(|ui| {
                        // DIS-31: geser kolom ke kiri/kanan.
                        if ui
                            .add_enabled(idx > 0, egui::Button::new("◀").small())
                            .on_hover_text(t("Geser kolom ke kiri"))
                            .clicked()
                        {
                            move_click = Some((idx, -1));
                        }
                        if ui
                            .add_enabled(idx + 1 < columns.len(), egui::Button::new("▶").small())
                            .on_hover_text(t("Geser kolom ke kanan"))
                            .clicked()
                        {
                            move_click = Some((idx, 1));
                        }
                        let target = sort_target(c, fields);
                        let text = RichText::new(header_label(c)).strong();
                        if let Some(target) = target {
                            let hint = if target == *c {
                                t("Klik untuk sort").to_owned()
                            } else {
                                tf("Klik untuk sort (memakai {0})", &[&target])
                            };
                            if ui.button(text).on_hover_text(hint).clicked() {
                                sort_click = Some(target);
                            }
                        } else {
                            ui.label(text)
                                .on_hover_text(t("Field ini tidak bisa di-sort"));
                        }
                    });
                }
                header.col(|ui| {
                    ui.strong(if columns.is_empty() { t("Dokumen") } else { "" });
                });
            })
            .body(|body| {
                body.rows(20.0, self.hits.len(), |mut row| {
                    let i = row.index();
                    row.set_selected(selected == Some(i));
                    let hit = &self.hits[i];
                    if let Some(tf) = &time_field {
                        row.col(|ui| {
                            let text = hit_value(hit, tf).map(query::display).unwrap_or_default();
                            ui.monospace(format_hit_time(&text));
                        });
                    }
                    for c in &columns {
                        row.col(|ui| {
                            let text = highlight_of(hit, c).unwrap_or_else(|| {
                                hit_value(hit, c)
                                    .map(query::display)
                                    .unwrap_or_else(|| "-".into())
                            });
                            widgets::highlight_label(ui, &truncate(&text, 300));
                        });
                    }
                    row.col(|ui| {
                        if columns.is_empty() {
                            widgets::highlight_label(ui, &summary(hit));
                        }
                    });
                    if row.response().clicked() {
                        clicked_row = Some(i);
                    }
                });
            });
        if let Some((i, delta)) = move_click {
            move_column(&mut self.columns, i, delta);
        }
        if let Some(i) = clicked_row {
            self.selected = if self.selected == Some(i) {
                None
            } else {
                Some(i)
            };
        }
        if let Some(field) = sort_click {
            self.sort = match &self.sort {
                Some((f, desc)) if *f == field => Some((field, !desc)),
                None if Some(&field) == time_field.as_ref() => Some((field, false)),
                _ => Some((field, true)),
            };
            self.needs_search = true;
        }
        let more = self
            .total
            .is_some_and(|(n, _)| (self.hits.len() as u64) < n);
        if more {
            ui.horizontal(|ui| {
                let enabled = self.pending.is_none() && self.pit.is_some();
                if ui
                    .add_enabled(enabled, egui::Button::new(tf("Muat {0} lagi", &[&PAGE])))
                    .clicked()
                {
                    self.search(ui.ctx(), cx.conn, true);
                }
                if self.pit.is_none() {
                    ui.weak(t("Paging lanjutan butuh point-in-time (tidak tersedia)."));
                }
            });
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui) {
        let Some(hit) = self.selected.and_then(|i| self.hits.get(i)).cloned() else {
            self.selected = None;
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(format!(
                "{} / {}",
                hit["_index"].as_str().unwrap_or("?"),
                hit["_id"].as_str().unwrap_or("?")
            ));
            ui.selectable_value(&mut self.detail_json, false, t("Tabel"));
            ui.selectable_value(&mut self.detail_json, true, "JSON");
            if ui.small_button(t("Salin JSON")).clicked() {
                let source = serde_json::to_string_pretty(&hit["_source"]).unwrap_or_default();
                ui.ctx().copy_text(source);
            }
            if ui.small_button(t("Salin _id")).clicked() {
                ui.ctx()
                    .copy_text(hit["_id"].as_str().unwrap_or_default().to_owned());
            }
            if let Some(tf) = self.time_field.clone()
                && self.around_pending.is_none()
                && ui
                    .small_button(t("Dokumen sekitar"))
                    .on_hover_text(t(
                        "5 dokumen sebelum dan sesudah berdasarkan waktu (filter aktif tetap berlaku)",
                    ))
                    .clicked()
            {
                self.load_around(ui.ctx(), &hit, &tf);
            }
            if self.around_pending.is_some() {
                ui.spinner();
            }
            // DIS-39: edit/hapus dokumen (disembunyikan di profile read-only).
            if !self.read_only && self.doc_job.is_none() {
                if ui.small_button("Edit").clicked() {
                    let text = serde_json::to_string_pretty(&hit["_source"]).unwrap_or_default();
                    self.doc_edit = self.selected.map(|i| (i, text));
                }
                if ui.small_button(t("Hapus")).clicked() {
                    self.doc_delete = self.selected;
                }
            }
            if self.doc_job.is_some() {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    self.selected = None;
                    self.doc_edit = None;
                }
            });
        });
        ui.separator();
        self.doc_edit_ui(ui, &hit);
        self.doc_delete_ui(ui.ctx(), &hit);
        egui::ScrollArea::both()
            .id_salt("doc_detail")
            .auto_shrink(false)
            .show(ui, |ui| {
                let id = hit["_id"].as_str().unwrap_or_default();
                if let Some((anchor, result)) = &self.around
                    && anchor == id
                {
                    self.around_ui(ui, &hit, result);
                    ui.separator();
                }
                if self.detail_json {
                    widgets::json_tree(
                        ui,
                        &hit,
                        egui::Id::unique(("doc", hit["_id"].as_str().unwrap_or_default())),
                    );
                    return;
                }
                let mut flat = Vec::new();
                flatten(&hit["_source"], "", &mut flat);
                egui::Grid::new("doc_fields")
                    .striped(true)
                    .num_columns(3)
                    .show(ui, |ui| {
                        for (key, value) in flat {
                            let text = query::display(value);
                            ui.horizontal(|ui| {
                                if ui
                                    .small_button("+")
                                    .on_hover_text(t("Filter nilai ini"))
                                    .clicked()
                                {
                                    self.filters.push(Filter::is(&key, &text, false));
                                    self.needs_search = true;
                                }
                                if ui
                                    .small_button("−")
                                    .on_hover_text(t("Kecualikan nilai ini"))
                                    .clicked()
                                {
                                    self.filters.push(Filter::is(&key, &text, true));
                                    self.needs_search = true;
                                }
                                let is_col = self.columns.contains(&key);
                                if ui
                                    .small_button(if is_col { "▣" } else { "▢" })
                                    .on_hover_text(t("Tampilkan sebagai kolom"))
                                    .clicked()
                                {
                                    if is_col {
                                        self.columns.retain(|c| c != &key);
                                    } else {
                                        self.columns.push(key.clone());
                                    }
                                }
                            });
                            ui.monospace(&key);
                            ui.add(egui::Label::new(truncate(&text, 2000)).wrap());
                            ui.end_row();
                        }
                    });
            });
    }

    fn search(&mut self, ctx: &egui::Context, conn: &Arc<Conn>, append: bool) {
        let bounds = match self
            .time_field
            .as_ref()
            .map(|_| self.range.bounds())
            .transpose()
        {
            Ok(b) => b,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        let time = self.time_field.as_deref().zip(bounds);
        let query = match query::build(self.mode, &self.query, &self.filters, time) {
            Ok(q) => q,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        let mut body = json!({
            "size": PAGE,
            "track_total_hits": if self.exact_total { json!(true) } else { json!(TOTAL_LIMIT) },
            "query": query,
            "sort": self.sort_clause(),
            "seq_no_primary_term": true,
        });
        if !self.query.trim().is_empty() {
            // DIS-35: penanda di luar teks normal; dirender sebagai latar berwarna.
            body["highlight"] = json!({
                "fields": {"*": {}},
                "pre_tags": [widgets::HL_START.to_string()],
                "post_tags": [widgets::HL_END.to_string()],
                "fragment_size": 200,
                "number_of_fragments": 1,
                "require_field_match": false,
                "max_analyzed_offset": 1_000_000
            });
        }
        if let (Some(tf), Some(b), false) = (&self.time_field, bounds, append) {
            self.interval = query::auto_interval(b.to - b.from, 60);
            body["aggs"] = histogram_agg(tf, self.interval.1, b, self.breakdown.as_deref());
        }
        if append {
            let Some(after) = self.hits.last().and_then(|h| h.get("sort")).cloned() else {
                return;
            };
            body["search_after"] = after;
        } else {
            if let Some(old) = self.pit.take() {
                close_pit(conn.clone(), old);
            }
            self.selected = None;
        }
        self.last_body = Some(body.clone());
        self.last_search = Some(Instant::now());
        self.error = None;
        self.search_cancel.store(true, Ordering::Relaxed);
        self.search_cancel = Arc::new(AtomicBool::new(false));
        let asynchronous = !append && bounds.is_some_and(|b| b.to - b.from > ASYNC_SPAN_MS);
        self.searching_async = asynchronous;
        let request = Request {
            pattern: self.pattern.clone(),
            body,
            pit: if append { self.pit.clone() } else { None },
            append,
            asynchronous,
            cancel: self.search_cancel.clone(),
        };
        let conn = conn.clone();
        self.pending = Some(Pending::spawn(ctx, move || run_search(&conn, request)));
    }

    fn doc_edit_ui(&mut self, ui: &mut egui::Ui, hit: &Value) {
        let Some((row, text)) = &mut self.doc_edit else {
            return;
        };
        if Some(*row) != self.selected {
            self.doc_edit = None;
            return;
        }
        let row = *row;
        ui.label(RichText::new("Edit _source").strong());
        ui.add(
            egui::TextEdit::multiline(text)
                .code_editor()
                .desired_rows(10)
                .desired_width(f32::INFINITY),
        );
        let parsed = serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object);
        if parsed.is_none() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                t("_source harus objek JSON valid."),
            );
        }
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let label = if self.prod {
                t("Simpan (PROD)")
            } else {
                t("Simpan")
            };
            save = ui
                .add_enabled(parsed.is_some(), egui::Button::new(label))
                .on_hover_text(t(
                    "PUT _doc/<id> dengan if_seq_no/if_primary_term (gagal bila dokumen berubah sejak dimuat)",
                ))
                .clicked();
            cancel = ui.button(t("Batal")).clicked();
        });
        ui.separator();
        if cancel {
            self.doc_edit = None;
        }
        if let (true, Some(source), Some(conn)) = (save, parsed, self.conn.clone()) {
            let path = doc_path(hit);
            self.doc_job = Some(Pending::spawn(ui.ctx(), move || {
                let result = conn
                    .json("PUT", &path, Some(&source))
                    .map(|v| Some((source, v["_seq_no"].clone(), v["_primary_term"].clone())));
                (row, result)
            }));
        }
    }

    fn doc_delete_ui(&mut self, ctx: &egui::Context, hit: &Value) {
        let Some(row) = self.doc_delete else { return };
        let id = hit["_id"].as_str().unwrap_or_default().to_owned();
        let message = tf(
            "Dokumen {0} di {1} akan dihapus permanen.\n\nRequest:\nDELETE {2}",
            &[&id, &hit["_index"].as_str().unwrap_or("?"), &doc_path(hit)],
        );
        let expected = self.prod.then_some(id.as_str());
        let mut typed = ctx
            .data_mut(|d| d.get_temp::<String>(egui::Id::unique("doc_del_typed")))
            .unwrap_or_default();
        let result = widgets::confirm_dialog(
            ctx,
            t("Hapus dokumen?"),
            &message,
            expected,
            &mut typed,
            true,
        );
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("doc_del_typed"), typed));
        match result {
            widgets::Confirm::Yes => {
                self.doc_delete = None;
                if let Some(conn) = self.conn.clone() {
                    let path = doc_path(hit);
                    self.doc_job = Some(Pending::spawn(ctx, move || {
                        (row, conn.json("DELETE", &path, None).map(|_| None))
                    }));
                }
            }
            widgets::Confirm::No => self.doc_delete = None,
            widgets::Confirm::Pending => {}
        }
    }

    fn load_around(&mut self, ctx: &egui::Context, hit: &Value, tf: &str) {
        let id = hit["_id"].as_str().unwrap_or_default().to_owned();
        let Some(ms) = hit_value(hit, tf).and_then(|v| time_ms(&query::display(v))) else {
            self.around = Some((id, Err(t("Waktu dokumen tidak bisa dibaca.").into())));
            return;
        };
        let query = match query::build(QueryMode::Kql, "", &self.filters, None) {
            Ok(q) => q,
            Err(err) => {
                self.around = Some((id, Err(err)));
                return;
            }
        };
        let side = |order: &str| {
            json!({"size": 5, "query": query, "search_after": [ms],
                "sort": [{tf: {"order": order, "numeric_type": "date", "unmapped_type": "boolean"}}]})
        };
        let (newer, older) = (side("asc"), side("desc"));
        let Some(conn) = self.conn.clone() else {
            return;
        };
        let path = format!("/{}/_search", self.pattern);
        self.around_pending = Some(Pending::spawn(ctx, move || {
            let fetch = |body: &Value| {
                conn.json("POST", &path, Some(body))
                    .map(|v| v["hits"]["hits"].as_array().cloned().unwrap_or_default())
            };
            let result = fetch(&newer).and_then(|n| fetch(&older).map(|o| (n, o)));
            (id, result)
        }));
    }

    fn around_ui(
        &self,
        ui: &mut egui::Ui,
        anchor: &Value,
        result: &Result<(Vec<Value>, Vec<Value>), String>,
    ) {
        let tf = self.time_field.clone().unwrap_or_default();
        let line = |ui: &mut egui::Ui, hit: &Value, strong: bool| {
            let time = hit_value(hit, &tf).map(query::display).unwrap_or_default();
            let text = format!(
                "{}  {}",
                format_hit_time(&time),
                truncate(&summary(hit), 160)
            );
            let text = widgets::strip_highlight(&text);
            if strong {
                ui.label(RichText::new(text).strong());
            } else {
                ui.label(RichText::new(text).weak());
            }
        };
        match result {
            Ok((newer, older)) => {
                ui.label(RichText::new(t("Dokumen sekitar")).strong());
                for hit in newer.iter().rev() {
                    line(ui, hit, false);
                }
                line(ui, anchor, true);
                for hit in older {
                    line(ui, hit, false);
                }
            }
            Err(err) => widgets::error_box(ui, err),
        }
    }

    fn esql_search(&mut self, ctx: &egui::Context, conn: &Arc<Conn>) {
        let bounds = match self
            .time_field
            .as_ref()
            .map(|_| self.range.bounds())
            .transpose()
        {
            Ok(b) => b,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        // Waktu dan filter pills diterapkan lewat parameter `filter` (Query DSL).
        let filter = match query::build(
            QueryMode::Kql,
            "",
            &self.filters,
            self.time_field.as_deref().zip(bounds),
        ) {
            Ok(f) => f,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };
        let body = json!({"query": self.query.trim(), "filter": filter});
        self.last_body = None;
        self.last_search = Some(Instant::now());
        self.error = None;
        let conn = conn.clone();
        self.esql_pending = Some(Pending::spawn(ctx, move || {
            let started = Instant::now();
            let value = conn.json("POST", "/_query?format=json", Some(&body))?;
            Ok(EsqlTable {
                columns: value["columns"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        (
                            c["name"].as_str().unwrap_or_default().to_owned(),
                            c["type"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect(),
                rows: value["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| r.as_array().cloned().unwrap_or_default())
                    .collect(),
                took: started.elapsed().as_millis(),
            })
        }));
    }

    fn esql_table(&mut self, ui: &mut egui::Ui) {
        if self.esql_pending.is_some() {
            ui.spinner();
        }
        let Some(table) = &self.esql_result else {
            ui.weak(t("Tulis query ES|QL lalu klik Cari."));
            return;
        };
        ui.weak(tf(
            "{0} baris · {1} kolom · {2} ms",
            &[&table.rows.len(), &table.columns.len(), &table.took],
        ));
        let mut builder = TableBuilder::new(ui)
            .id_salt("esql_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for _ in &table.columns {
            builder = builder.column(Column::initial(160.0).clip(true).at_least(40.0));
        }
        builder = builder.column(Column::remainder());
        builder
            .header(22.0, |mut h| {
                for (name, kind) in &table.columns {
                    h.col(|ui| {
                        ui.strong(name).on_hover_text(kind);
                    });
                }
                h.col(|_| {});
            })
            .body(|body| {
                body.rows(20.0, table.rows.len(), |mut row| {
                    let values = &table.rows[row.index()];
                    for i in 0..table.columns.len() {
                        row.col(|ui| {
                            let text = values.get(i).map(query::display).unwrap_or_default();
                            ui.label(truncate(&text, 300)).on_hover_text(&text);
                        });
                    }
                    row.col(|_| {});
                });
            });
    }

    fn sort_clause(&self) -> Value {
        let mut sort = Vec::new();
        if let Some((field, desc)) = &self.sort {
            sort.push(json!({field: {"order": if *desc { "desc" } else { "asc" }, "unmapped_type": "boolean"}}));
        }
        if let Some(tf) = &self.time_field
            && self.sort.as_ref().is_none_or(|(f, _)| f != tf)
        {
            sort.push(json!({tf: {"order": "desc", "unmapped_type": "boolean"}}));
        }
        if sort.is_empty() {
            sort.push(json!("_score"));
        }
        Value::Array(sort)
    }

    fn apply(&mut self, result: Result<Page, String>) {
        match result {
            Ok(page) => {
                if page.append {
                    self.hits.extend(page.hits);
                } else {
                    self.hits = page.hits;
                    self.buckets = page.buckets;
                    self.series = page.series;
                    self.total = page.total;
                }
                self.pit = page.pit;
                self.took = page.took;
                self.warning = (page.failed_shards > 0).then(|| {
                    tf(
                        "{0} shard gagal: hasil bisa tidak lengkap.",
                        &[&page.failed_shards],
                    )
                });
            }
            Err(err) => self.error = Some(err),
        }
    }
}

fn export_csv(
    conn: &Conn,
    pattern: &str,
    mut body: Value,
    columns: &[String],
    path: &std::path::Path,
    rows: &AtomicUsize,
    cancel: &AtomicBool,
) -> Result<usize, String> {
    let file = std::fs::File::create(path).map_err(|e| tf("Gagal membuat file: {0}", &[&e]))?;
    let mut out = std::io::BufWriter::new(file);
    let header: Vec<String> = columns.iter().map(|c| widgets::csv_cell(c)).collect();
    writeln!(out, "{}", header.join(",")).map_err(|e| e.to_string())?;
    let pit = conn.json(
        "POST",
        &format!("/{pattern}/_pit?keep_alive={PIT_KEEP_ALIVE}"),
        None,
    )?["id"]
        .as_str()
        .ok_or(t("PIT tidak tersedia untuk ekspor."))?
        .to_owned();
    // Ambil hanya field yang diekspor: halaman 1.000 dokumen penuh bisa berukuran MB.
    let source: Vec<&String> = columns.iter().filter(|c| !c.starts_with('_')).collect();
    body["_source"] = json!(source);
    let mut pit_id = pit;
    let mut written = 0;
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Ok(written);
        }
        body["pit"] = json!({"id": pit_id, "keep_alive": PIT_KEEP_ALIVE});
        let value = match conn.json("POST", "/_search", Some(&body)) {
            Ok(v) => v,
            Err(e) => break Err(e),
        };
        if let Some(id) = value["pit_id"].as_str() {
            pit_id = id.to_owned();
        }
        let hits = value["hits"]["hits"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for hit in &hits {
            let line: Vec<String> = columns
                .iter()
                .map(|c| {
                    widgets::csv_cell(&hit_value(hit, c).map(query::display).unwrap_or_default())
                })
                .collect();
            if let Err(e) = writeln!(out, "{}", line.join(",")) {
                return Err(e.to_string());
            }
            written += 1;
            if written >= EXPORT_MAX {
                break;
            }
        }
        rows.store(written, Ordering::Relaxed);
        match hits.last().and_then(|h| h.get("sort")) {
            Some(after) if hits.len() == EXPORT_PAGE && written < EXPORT_MAX => {
                body["search_after"] = after.clone();
            }
            _ => break Ok(written),
        }
    };
    let _ = conn.json("DELETE", "/_pit", Some(&json!({"id": pit_id})));
    out.flush().map_err(|e| e.to_string())?;
    result
}

/// Timestamp dokumen (ISO atau epoch millis) -> epoch millis.
fn time_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|d| d.timestamp_millis())
        .ok()
        .or_else(|| text.parse().ok())
}

fn resolve_pattern(conn: &Conn, pattern: &str) -> Result<Vec<String>, String> {
    let value = conn.get(&format!("/_resolve/index/{pattern}?expand_wildcards=open"))?;
    let mut names = Vec::new();
    for key in ["indices", "aliases", "data_streams"] {
        for item in value[key].as_array().into_iter().flatten() {
            if let Some(name) = item["name"].as_str() {
                names.push(name.to_owned());
            }
        }
    }
    Ok(names)
}

/// DIS-32: field yang dipakai untuk sort. Field `text` memakai sub-field `.keyword` jika ada.
fn sort_target(field: &str, fields: &[Field]) -> Option<String> {
    let find = |name: &str| fields.iter().find(|f| f.name == name);
    if find(field).is_some_and(Field::sortable) {
        return Some(field.to_owned());
    }
    let keyword = format!("{field}.keyword");
    find(&keyword).filter(|f| f.sortable()).map(|_| keyword)
}

fn histogram_agg(field: &str, interval: &str, b: TimeBounds, breakdown: Option<&str>) -> Value {
    let mut agg = json!({"histogram": {"date_histogram": {
        "field": field,
        "fixed_interval": interval,
        "time_zone": query::local_tz(),
        "min_doc_count": 0,
        "extended_bounds": {"min": b.from, "max": b.to}
    }}});
    if let Some(field) = breakdown {
        agg["histogram"]["aggs"] = json!({"b": {"terms": {"field": field, "size": 5}}});
    }
    agg
}

/// DIS-54: jalankan async search dan tunggu selesai; bisa dibatalkan.
const BREAKDOWN_COLORS: [egui::Color32; 5] = [
    egui::Color32::from_rgb(0x54, 0xb3, 0x99),
    egui::Color32::from_rgb(0x61, 0x92, 0xd2),
    egui::Color32::from_rgb(0xd6, 0xbf, 0x57),
    egui::Color32::from_rgb(0xd3, 0x60, 0x86),
    egui::Color32::from_rgb(0x9a, 0x7d, 0xd8),
];

/// Path dokumen dengan kontrol konkurensi optimistik bila tersedia.
fn doc_path(hit: &Value) -> String {
    let index = hit["_index"].as_str().unwrap_or_default();
    let id = hit["_id"].as_str().unwrap_or_default();
    let id = id
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('#', "%23")
        .replace('?', "%3F");
    let mut path = format!("/{index}/_doc/{id}?refresh=wait_for");
    if let (Some(seq), Some(term)) = (hit["_seq_no"].as_u64(), hit["_primary_term"].as_u64()) {
        path.push_str(&format!("&if_seq_no={seq}&if_primary_term={term}"));
    }
    path
}

fn async_search(
    conn: &Conn,
    path: &str,
    body: &Value,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    let wait = "wait_for_completion_timeout=3s";
    let mut value = conn.json("POST", &format!("{path}?{wait}&keep_alive=5m"), Some(body))?;
    while value["is_running"] == json!(true) {
        let id = value["id"]
            .as_str()
            .ok_or(t("async search tanpa id"))?
            .to_owned();
        if cancel.load(Ordering::Relaxed) {
            let _ = conn.json("DELETE", &format!("/_async_search/{id}"), None);
            return Err(t("Pencarian dibatalkan.").into());
        }
        value = conn.get(&format!("/_async_search/{id}?{wait}"))?;
    }
    if let Some(id) = value["id"].as_str() {
        let _ = conn.json("DELETE", &format!("/_async_search/{id}"), None);
    }
    Ok(value["response"].clone())
}

fn run_search(conn: &Conn, request: Request) -> Result<Page, String> {
    let mut body = request.body;
    let mut pit = request.pit;
    if !request.append {
        // PIT menjaga paging konsisten; jika gagal (izin/serverless) tetap cari tanpa PIT.
        let path = format!("/{}/_pit?keep_alive={PIT_KEEP_ALIVE}", request.pattern);
        pit = conn
            .json("POST", &path, None)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_owned));
    }
    let path = match &pit {
        Some(id) => {
            body["pit"] = json!({"id": id, "keep_alive": PIT_KEEP_ALIVE});
            "/_search".to_owned()
        }
        None => format!("/{}/_search", request.pattern),
    };
    let value = if request.asynchronous {
        let path = path.replace("/_search", "/_async_search");
        async_search(conn, &path, &body, &request.cancel)?
    } else {
        conn.json("POST", &path, Some(&body))?
    };
    if request.cancel.load(Ordering::Relaxed) {
        return Err(t("Pencarian digantikan oleh pencarian baru.").into());
    }
    let mut series: Vec<(String, Vec<(f64, f64)>)> = Vec::new();
    for b in value["aggregations"]["histogram"]["buckets"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let x = b["key"].as_f64().unwrap_or_default();
        for t in b["b"]["buckets"].as_array().into_iter().flatten() {
            let key = query::display(&t["key"]);
            let y = t["doc_count"].as_f64().unwrap_or_default();
            match series.iter_mut().find(|(k, _)| *k == key) {
                Some((_, pts)) => pts.push((x, y)),
                None => series.push((key, vec![(x, y)])),
            }
        }
    }
    let hits = value["hits"]["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let total = value["hits"]["total"]
        .get("value")
        .and_then(Value::as_u64)
        .map(|n| (n, value["hits"]["total"]["relation"] == "eq"));
    let buckets = value["aggregations"]["histogram"]["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            (
                b["key"].as_f64().unwrap_or_default(),
                b["doc_count"].as_f64().unwrap_or_default(),
            )
        })
        .collect();
    Ok(Page {
        hits,
        total,
        buckets,
        series,
        pit: value["pit_id"].as_str().map(str::to_owned).or(pit),
        append: request.append,
        failed_shards: value["_shards"]["failed"].as_u64().unwrap_or(0),
        took: value["took"].as_u64().unwrap_or(0),
    })
}

fn close_pit(conn: Arc<Conn>, id: String) {
    std::thread::spawn(move || {
        let _ = conn.json("DELETE", "/_pit", Some(&json!({"id": id})));
    });
}

fn strip_pit(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("pit");
        o.remove("search_after");
    }
    body
}

pub(crate) fn hit_value<'a>(hit: &'a Value, field: &str) -> Option<&'a Value> {
    match field {
        "_id" | "_index" => hit.get(field),
        _ => query::get_path(&hit["_source"], field).or_else(|| {
            hit.get("fields")
                .and_then(|f| f.get(field))
                .map(|v| match v.as_array() {
                    Some(items) if items.len() == 1 => &items[0],
                    _ => v,
                })
        }),
    }
}

fn flatten<'a>(value: &'a Value, prefix: &str, out: &mut Vec<(String, &'a Value)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(v, &key, out);
            }
        }
        other if !prefix.is_empty() => out.push((prefix.to_owned(), other)),
        _ => {}
    }
}

/// Fragmen highlight pertama untuk field, jika ada.
fn highlight_of(hit: &Value, field: &str) -> Option<String> {
    hit["highlight"][field][0].as_str().map(str::to_owned)
}

/// Ringkasan `_source`; field yang cocok dengan query (highlight) tampil lebih dulu.
pub(crate) fn summary(hit: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut shown = Vec::new();
    for (k, v) in hit["highlight"].as_object().into_iter().flatten() {
        if let Some(frag) = v[0].as_str() {
            parts.push(format!("{k}: {frag}"));
            shown.push(k.clone());
        }
    }
    let mut flat = Vec::new();
    flatten(&hit["_source"], "", &mut flat);
    parts.extend(
        flat.iter()
            .filter(|(k, _)| !shown.contains(k))
            .map(|(k, v)| format!("{k}: {}", query::display(v))),
    );
    truncate(&parts.join("  ·  "), 400)
}

fn truncate(text: &str, max: usize) -> String {
    let single = text.replace('\n', " ");
    match single.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &single[..i]),
        None => single,
    }
}

fn type_icon(f: &Field) -> &'static str {
    if f.is_date() {
        "📅"
    } else if f.is_numeric() {
        "#"
    } else {
        match f.es_type() {
            "keyword" | "constant_keyword" | "wildcard" => "k",
            "text" | "match_only_text" => "t",
            "boolean" => "b",
            "ip" => "ip",
            "geo_point" | "geo_shape" => "⌖",
            "nested" | "flattened" => "{}",
            _ => "?",
        }
    }
}

fn short_time(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub(crate) fn iso_local(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Tampilkan timestamp ISO dalam zona lokal jika bisa diparse.
pub(crate) fn format_hit_time(text: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(text) {
        Ok(dt) => widgets::format_time_ms(dt.timestamp_millis()),
        Err(_) => match text.parse::<i64>() {
            Ok(ms) => widgets::format_time_ms(ms),
            Err(_) => text.to_owned(),
        },
    }
}

/// DIS-31: tukar kolom `index` dengan tetangganya (`delta` = -1 kiri, +1 kanan).
/// Mengembalikan `false` tanpa mengubah apa pun bila tujuan di luar batas.
fn move_column(columns: &mut [String], index: usize, delta: isize) -> bool {
    let Some(target) = index.checked_add_signed(delta) else {
        return false;
    };
    if index >= columns.len() || target >= columns.len() {
        return false;
    }
    columns.swap(index, target);
    true
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("Dokumen disimpan.", "Document saved."),
    ("Dokumen dihapus.", "Document deleted."),
    ("Cari", "Search"),
    (
        "Buka request ini di Dev Tools",
        "Open this request in Dev Tools",
    ),
    (
        "↑↓ pilih · Tab terima · Enter cari",
        "↑↓ select · Tab accept · Enter search",
    ),
    ("Sumber data", "Data source"),
    ("Gunakan pola \"{0}\"", "Use pattern \"{0}\""),
    ("Tutup", "Close"),
    (
        "Pilih index, alias, atau data stream.",
        "Choose an index, alias, or data stream.",
    ),
    ("Field waktu", "Time field"),
    ("tanpa waktu", "no time"),
    ("Kolom terpilih", "Selected columns"),
    ("Field ({0})", "Fields ({0})"),
    ("⭐ Pencarian tersimpan ({0})", "⭐ Saved searches ({0})"),
    ("Belum ada pencarian tersimpan.", "No saved searches yet."),
    ("{0} baris diekspor ke {1}", "{0} rows exported to {1}"),
    ("Ekspor {0} baris…", "Exporting {0} rows…"),
    ("Ekspor CSV", "Export CSV"),
    ("Maksimal {0} baris", "Maximum {0} rows"),
    ("Tidak ada index yang cocok.", "No matching indices."),
    ("Cocok: {0} ({1}{2})", "Matches: {0} ({1}{2})"),
    (
        "Konflik tipe antar index: {0}",
        "Type conflict across indices: {0}",
    ),
    ("Hapus kolom", "Remove column"),
    ("Geser kolom ke bawah", "Move column down"),
    ("Geser kolom ke atas", "Move column up"),
    ("Tambah kolom", "Add column"),
    (
        "Top 5 dari {0} dokumen termuat ({1} memiliki field ini)",
        "Top 5 of {0} loaded documents ({1} have this field)",
    ),
    (
        "Tidak ada nilai di dokumen termuat.",
        "No values in the loaded documents.",
    ),
    ("Filter nilai ini", "Filter for this value"),
    ("Kecualikan nilai ini", "Exclude this value"),
    (
        "Hitung akurat (seluruh rentang)",
        "Compute exactly (whole range)",
    ),
    (
        "terms aggregation dengan query dan filter saat ini",
        "terms aggregation with the current query and filters",
    ),
    ("Top 5 akurat:", "Exact top 5:"),
    ("Filter: field ada (exists)", "Filter: field exists"),
    (
        "Pilih sumber data di panel kiri untuk mulai.",
        "Choose a data source in the left panel to start.",
    ),
    ("· {0} termuat · {1} ms", "· {0} loaded · {1} ms"),
    ("hitung total akurat", "exact total count"),
    (
        "track_total_hits: true. Lebih lambat untuk data besar.",
        "track_total_hits: true. Slower on large data.",
    ),
    ("· interval {0} · {1}", "· interval {0} · {1}"),
    (
        "Rentang waktu lebar: query dijalankan sebagai async search di server…",
        "Wide time range: the query runs as an async search on the server…",
    ),
    ("tanpa", "none"),
    ("Geser kolom ke kiri", "Move column left"),
    ("Geser kolom ke kanan", "Move column right"),
    ("Klik untuk sort", "Click to sort"),
    ("Klik untuk sort (memakai {0})", "Click to sort (using {0})"),
    (
        "Field ini tidak bisa di-sort",
        "This field cannot be sorted",
    ),
    ("Dokumen", "Document"),
    ("Muat {0} lagi", "Load {0} more"),
    (
        "Paging lanjutan butuh point-in-time (tidak tersedia).",
        "Further paging needs point-in-time (not available).",
    ),
    ("Tabel", "Table"),
    ("Salin JSON", "Copy JSON"),
    ("Salin _id", "Copy _id"),
    ("Dokumen sekitar", "Surrounding documents"),
    (
        "5 dokumen sebelum dan sesudah berdasarkan waktu (filter aktif tetap berlaku)",
        "5 documents before and after by time (active filters still apply)",
    ),
    ("Tampilkan sebagai kolom", "Show as column"),
    (
        "_source harus objek JSON valid.",
        "_source must be a valid JSON object.",
    ),
    ("Simpan (PROD)", "Save (PROD)"),
    (
        "PUT _doc/<id> dengan if_seq_no/if_primary_term (gagal bila dokumen berubah sejak dimuat)",
        "PUT _doc/<id> with if_seq_no/if_primary_term (fails if the document changed since it was loaded)",
    ),
    (
        "Dokumen {0} di {1} akan dihapus permanen.\n\nRequest:\nDELETE {2}",
        "Document {0} in {1} will be permanently deleted.\n\nRequest:\nDELETE {2}",
    ),
    ("Hapus dokumen?", "Delete document?"),
    (
        "Waktu dokumen tidak bisa dibaca.",
        "The document time could not be read.",
    ),
    (
        "Tulis query ES|QL lalu klik Cari.",
        "Write an ES|QL query, then click Search.",
    ),
    (
        "{0} baris · {1} kolom · {2} ms",
        "{0} rows · {1} columns · {2} ms",
    ),
    (
        "{0} shard gagal: hasil bisa tidak lengkap.",
        "{0} shards failed: results may be incomplete.",
    ),
    ("Gagal membuat file: {0}", "Failed to create file: {0}"),
    (
        "PIT tidak tersedia untuk ekspor.",
        "PIT is not available for export.",
    ),
    ("async search tanpa id", "async search without id"),
    ("Pencarian dibatalkan.", "Search cancelled."),
    (
        "Pencarian digantikan oleh pencarian baru.",
        "Search replaced by a newer search.",
    ),
];

#[cfg(test)]
mod tests {
    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("discover.rs"), super::EN);
    }

    use super::*;

    fn cols(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn move_column_left_and_right() {
        let mut c = cols(&["a", "b", "c"]);
        assert!(move_column(&mut c, 1, -1));
        assert_eq!(c, cols(&["b", "a", "c"]));
        assert!(move_column(&mut c, 1, 1));
        assert_eq!(c, cols(&["b", "c", "a"]));
    }

    #[test]
    fn move_column_edges_and_invalid_index() {
        let mut c = cols(&["a", "b"]);
        assert!(!move_column(&mut c, 0, -1));
        assert!(!move_column(&mut c, 1, 1));
        assert!(!move_column(&mut c, 5, -1));
        assert_eq!(c, cols(&["a", "b"]));
    }

    #[test]
    fn flattens_source_and_reads_hits() {
        let hit =
            json!({"_id": "1", "_source": {"a": {"b": 1}, "c": [1, 2]}, "fields": {"d": ["x"]}});
        let mut flat = Vec::new();
        flatten(&hit["_source"], "", &mut flat);
        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a.b", "c"]);
        assert_eq!(hit_value(&hit, "a.b"), Some(&json!(1)));
        assert_eq!(hit_value(&hit, "d"), Some(&json!("x")));
        assert_eq!(hit_value(&hit, "_id"), Some(&json!("1")));
        assert_eq!(truncate("abcdef", 3), "abc…");
    }

    #[test]
    fn builds_doc_paths() {
        let hit = json!({"_index": "logs", "_id": "a/b", "_seq_no": 5, "_primary_term": 1});
        assert_eq!(
            doc_path(&hit),
            "/logs/_doc/a%2Fb?refresh=wait_for&if_seq_no=5&if_primary_term=1"
        );
        let plain = json!({"_index": "x", "_id": "1"});
        assert_eq!(doc_path(&plain), "/x/_doc/1?refresh=wait_for");
    }

    #[test]
    fn escapes_csv_cells() {
        assert_eq!(widgets::csv_cell("a"), "a");
        assert_eq!(widgets::csv_cell("a,b"), "\"a,b\"");
        assert_eq!(widgets::csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(widgets::csv_cell("x\ny"), "\"x\ny\"");
    }

    #[test]
    fn sorts_text_by_keyword_subfield() {
        let field = |name: &str, t: &str, agg: bool| Field {
            name: name.into(),
            types: vec![t.into()],
            aggregatable: agg,
        };
        let fields = [
            field("title", "text", false),
            field("title.keyword", "keyword", true),
            field("body", "text", false),
            field("code", "long", true),
        ];
        assert_eq!(sort_target("code", &fields).as_deref(), Some("code"));
        assert_eq!(
            sort_target("title", &fields).as_deref(),
            Some("title.keyword")
        );
        assert_eq!(sort_target("body", &fields), None);
    }

    #[test]
    fn parse_stats_full_response() {
        let v = json!({"aggregations": {"st": {"count": 3, "min": 1.0, "max": 5.0, "avg": 3.0, "sum": 9.0}}});
        assert_eq!(
            parse_stats(&v),
            Some(Stats {
                count: 3,
                min: Some(1.0),
                max: Some(5.0),
                avg: Some(3.0),
                sum: 9.0
            })
        );
    }

    #[test]
    fn parse_stats_without_aggregation() {
        assert_eq!(parse_stats(&json!({"aggregations": {"top": {}}})), None);
        assert_eq!(parse_stats(&json!({})), None);
    }

    #[test]
    fn parse_stats_empty_field() {
        let v = json!({"aggregations": {"st": {"count": 0, "min": null, "max": null, "avg": null, "sum": 0.0}}});
        let st = parse_stats(&v).unwrap();
        assert_eq!(st.count, 0);
        assert_eq!(st.min, None);
        assert_eq!(st.max, None);
    }

    #[test]
    fn format_stat_integers_and_fractions() {
        assert_eq!(format_stat(1234567.0), "1.234.567");
        assert_eq!(format_stat(-1500.0), "-1.500");
        assert_eq!(format_stat(2.5), "2.50");
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_numeric_stats() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let Some(f) = fields.iter().find(|f| f.is_numeric() && f.aggregatable) else {
            println!("tidak ada field numeric aggregatable di {pattern}, dilewati");
            return;
        };
        let body = json!({"size": 0, "query": {"match_all": {}}, "aggs": {"st": {"stats": {"field": f.name}}}});
        let v = conn
            .json("POST", &format!("/{pattern}/_search"), Some(&body))
            .unwrap();
        let st = parse_stats(&v).expect("aggregation st");
        println!("{}: {st:?}", f.name);
        if st.count > 0 {
            assert!(st.min.unwrap() <= st.max.unwrap());
        }
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_search_with_pit_and_paging() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let tf = fields
            .iter()
            .find(|f| f.name == "@timestamp" && f.is_date())
            .or_else(|| fields.iter().find(|f| f.is_date()))
            .map(|f| f.name.clone());
        let bounds = TimeRange::new("now-1y", "now").bounds().unwrap();
        let mut d = Discover {
            pattern: pattern.clone(),
            time_field: tf.clone(),
            ..Discover::default()
        };
        d.range = TimeRange::new("now-1y", "now");
        let query =
            query::build(QueryMode::Lucene, "", &[], tf.as_deref().zip(Some(bounds))).unwrap();
        let mut body = json!({"size": PAGE, "track_total_hits": TOTAL_LIMIT, "query": query, "sort": d.sort_clause()});
        if let Some(tf) = &tf {
            body["aggs"] = histogram_agg(
                tf,
                query::auto_interval(bounds.to - bounds.from, 60).1,
                bounds,
                None,
            );
        }
        let page = run_search(
            &conn,
            Request {
                pattern: pattern.clone(),
                body: body.clone(),
                pit: None,
                append: false,
                asynchronous: false,
                cancel: Default::default(),
            },
        )
        .unwrap();
        println!(
            "time field {tf:?}, total {:?}, hits {}, buckets {}, pit {}",
            page.total,
            page.hits.len(),
            page.buckets.len(),
            page.pit.is_some()
        );
        assert!(page.failed_shards == 0);
        if tf.is_some() {
            assert!(!page.buckets.is_empty());
        }
        if page.hits.len() == PAGE && page.pit.is_some() {
            body["search_after"] = page.hits.last().unwrap()["sort"].clone();
            body.as_object_mut().unwrap().remove("aggs");
            let next = run_search(
                &conn,
                Request {
                    pattern,
                    body,
                    pit: page.pit.clone(),
                    append: true,
                    asynchronous: false,
                    cancel: Default::default(),
                },
            )
            .unwrap();
            assert!(!next.hits.is_empty());
            assert_ne!(next.hits[0]["_id"], page.hits[0]["_id"]);
        }
        if let Some(pit) = page.pit {
            let _ = conn.json("DELETE", "/_pit", Some(&json!({"id": pit})));
        }
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_kql_esql_and_csv() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        // KQL: `field:*` (exists) dan negasi harus diterima ES.
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let kw = fields
            .iter()
            .find(|f| f.es_type() == "keyword")
            .unwrap()
            .name
            .clone();
        let q = query::build(
            QueryMode::Kql,
            &format!("{kw}:* and not {kw}:\"zzz-tidak-ada\""),
            &[],
            None,
        )
        .unwrap();
        let v = conn
            .json(
                "POST",
                &format!("/{pattern}/_count"),
                Some(&json!({"query": q})),
            )
            .unwrap();
        println!("KQL count {}", v["count"]);
        assert!(v["count"].as_u64().unwrap() > 0);
        // ES|QL dengan filter DSL.
        let body = json!({"query": format!("FROM {pattern} | STATS c = COUNT(*) BY {kw} | SORT c DESC | LIMIT 3"),
            "filter": {"match_all": {}}});
        let v = conn
            .json("POST", "/_query?format=json", Some(&body))
            .unwrap();
        println!(
            "ES|QL kolom {} baris {}",
            v["columns"].as_array().unwrap().len(),
            v["values"].as_array().unwrap().len()
        );
        assert!(!v["values"].as_array().unwrap().is_empty());
        // CSV: 2.500 baris lewat 3 halaman PIT.
        let path = std::env::temp_dir().join("elastic-gui-live.csv");
        let body = json!({"size": EXPORT_PAGE, "track_total_hits": false, "query": {"match_all": {}}, "sort": ["_doc"]});
        let rows = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let columns = vec!["_id".to_string(), kw];
        let n = export_csv(&conn, &pattern, body, &columns, &path, &rows, &cancel).unwrap();
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        println!("CSV {n} baris, file {lines} baris");
        assert_eq!(lines, n + 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_highlight_around_terms_enum() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let tf = fields.iter().find(|f| f.is_date()).unwrap().name.clone();
        let kw = fields
            .iter()
            .find(|f| f.es_type() == "keyword")
            .unwrap()
            .name
            .clone();
        // _terms_enum untuk saran nilai.
        let v = conn
            .json(
                "POST",
                &format!("/{pattern}/_terms_enum"),
                Some(&json!({"field": kw, "string": "", "size": 3})),
            )
            .unwrap();
        let term = v["terms"][0].as_str().unwrap().to_owned();
        println!("terms_enum {kw}: {}", v["terms"]);
        // Highlight dengan penanda private-use.
        let q = query::build(QueryMode::Kql, &format!("{kw}:\"{term}\""), &[], None).unwrap();
        let body = json!({"size": 1, "query": q, "highlight": {"fields": {"*": {}},
            "pre_tags": [widgets::HL_START.to_string()], "post_tags": [widgets::HL_END.to_string()],
            "fragment_size": 200, "number_of_fragments": 1, "require_field_match": false, "max_analyzed_offset": 1_000_000}});
        let v = conn
            .json("POST", &format!("/{pattern}/_search"), Some(&body))
            .unwrap();
        let hit = &v["hits"]["hits"][0];
        let s = summary(hit);
        println!("summary: {}", truncate(&widgets::strip_highlight(&s), 100));
        assert!(s.contains(widgets::HL_START), "highlight harus ada");
        // Dokumen sekitar.
        let mut d = Discover {
            pattern: pattern.clone(),
            time_field: Some(tf.clone()),
            conn: Some(Arc::new(conn)),
            ..Discover::default()
        };
        let ctx = egui::Context::default();
        d.load_around(&ctx, hit, &tf);
        // Batas waktu: slot kosong tanpa hasil berarti thread gagal.
        let started = Instant::now();
        let (id, result) = loop {
            if let Some(r) = poll(&mut d.around_pending) {
                break r;
            }
            assert!(d.around_pending.is_some(), "thread dokumen sekitar gagal");
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "timeout dokumen sekitar"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let (newer, older) = result.unwrap();
        println!(
            "sekitar {id}: {} lebih baru, {} lebih lama",
            newer.len(),
            older.len()
        );
        assert!(newer.len() + older.len() > 0);
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_async_breakdown() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let tf = fields.iter().find(|f| f.is_date()).unwrap().name.clone();
        let kw = fields
            .iter()
            .find(|f| f.es_type() == "keyword")
            .unwrap()
            .name
            .clone();
        let b = TimeRange::new("now-2y", "now").bounds().unwrap();
        let body = json!({"size": 5, "query": {"match_all": {}}, "seq_no_primary_term": true,
            "aggs": histogram_agg(&tf, "30d", b, Some(&kw))});
        let request = Request {
            pattern,
            body,
            pit: None,
            append: false,
            asynchronous: true,
            cancel: Default::default(),
        };
        let page = run_search(&conn, request).unwrap();
        println!(
            "async: {} hit, {} bucket, {} seri breakdown",
            page.hits.len(),
            page.buckets.len(),
            page.series.len()
        );
        assert!(!page.buckets.is_empty());
        assert!(!page.series.is_empty());
        assert!(
            page.hits[0]["_seq_no"].is_u64(),
            "seq_no untuk edit dokumen"
        );
    }
}
