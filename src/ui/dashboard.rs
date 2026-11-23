//! Dashboard: panel berbasis aggregation dengan konteks global (waktu, query, filter).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_plot::{Bar, BarChart, Legend, Line, Plot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Ctx, Nav};
use crate::es::Conn;
use crate::es::catalog::Field;
use crate::es::query::{self, Filter, QueryMode, TimeBounds};
use crate::task::{Pending, poll};
use crate::ui::discover::{SAVED_FILE, SavedSearch};
use crate::widgets::{self, Confirm, filters, time::TimeRange};

const FILE: &str = "dashboards.json";
const COLUMNS: f32 = 3.0;
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x54, 0xb3, 0x99),
    Color32::from_rgb(0x61, 0x92, 0xd2),
    Color32::from_rgb(0xd6, 0xbf, 0x57),
    Color32::from_rgb(0xd3, 0x60, 0x86),
    Color32::from_rgb(0x9a, 0x7d, 0xd8),
    Color32::from_rgb(0xe7, 0x66, 0x4c),
    Color32::from_rgb(0x60, 0xc0, 0xe0),
    Color32::from_rgb(0xaa, 0x8f, 0x6c),
];

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Kind {
    Metric,
    TimeSeries,
    Bar,
    Pie,
    Table,
    Markdown,
    SavedSearch,
    /// DSH-11: request DSL atau ES|QL yang ditulis user.
    Raw,
    /// DSH-12: dropdown nilai yang menghasilkan filter global.
    Control,
}

impl Kind {
    const ALL: [Kind; 9] = [
        Kind::Metric,
        Kind::TimeSeries,
        Kind::Bar,
        Kind::Pie,
        Kind::Table,
        Kind::Markdown,
        Kind::SavedSearch,
        Kind::Raw,
        Kind::Control,
    ];

    fn label(self) -> &'static str {
        match self {
            Kind::Metric => "Metric",
            Kind::TimeSeries => "Time series",
            Kind::Bar => "Bar kategori",
            Kind::Pie => "Pie / donut",
            Kind::Table => "Tabel",
            Kind::Markdown => "Markdown",
            Kind::SavedSearch => "Pencarian tersimpan",
            Kind::Raw => "Raw DSL / ES|QL",
            Kind::Control => "Kontrol filter",
        }
    }

    fn uses_split(self) -> bool {
        matches!(
            self,
            Kind::TimeSeries | Kind::Bar | Kind::Pie | Kind::Table | Kind::Control
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Cardinality,
    P50,
    P95,
}

impl Agg {
    const ALL: [Agg; 8] = [
        Agg::Count,
        Agg::Sum,
        Agg::Avg,
        Agg::Min,
        Agg::Max,
        Agg::Cardinality,
        Agg::P50,
        Agg::P95,
    ];

    fn label(self) -> &'static str {
        match self {
            Agg::Count => "Count",
            Agg::Sum => "Sum",
            Agg::Avg => "Average",
            Agg::Min => "Min",
            Agg::Max => "Max",
            Agg::Cardinality => "Unique count",
            Agg::P50 => "Median (p50)",
            Agg::P95 => "Persentil 95",
        }
    }

    fn numeric_only(self) -> bool {
        !matches!(self, Agg::Count | Agg::Cardinality)
    }

    fn dsl(self, field: &str) -> Option<Value> {
        Some(match self {
            Agg::Count => return None,
            Agg::Sum => json!({"sum": {"field": field}}),
            Agg::Avg => json!({"avg": {"field": field}}),
            Agg::Min => json!({"min": {"field": field}}),
            Agg::Max => json!({"max": {"field": field}}),
            Agg::Cardinality => json!({"cardinality": {"field": field}}),
            Agg::P50 => json!({"percentiles": {"field": field, "percents": [50]}}),
            Agg::P95 => json!({"percentiles": {"field": field, "percents": [95]}}),
        })
    }

    /// Nilai metric dari bucket (atau root response untuk panel Metric).
    fn read(self, bucket: &Value, count: f64) -> f64 {
        let m = &bucket["m"];
        match self {
            Agg::Count => count,
            Agg::P50 | Agg::P95 => m["values"]
                .as_object()
                .and_then(|o| o.values().next())
                .and_then(Value::as_f64)
                .unwrap_or(f64::NAN),
            _ => m["value"].as_f64().unwrap_or(f64::NAN),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Style {
    Line,
    Area,
    Bar,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Format {
    Number,
    Bytes,
    Millis,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Panel {
    title: String,
    kind: Kind,
    agg: Agg,
    field: String,
    split: String,
    size: u32,
    style: Style,
    format: Format,
    /// Lebar dalam kolom (1–3).
    width: u8,
    height: f32,
    /// Query KQL khusus panel, ditambahkan ke query global.
    query: String,
    markdown: String,
    /// Nama pencarian tersimpan untuk panel `SavedSearch`.
    saved: String,
    /// DSH-11: body `_search` (JSON) atau query ES|QL (diawali `FROM`).
    raw: String,
    /// DSH-11: JSON pointer ke array bucket di response, mis. `/aggregations/top/buckets`.
    raw_path: String,
    /// DSH-19: profile sumber data panel (`None` = cluster aktif).
    profile: Option<String>,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            title: "Panel baru".into(),
            kind: Kind::Metric,
            agg: Agg::Count,
            field: String::new(),
            split: String::new(),
            size: 5,
            style: Style::Line,
            format: Format::Number,
            width: 1,
            height: 220.0,
            query: String::new(),
            markdown: String::new(),
            saved: String::new(),
            raw: String::new(),
            raw_path: String::new(),
            profile: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Dash {
    id: String,
    name: String,
    pattern: String,
    time_field: String,
    range: TimeRange,
    query: String,
    filters: Vec<Filter>,
    panels: Vec<Panel>,
}

impl Default for Dash {
    fn default() -> Self {
        Self {
            id: format!("d{:x}", chrono::Utc::now().timestamp_millis()),
            name: "Dashboard baru".into(),
            pattern: String::new(),
            time_field: String::new(),
            range: TimeRange::new("now-24h", "now"),
            query: String::new(),
            filters: Vec::new(),
            panels: Vec::new(),
        }
    }
}

enum Data {
    Metric(f64),
    /// (nama seri, titik [x, y])
    Series(Vec<(String, Vec<[f64; 2]>)>),
    Terms(Vec<(String, f64)>, f64),
    Error(String),
    /// DSH-10: dokumen terbaru dari pencarian tersimpan (hits, field waktu).
    Docs(Vec<Value>, Option<String>),
    /// DSH-11: response mentah (ditampilkan sebagai tree).
    Json(Value),
    /// DSH-11: hasil ES|QL (kolom, baris).
    Esql(Vec<String>, Vec<Vec<Value>>),
}

/// Request satu panel; `conn` = indeks koneksi (0 = cluster aktif).
enum Req {
    Static(Data),
    Search {
        conn: usize,
        target: String,
        body: Value,
    },
    Esql {
        conn: usize,
        body: Value,
    },
}

/// Interaksi dari panel ke dashboard.
enum Event {
    Filter(Filter),
    /// DSH-14: rentang waktu baru dari brush (epoch millis).
    Zoom(i64, i64),
    /// DSH-12: ganti filter `field: nilai` (None = hapus filter field ini).
    Control(String, Option<String>),
}

#[derive(Default)]
pub struct Dashboard {
    loaded: bool,
    dashes: Vec<Dash>,
    active: Option<usize>,
    search: String,
    edit: bool,
    editing: Option<usize>,
    data: Vec<Option<Data>>,
    pending: Option<Pending<Result<Vec<Data>, String>>>,
    error: Option<String>,
    dirty: bool,
    needs_fetch: bool,
    last_fetch: Option<Instant>,
    refresh_secs: u64,
    filter_draft: filters::Draft,
    renaming: bool,
    confirm_delete: bool,
    confirm_typed: String,
    pattern_input: String,
    interval: (i64, &'static str),
    /// Nama pencarian tersimpan (untuk editor panel).
    saved_names: Vec<String>,
    /// Akumulasi drag horizontal saat resize (DSH-02).
    resize_acc: f32,
}

impl Dashboard {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let ctx = ui.ctx().clone();
        if !self.loaded {
            self.loaded = true;
            self.dashes = cx.store.load(&cx.profile.id, FILE).unwrap_or_default();
        }
        if let Some(result) = poll(&mut self.pending) {
            match result {
                Ok(data) => {
                    self.data = data.into_iter().map(Some).collect();
                    self.error = None;
                }
                Err(err) => self.error = Some(err),
            }
        }
        if self.dirty {
            self.dirty = false;
            cx.store.save(&cx.profile.id, FILE, &self.dashes);
        }

        egui::Panel::left("dash_list")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| self.list_ui(ui));
        let Some(index) = self.active.filter(|i| *i < self.dashes.len()) else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.weak("Pilih atau buat dashboard di panel kiri.");
            });
            return;
        };

        let fields: Vec<Field> = match cx
            .catalog
            .fields(&ctx, cx.conn, &self.dashes[index].pattern)
        {
            Some(Ok(f)) => f.clone(),
            _ => Vec::new(),
        };
        let dash = &mut self.dashes[index];
        if dash.time_field.is_empty()
            && let Some(f) = fields
                .iter()
                .find(|f| f.name == "@timestamp")
                .or_else(|| fields.iter().find(|f| f.is_date()))
        {
            dash.time_field = f.name.clone();
            self.dirty = true;
            self.needs_fetch = true;
        }
        if self.refresh_secs > 0 {
            let every = Duration::from_secs(self.refresh_secs);
            if self.last_fetch.is_none_or(|t| t.elapsed() >= every) {
                self.needs_fetch = true;
            } else {
                ctx.request_repaint_after(every);
            }
        }
        if self.needs_fetch && self.pending.is_none() {
            self.needs_fetch = false;
            self.fetch(&ctx, cx, index);
        }

        self.context_bar(ui, cx, index, &fields);
        if let Some(p) = self
            .editing
            .filter(|p| *p < self.dashes[index].panels.len())
        {
            egui::Panel::right("panel_editor")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| self.editor_ui(ui, index, p, &fields, cx));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.error {
                widgets::error_box(ui, err);
            }
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| self.grid(ui, cx, index));
        });
        self.delete_dialog(&ctx);
    }

    fn list_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dashboard");
        ui.horizontal(|ui| {
            if ui.button("+ Baru").clicked() {
                self.dashes.push(Dash::default());
                self.open(self.dashes.len() - 1);
                self.edit = true;
                self.dirty = true;
            }
            if ui
                .button("Impor…")
                .on_hover_text("Impor file JSON dashboard")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .pick_file()
            {
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| serde_json::from_str::<Dash>(&t).map_err(|e| e.to_string()))
                {
                    Ok(mut dash) => {
                        dash.id = Dash::default().id;
                        self.dashes.push(dash);
                        self.open(self.dashes.len() - 1);
                        self.dirty = true;
                    }
                    Err(err) => self.error = Some(format!("Gagal impor: {err}")),
                }
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Cari…"));
        ui.separator();
        let needle = self.search.to_lowercase();
        let mut pick = None;
        for (i, d) in self.dashes.iter().enumerate() {
            if !needle.is_empty() && !d.name.to_lowercase().contains(&needle) {
                continue;
            }
            if ui
                .selectable_label(self.active == Some(i), &d.name)
                .clicked()
            {
                pick = Some(i);
            }
        }
        if let Some(i) = pick {
            self.open(i);
        }
    }

    fn open(&mut self, index: usize) {
        self.active = Some(index);
        self.data.clear();
        self.editing = None;
        self.renaming = false;
        self.needs_fetch = true;
        self.pattern_input = self.dashes[index].pattern.clone();
    }

    fn context_bar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, index: usize, fields: &[Field]) {
        let mut changed = false;
        let mut duplicate = None;
        let dash = &mut self.dashes[index];
        ui.horizontal(|ui| {
            if self.renaming {
                let r = ui.add(egui::TextEdit::singleline(&mut dash.name).desired_width(200.0));
                if r.lost_focus() {
                    self.renaming = false;
                    self.dirty = true;
                }
            } else if ui
                .heading(&dash.name)
                .on_hover_text("Klik dua kali untuk ganti nama")
                .double_clicked()
            {
                self.renaming = true;
            }
            ui.separator();
            ui.label("Index");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.pattern_input)
                    .desired_width(180.0)
                    .hint_text("logs-*"),
            );
            if r.lost_focus() && self.pattern_input.trim() != dash.pattern {
                dash.pattern = self.pattern_input.trim().to_owned();
                dash.time_field.clear();
                changed = true;
            }
            let dates: Vec<&Field> = fields.iter().filter(|f| f.is_date()).collect();
            egui::ComboBox::from_id_salt("dash_time_field")
                .selected_text(if dash.time_field.is_empty() {
                    "tanpa waktu"
                } else {
                    dash.time_field.as_str()
                })
                .show_ui(ui, |ui| {
                    for f in dates {
                        if ui
                            .selectable_value(&mut dash.time_field, f.name.clone(), &f.name)
                            .clicked()
                        {
                            changed = true;
                        }
                    }
                });
            if widgets::time::time_picker(ui, &mut dash.range, &mut self.refresh_secs) {
                changed = true;
            }
            ui.toggle_value(&mut self.edit, "✎ Edit");
            if self.pending.is_some() {
                ui.spinner();
            } else if ui.button("⟳").on_hover_text("Muat ulang").clicked() {
                changed = true;
            }
            ui.menu_button("⋯", |ui| {
                if ui.button("Duplikat").clicked() {
                    let mut copy = dash.clone();
                    copy.id = Dash::default().id;
                    copy.name = format!("{} (salinan)", copy.name);
                    duplicate = Some(copy);
                    ui.close();
                }
                if ui.button("Ekspor…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name(format!("{}.json", dash.name))
                        .save_file()
                    {
                        let text = serde_json::to_string_pretty(&*dash).unwrap_or_default();
                        if let Err(err) = std::fs::write(path, text) {
                            self.error = Some(format!("Gagal ekspor: {err}"));
                        }
                    }
                    ui.close();
                }
                if ui.button("Hapus dashboard").clicked() {
                    self.confirm_delete = true;
                    self.confirm_typed.clear();
                    ui.close();
                }
            });
        });
        if let Some(copy) = duplicate {
            self.dashes.push(copy);
            self.dirty = true;
        }
        let dash = &mut self.dashes[index];
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut dash.query)
                    .desired_width((ui.available_width() - 80.0).max(200.0))
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Query KQL global, mis. status:500 and not level:debug"),
            );
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("Terapkan").clicked()
            {
                changed = true;
            }
        });
        if filters::filter_bar(ui, &mut dash.filters, fields, &mut self.filter_draft) {
            changed = true;
        }
        if self.edit && ui.button("+ Tambah panel").clicked() {
            dash.panels.push(Panel::default());
            self.editing = Some(dash.panels.len() - 1);
            changed = true;
        }
        ui.separator();
        if changed {
            self.dirty = true;
            self.needs_fetch = true;
            cx.catalog.refresh_fields(&self.dashes[index].pattern);
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, index: usize) {
        let dash = &self.dashes[index];
        if dash.pattern.is_empty() {
            ui.weak("Isi pola index di atas untuk mulai.");
            return;
        }
        if dash.panels.is_empty() {
            ui.weak("Belum ada panel. Aktifkan ✎ Edit lalu klik + Tambah panel.");
            return;
        }
        let total = ui.available_width();
        let gap = 8.0;
        let unit = (total - gap * (COLUMNS - 1.0)) / COLUMNS;
        // Susun panel per baris berdasarkan lebar kolom.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut used = 0u8;
        for (i, p) in dash.panels.iter().enumerate() {
            let w = p.width.clamp(1, 3);
            if used + w > 3 {
                rows.push(Vec::new());
                used = 0;
            }
            rows.last_mut().unwrap().push(i);
            used += w;
        }
        let mut action: Option<(usize, &str)> = None;
        let mut event: Option<Event> = None;
        // DSH-02: pindah panel (dari, ke) dan resize (panel, delta).
        let mut moved: Option<(usize, usize)> = None;
        let mut resize: Option<(usize, egui::Vec2, bool)> = None;
        for row in rows {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for i in row {
                    let dash = &self.dashes[index];
                    let panel = &dash.panels[i];
                    let w = panel.width.clamp(1, 3) as f32;
                    let width = unit * w + gap * (w - 1.0);
                    let frame = egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(width - 14.0);
                        ui.set_height(panel.height);
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                if self.edit {
                                    ui.dnd_drag_source(
                                        egui::Id::unique(("panel_drag", index, i)),
                                        i,
                                        |ui| {
                                            ui.strong(format!("⠿ {}", panel.title));
                                        },
                                    )
                                    .response
                                    .on_hover_text("Drag ke panel lain untuk memindahkan");
                                } else {
                                    ui.strong(&panel.title);
                                }
                                if let Some(pid) = &panel.profile
                                    && let Some(p) = cx.profiles.iter().find(|p| &p.id == pid)
                                {
                                    ui.weak(format!("@{}", p.name));
                                }
                                if self.edit {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            for (icon, name, tip) in [
                                                ("🗑", "delete", "Hapus"),
                                                ("✎", "edit", "Edit panel"),
                                            ] {
                                                if ui
                                                    .small_button(icon)
                                                    .on_hover_text(tip)
                                                    .clicked()
                                                {
                                                    action = Some((i, name));
                                                }
                                            }
                                        },
                                    );
                                } else if !matches!(panel.kind, Kind::Markdown | Kind::Control) {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button("⌕")
                                                .on_hover_text("Inspect: buka request di Dev Tools")
                                                .clicked()
                                            {
                                                action = Some((i, "inspect"));
                                            }
                                        },
                                    );
                                }
                            });
                            match self.data.get(i).and_then(Option::as_ref) {
                                _ if panel.kind == Kind::Markdown => {
                                    egui::ScrollArea::vertical().id_salt(("md", i)).show(
                                        ui,
                                        |ui| {
                                            ui.label(&panel.markdown);
                                        },
                                    );
                                }
                                None => {
                                    ui.centered_and_justified(|ui| ui.spinner());
                                }
                                Some(Data::Error(err)) => widgets::error_box(ui, err),
                                Some(Data::Terms(list, _)) if panel.kind == Kind::Control => {
                                    // DSH-12: nilai terpilih = filter `is` aktif untuk field ini.
                                    let current = dash
                                        .filters
                                        .iter()
                                        .find(|f| {
                                            f.enabled
                                                && !f.negate
                                                && f.op == query::FilterOp::Is
                                                && f.field == panel.split
                                        })
                                        .map(|f| f.value.clone());
                                    let mut chosen = current.clone();
                                    egui::ComboBox::from_id_salt(("control", index, i))
                                        .width((width - 40.0).max(120.0))
                                        .selected_text(
                                            chosen.clone().unwrap_or_else(|| {
                                                format!("Semua {}", panel.split)
                                            }),
                                        )
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut chosen, None, "Semua");
                                            for (k, v) in list {
                                                ui.selectable_value(
                                                    &mut chosen,
                                                    Some(k.clone()),
                                                    format!("{k} ({})", fmt(Format::Number, *v)),
                                                );
                                            }
                                        });
                                    if chosen != current {
                                        event = Some(Event::Control(panel.split.clone(), chosen));
                                    }
                                }
                                Some(data) => {
                                    if let Some(e) = render(ui, i, panel, data) {
                                        event = Some(e);
                                    }
                                }
                            }
                        });
                        if self.edit {
                            let r = ui.max_rect();
                            let handle = egui::Rect::from_min_size(
                                r.right_bottom() - egui::vec2(14.0, 14.0),
                                egui::vec2(14.0, 14.0),
                            );
                            let resp = ui
                                .interact(
                                    handle,
                                    egui::Id::unique(("panel_resize", index, i)),
                                    egui::Sense::drag(),
                                )
                                .on_hover_cursor(egui::CursorIcon::ResizeNwSe)
                                .on_hover_text("Drag untuk ubah ukuran");
                            ui.painter().text(
                                handle.center(),
                                egui::Align2::CENTER_CENTER,
                                "◢",
                                egui::FontId::proportional(12.0),
                                ui.visuals().weak_text_color(),
                            );
                            if resp.dragged() || resp.drag_stopped() {
                                resize = Some((i, resp.drag_delta(), resp.drag_stopped()));
                            }
                        }
                    });
                    if self.edit {
                        let r = &frame.response;
                        if r.dnd_hover_payload::<usize>()
                            .is_some_and(|from| *from != i)
                        {
                            ui.painter().rect_stroke(
                                r.rect,
                                4.0,
                                ui.visuals().selection.stroke,
                                egui::StrokeKind::Outside,
                            );
                        }
                        if let Some(from) = r.dnd_release_payload::<usize>()
                            && *from != i
                        {
                            moved = Some((*from, i));
                        }
                    }
                }
            });
            ui.add_space(gap);
        }
        if let Some((from, to)) = moved {
            let dash = &mut self.dashes[index];
            let panel = dash.panels.remove(from);
            dash.panels.insert(to, panel);
            if from < self.data.len() && to < self.data.len() {
                let d = self.data.remove(from);
                self.data.insert(to, d);
            }
            self.editing = None;
            self.dirty = true;
        }
        if let Some((i, delta, stopped)) = resize {
            let panel = &mut self.dashes[index].panels[i];
            panel.height = (panel.height + delta.y).clamp(120.0, 900.0);
            self.resize_acc += delta.x;
            // Lebar berpindah per kolom saat drag melewati setengah kolom.
            if self.resize_acc > unit * 0.5 && panel.width < 3 {
                panel.width += 1;
                self.resize_acc = 0.0;
            } else if self.resize_acc < -unit * 0.5 && panel.width > 1 {
                panel.width -= 1;
                self.resize_acc = 0.0;
            }
            if stopped {
                self.resize_acc = 0.0;
                self.dirty = true;
            }
        }
        let dash = &mut self.dashes[index];
        if let Some(Event::Zoom(from, to)) = event {
            dash.range = TimeRange::new(
                &crate::ui::discover::iso_local(from),
                &crate::ui::discover::iso_local(to),
            );
            self.dirty = true;
            self.needs_fetch = true;
        } else if let Some(Event::Control(field, value)) = event {
            dash.filters
                .retain(|f| !(f.field == field && f.op == query::FilterOp::Is && !f.negate));
            if let Some(v) = value {
                dash.filters.push(Filter::is(&field, &v, false));
            }
            self.dirty = true;
            self.needs_fetch = true;
        } else if let Some(Event::Filter(filter)) = event {
            dash.filters.push(filter);
            self.dirty = true;
            self.needs_fetch = true;
        }
        if let Some((i, name)) = action {
            match name {
                "delete" => {
                    dash.panels.remove(i);
                    self.editing = None;
                    self.needs_fetch = true;
                }
                "edit" => self.editing = Some(i),
                "inspect" => {
                    let bounds = dash.range.bounds().ok();
                    let panel = &dash.panels[i];
                    let request = if panel.kind == Kind::Raw {
                        raw_request(dash, panel, bounds).map(|r| match r {
                            Ok(body) => (format!("/{}/_search", dash.pattern), body),
                            Err(body) => ("/_query?format=txt".to_owned(), body),
                        })
                    } else {
                        panel_body(dash, panel, bounds, self.interval.1)
                            .map(|body| (format!("/{}/_search", dash.pattern), body))
                    };
                    if let Ok((path, body)) = request {
                        *cx.nav = Some(Nav::Console {
                            method: "POST".into(),
                            path,
                            body: Some(body),
                        });
                    }
                }
                _ => {}
            }
            self.dirty = true;
        }
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, index: usize, p: usize, fields: &[Field], cx: &Ctx) {
        let before = serde_json::to_string(&self.dashes[index].panels[p]).unwrap_or_default();
        let panel = &mut self.dashes[index].panels[p];
        ui.horizontal(|ui| {
            ui.heading("Edit panel");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    self.editing = None;
                }
            });
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("panel_form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Judul");
                    ui.text_edit_singleline(&mut panel.title);
                    ui.end_row();
                    ui.label("Tipe");
                    egui::ComboBox::from_id_salt("panel_kind")
                        .selected_text(panel.kind.label())
                        .show_ui(ui, |ui| {
                            for k in Kind::ALL {
                                ui.selectable_value(&mut panel.kind, k, k.label());
                            }
                        });
                    ui.end_row();
                    if panel.kind == Kind::Markdown {
                        ui.label("Teks");
                        ui.add(egui::TextEdit::multiline(&mut panel.markdown).desired_rows(8));
                        ui.end_row();
                    } else if panel.kind == Kind::Raw {
                        ui.label("Request");
                        ui.add(
                            egui::TextEdit::multiline(&mut panel.raw)
                                .code_editor()
                                .desired_rows(10)
                                .desired_width(260.0)
                                .hint_text("{\"size\": 0, \"aggs\": {...}}  atau  FROM logs-* | STATS ..."),
                        );
                        ui.end_row();
                        ui.label("Path bucket");
                        ui.add(
                            egui::TextEdit::singleline(&mut panel.raw_path)
                                .hint_text("/aggregations/NAMA/buckets (opsional)"),
                        )
                        .on_hover_text("JSON pointer ke array bucket; jika diisi, hasil tampil sebagai bar. Kosong = tree response.");
                        ui.end_row();
                        ui.label("");
                        ui.weak("Waktu, query, dan filter dashboard ditambahkan sebagai filter.");
                        ui.end_row();
                    } else if panel.kind == Kind::Control {
                        ui.label("Field");
                        field_combo(ui, "panel_control", &mut panel.split, fields, |f| f.aggregatable && !f.is_date());
                        ui.end_row();
                        ui.label("Jumlah opsi");
                        ui.add(egui::DragValue::new(&mut panel.size).range(5..=200));
                        ui.end_row();
                    } else if panel.kind == Kind::SavedSearch {
                        ui.label("Pencarian");
                        egui::ComboBox::from_id_salt("panel_saved")
                            .selected_text(if panel.saved.is_empty() {
                                "-"
                            } else {
                                panel.saved.as_str()
                            })
                            .show_ui(ui, |ui| {
                                for name in &self.saved_names {
                                    ui.selectable_value(&mut panel.saved, name.clone(), name);
                                }
                            });
                        ui.end_row();
                        if self.saved_names.is_empty() {
                            ui.label("");
                            ui.weak("Belum ada. Simpan pencarian dari Discover (⭐).");
                            ui.end_row();
                        }
                    } else {
                        ui.label("Metric");
                        egui::ComboBox::from_id_salt("panel_agg")
                            .selected_text(panel.agg.label())
                            .show_ui(ui, |ui| {
                                for a in Agg::ALL {
                                    ui.selectable_value(&mut panel.agg, a, a.label());
                                }
                            });
                        ui.end_row();
                        if panel.agg != Agg::Count {
                            ui.label("Field");
                            let numeric = panel.agg.numeric_only();
                            field_combo(ui, "panel_field", &mut panel.field, fields, |f| {
                                f.aggregatable && (!numeric || f.is_numeric())
                            });
                            ui.end_row();
                        }
                        if panel.kind.uses_split() {
                            ui.label(if panel.kind == Kind::TimeSeries {
                                "Pisah per (opsional)"
                            } else {
                                "Kelompok per"
                            });
                            field_combo(ui, "panel_split", &mut panel.split, fields, |f| {
                                f.aggregatable && !f.is_date()
                            });
                            ui.end_row();
                            ui.label("Top N");
                            ui.add(egui::DragValue::new(&mut panel.size).range(1..=50));
                            ui.end_row();
                        }
                        if panel.kind == Kind::TimeSeries {
                            ui.label("Gaya");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut panel.style, Style::Line, "Line");
                                ui.selectable_value(&mut panel.style, Style::Area, "Area");
                                ui.selectable_value(&mut panel.style, Style::Bar, "Bar");
                            });
                            ui.end_row();
                        }
                        ui.label("Format");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut panel.format, Format::Number, "Angka");
                            ui.selectable_value(&mut panel.format, Format::Bytes, "Bytes");
                            ui.selectable_value(&mut panel.format, Format::Millis, "Durasi (ms)");
                        });
                        ui.end_row();
                        ui.label("Query panel");
                        ui.add(
                            egui::TextEdit::singleline(&mut panel.query).hint_text("KQL, opsional"),
                        );
                        ui.end_row();
                    }
                    if panel.kind != Kind::Markdown {
                        // DSH-19: panel bisa mengambil data dari cluster lain.
                        ui.label("Cluster");
                        let label = panel
                            .profile
                            .as_ref()
                            .and_then(|id| cx.profiles.iter().find(|p| &p.id == id))
                            .map_or_else(|| format!("{} (aktif)", cx.profile.name), |p| p.name.clone());
                        egui::ComboBox::from_id_salt("panel_profile")
                            .selected_text(label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut panel.profile, None, format!("{} (aktif)", cx.profile.name));
                                for pr in cx.profiles.iter().filter(|pr| pr.id != cx.profile.id) {
                                    ui.selectable_value(&mut panel.profile, Some(pr.id.clone()), format!("{} · {}", pr.name, pr.env.label()));
                                }
                            });
                        ui.end_row();
                    }
                    ui.label("Lebar");
                    ui.horizontal(|ui| {
                        for w in 1..=3u8 {
                            ui.selectable_value(&mut panel.width, w, format!("{w}/3"));
                        }
                    });
                    ui.end_row();
                    ui.label("Tinggi");
                    ui.add(egui::Slider::new(&mut panel.height, 120.0..=600.0));
                    ui.end_row();
                });
            if panel.kind.uses_split() && panel.kind != Kind::TimeSeries && panel.split.is_empty() {
                widgets::warn_box(ui, "Pilih field \"Kelompok per\".");
            }
        });
        let after = serde_json::to_string(&self.dashes[index].panels[p]).unwrap_or_default();
        if before != after {
            self.dirty = true;
            // Ubah judul/ukuran tidak perlu query ulang; sisanya diterapkan langsung sebagai preview.
            let strip = |s: &str| {
                let mut v: Value = serde_json::from_str(s).unwrap_or_default();
                for k in ["title", "width", "height", "markdown", "format"] {
                    v.as_object_mut().map(|o| o.remove(k));
                }
                v
            };
            if strip(&before) != strip(&after) {
                self.needs_fetch = true;
            }
        }
    }

    fn fetch(&mut self, ctx: &egui::Context, cx: &Ctx, index: usize) {
        let saved: Vec<SavedSearch> = cx
            .store
            .load(&cx.profile.id, SAVED_FILE)
            .unwrap_or_default();
        self.saved_names = saved.iter().map(|s| s.name.clone()).collect();
        let dash = self.dashes[index].clone();
        if dash.pattern.is_empty() || dash.panels.is_empty() {
            return;
        }
        let bounds = if dash.time_field.is_empty() {
            None
        } else {
            match dash.range.bounds() {
                Ok(b) => Some(b),
                Err(err) => {
                    self.error = Some(err);
                    return;
                }
            }
        };
        self.interval = bounds.map_or((3_600_000, "1h"), |b| {
            query::auto_interval(b.to - b.from, 60)
        });
        // DSH-19: koneksi per profile sumber panel; indeks 0 = cluster aktif.
        let mut conns: Vec<(Option<String>, Arc<Conn>)> = vec![(None, cx.conn.clone())];
        let mut reqs: Vec<Req> = Vec::new();
        for panel in &dash.panels {
            let conn = match panel.profile.as_ref().filter(|id| **id != cx.profile.id) {
                None => Ok(0),
                Some(id) => match conns.iter().position(|(p, _)| p.as_ref() == Some(id)) {
                    Some(i) => Ok(i),
                    None => cx
                        .profiles
                        .iter()
                        .find(|p| &p.id == id)
                        .ok_or_else(|| "Profile sumber panel sudah dihapus.".to_string())
                        .and_then(|p| Conn::new(p, crate::profile::secret::get(id).as_deref()))
                        .map(|c| {
                            conns.push((Some(id.clone()), Arc::new(c)));
                            conns.len() - 1
                        }),
                },
            };
            let req = match (panel.kind, conn) {
                (Kind::Markdown, _) => Req::Static(Data::Metric(0.0)),
                (_, Err(e)) => Req::Static(Data::Error(e)),
                (Kind::SavedSearch, Ok(conn)) => {
                    match saved_body(&saved, panel, dash.range.bounds().ok()) {
                        Ok((target, body)) => Req::Search { conn, target, body },
                        Err(e) => Req::Static(Data::Error(e)),
                    }
                }
                (Kind::Raw, Ok(conn)) => match raw_request(&dash, panel, bounds) {
                    Ok(Ok(body)) => Req::Search {
                        conn,
                        target: dash.pattern.clone(),
                        body,
                    },
                    Ok(Err(body)) => Req::Esql { conn, body },
                    Err(e) => Req::Static(Data::Error(e)),
                },
                (_, Ok(conn)) => match panel_body(&dash, panel, bounds, self.interval.1) {
                    Ok(body) => Req::Search {
                        conn,
                        target: dash.pattern.clone(),
                        body,
                    },
                    Err(e) => Req::Static(Data::Error(e)),
                },
            };
            reqs.push(req);
        }
        self.last_fetch = Some(Instant::now());
        let panels = dash.panels.clone();
        let saved_names: Vec<(String, Option<String>)> = saved
            .iter()
            .map(|s| (s.name.clone(), s.time_field.clone()))
            .collect();
        let conns: Vec<Arc<Conn>> = conns.into_iter().map(|(_, c)| c).collect();
        self.pending = Some(Pending::spawn(ctx, move || {
            let mut out: Vec<Option<Data>> = (0..reqs.len()).map(|_| None).collect();
            // Satu `_msearch` per koneksi.
            for (ci, conn) in conns.iter().enumerate() {
                let batch: Vec<(usize, &String, &Value)> = reqs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| match r {
                        Req::Search { conn, target, body } if *conn == ci => {
                            Some((i, target, body))
                        }
                        _ => None,
                    })
                    .collect();
                if batch.is_empty() {
                    continue;
                }
                let mut lines = String::new();
                for (_, target, body) in &batch {
                    lines.push_str(&format!("{}\n{}\n", json!({"index": target}), body));
                }
                let responses = conn
                    .send("POST", "/_msearch", Some(&lines))
                    .and_then(|r| r.json())
                    .map(|v| v["responses"].as_array().cloned().unwrap_or_default());
                for (k, (i, _, _)) in batch.iter().enumerate() {
                    let panel = &panels[*i];
                    out[*i] = Some(match &responses {
                        Err(e) => Data::Error(e.clone()),
                        Ok(list) => match list.get(k) {
                            Some(r) if r.get("error").is_some() => {
                                Data::Error(crate::es::client::describe_error(r))
                            }
                            Some(r) if panel.kind == Kind::SavedSearch => Data::Docs(
                                r["hits"]["hits"].as_array().cloned().unwrap_or_default(),
                                saved_time_field(&saved_names, &panel.saved),
                            ),
                            Some(r) => parse_panel(panel, r),
                            None => Data::Error("Response kosong".into()),
                        },
                    });
                }
            }
            Ok(reqs
                .into_iter()
                .zip(out)
                .map(|(req, data)| match req {
                    Req::Static(d) => d,
                    Req::Search { .. } => data.unwrap_or(Data::Error("Response kosong".into())),
                    Req::Esql { conn, body } => {
                        match conns[conn].json("POST", "/_query?format=json", Some(&body)) {
                            Ok(v) => Data::Esql(
                                v["columns"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|c| c["name"].as_str().unwrap_or_default().to_owned())
                                    .collect(),
                                v["values"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|r| r.as_array().cloned().unwrap_or_default())
                                    .collect(),
                            ),
                            Err(e) => Data::Error(e),
                        }
                    }
                })
                .collect())
        }));
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_delete {
            return;
        }
        let Some(index) = self.active else {
            self.confirm_delete = false;
            return;
        };
        let message = format!("Dashboard \"{}\" akan dihapus.", self.dashes[index].name);
        match widgets::confirm_dialog(
            ctx,
            "Hapus dashboard?",
            &message,
            None,
            &mut self.confirm_typed,
            true,
        ) {
            Confirm::Yes => {
                self.dashes.remove(index);
                self.active = None;
                self.confirm_delete = false;
                self.dirty = true;
            }
            Confirm::No => self.confirm_delete = false,
            Confirm::Pending => {}
        }
    }
}

fn field_combo(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut String,
    fields: &[Field],
    keep: impl Fn(&Field) -> bool,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if value.is_empty() {
            "-"
        } else {
            value.as_str()
        })
        .width(200.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, String::new(), "-");
            for f in fields.iter().filter(|f| keep(f)) {
                ui.selectable_value(
                    value,
                    f.name.clone(),
                    format!("{}  ·  {}", f.name, f.es_type()),
                );
            }
        });
}

fn saved_time_field(saved: &[(String, Option<String>)], name: &str) -> Option<String> {
    saved
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, tf)| tf.clone())
}

/// DSH-10: request 20 dokumen terbaru dari pencarian tersimpan dengan rentang waktu dashboard.
fn saved_body(
    saved: &[SavedSearch],
    panel: &Panel,
    bounds: Option<TimeBounds>,
) -> Result<(String, Value), String> {
    let s = saved
        .iter()
        .find(|s| s.name == panel.saved)
        .ok_or("Pilih pencarian tersimpan (dibuat dari Discover).")?;
    if s.esql {
        return Err("Pencarian ES|QL belum didukung sebagai panel.".into());
    }
    let time = s.time_field.as_deref().zip(bounds);
    let query = query::build(s.mode, &s.query, &s.filters, time)?;
    let mut body = json!({"size": 20, "query": query});
    if let Some(tf) = &s.time_field {
        body["sort"] = json!([{tf: {"order": "desc", "unmapped_type": "boolean"}}]);
    }
    Ok((s.pattern.clone(), body))
}

fn is_esql(text: &str) -> bool {
    let t = text.trim_start().to_ascii_uppercase();
    t.starts_with("FROM ") || t.starts_with("ROW ") || t.starts_with("SHOW ")
}

/// DSH-11: `Ok(body _search)` atau `Err(body _query)` untuk ES|QL. Query/filter/waktu global
/// dashboard selalu diterapkan sebagai filter tambahan.
fn raw_request(
    dash: &Dash,
    panel: &Panel,
    bounds: Option<TimeBounds>,
) -> Result<Result<Value, Value>, String> {
    let time = (!dash.time_field.is_empty())
        .then_some(dash.time_field.as_str())
        .zip(bounds);
    let global = query::build(QueryMode::Kql, &dash.query, &dash.filters, time)?;
    if panel.raw.trim().is_empty() {
        return Err("Isi body DSL atau query ES|QL di editor panel.".into());
    }
    if is_esql(&panel.raw) {
        return Ok(Err(json!({"query": panel.raw.trim(), "filter": global})));
    }
    let mut body: Value =
        serde_json::from_str(&panel.raw).map_err(|e| format!("Body DSL tidak valid: {e}"))?;
    if !body.is_object() {
        return Err("Body DSL harus objek JSON.".into());
    }
    let user = body
        .get("query")
        .cloned()
        .unwrap_or_else(|| json!({"match_all": {}}));
    body["query"] = json!({"bool": {"must": [user], "filter": [global]}});
    Ok(Ok(body))
}

/// DSH-11: bucket dari `raw_path` jadi bar; selain itu tampilkan aggregations/response mentah.
fn raw_data(panel: &Panel, r: &Value) -> Data {
    if !panel.raw_path.trim().is_empty() {
        return match r.pointer(panel.raw_path.trim()).and_then(Value::as_array) {
            Some(buckets) => Data::Terms(
                buckets
                    .iter()
                    .map(|b| {
                        let key = b["key_as_string"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| query::display(&b["key"]));
                        (key, b["doc_count"].as_f64().unwrap_or(0.0))
                    })
                    .collect(),
                0.0,
            ),
            None => Data::Error(format!(
                "Pointer {} bukan array bucket di response.",
                panel.raw_path
            )),
        };
    }
    let shown = if r["aggregations"].is_object() {
        r["aggregations"].clone()
    } else {
        r.clone()
    };
    Data::Json(shown)
}

fn panel_body(
    dash: &Dash,
    panel: &Panel,
    bounds: Option<TimeBounds>,
    interval: &str,
) -> Result<Value, String> {
    let time = (!dash.time_field.is_empty())
        .then_some(dash.time_field.as_str())
        .zip(bounds);
    let mut text = dash.query.trim().to_owned();
    if !panel.query.trim().is_empty() {
        text = if text.is_empty() {
            panel.query.clone()
        } else {
            format!("({text}) and ({})", panel.query)
        };
    }
    let query = query::build(QueryMode::Kql, &text, &dash.filters, time)?;
    if panel.agg != Agg::Count && panel.field.is_empty() {
        return Err("Pilih field untuk metric.".into());
    }
    let metric = panel.agg.dsl(&panel.field).map(|m| json!({"m": m}));
    let terms = |inner: Option<Value>| {
        let mut t = json!({"terms": {"field": panel.split, "size": panel.size}});
        if let Some(inner) = inner {
            t["aggs"] = inner;
        }
        t
    };
    let aggs = match panel.kind {
        Kind::Metric | Kind::Markdown | Kind::SavedSearch | Kind::Raw => metric.clone(),
        Kind::TimeSeries => {
            let (tf, b) = time.ok_or("Time series butuh field waktu.")?;
            let inner = if panel.split.is_empty() {
                metric.clone()
            } else {
                Some(json!({"s": terms(metric.clone())}))
            };
            let mut hist = json!({"date_histogram": {
                "field": tf, "fixed_interval": interval, "time_zone": query::local_tz(),
                "min_doc_count": 0, "extended_bounds": {"min": b.from, "max": b.to}}});
            if let Some(inner) = inner {
                hist["aggs"] = inner;
            }
            Some(json!({"t": hist}))
        }
        Kind::Bar | Kind::Pie | Kind::Table | Kind::Control => {
            if panel.split.is_empty() {
                return Err("Pilih field pengelompokan.".into());
            }
            Some(json!({"s": terms(metric.clone())}))
        }
    };
    let mut body = json!({"size": 0, "track_total_hits": true, "query": query});
    if let Some(aggs) = aggs {
        body["aggs"] = aggs;
    }
    Ok(body)
}

fn parse_panel(panel: &Panel, r: &Value) -> Data {
    let aggs = &r["aggregations"];
    let total = r["hits"]["total"]["value"].as_f64().unwrap_or(0.0);
    let terms = |v: &Value| -> (Vec<(String, f64)>, f64) {
        let list = v["buckets"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| {
                let key = b["key_as_string"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| query::display(&b["key"]));
                (
                    key,
                    panel.agg.read(b, b["doc_count"].as_f64().unwrap_or(0.0)),
                )
            })
            .collect();
        (list, v["sum_other_doc_count"].as_f64().unwrap_or(0.0))
    };
    match panel.kind {
        Kind::Metric | Kind::Markdown => Data::Metric(panel.agg.read(aggs, total)),
        Kind::SavedSearch => Data::Docs(
            r["hits"]["hits"].as_array().cloned().unwrap_or_default(),
            None,
        ),
        Kind::Raw => raw_data(panel, r),
        Kind::TimeSeries => {
            let buckets = aggs["t"]["buckets"].as_array().cloned().unwrap_or_default();
            if panel.split.is_empty() {
                let points = buckets
                    .iter()
                    .map(|b| {
                        [
                            b["key"].as_f64().unwrap_or(0.0),
                            panel.agg.read(b, b["doc_count"].as_f64().unwrap_or(0.0)),
                        ]
                    })
                    .collect();
                return Data::Series(vec![(panel.agg.label().to_owned(), points)]);
            }
            let mut series: Vec<(String, Vec<[f64; 2]>)> = Vec::new();
            for b in &buckets {
                let x = b["key"].as_f64().unwrap_or(0.0);
                for (key, y) in terms(&b["s"]).0 {
                    match series.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, pts)) => pts.push([x, y]),
                        None => series.push((key, vec![[x, y]])),
                    }
                }
            }
            Data::Series(series)
        }
        Kind::Bar | Kind::Pie | Kind::Table | Kind::Control => {
            let (list, other) = terms(&aggs["s"]);
            Data::Terms(list, other)
        }
    }
}

fn fmt(format: Format, v: f64) -> String {
    if v.is_nan() {
        return "-".into();
    }
    match format {
        Format::Bytes => widgets::format_bytes(v),
        Format::Millis if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
        Format::Millis => format!("{v:.0} ms"),
        Format::Number if v.fract() == 0.0 && v.abs() < 1e15 => {
            let digits = widgets::format_count(v.abs() as u64);
            if v < 0.0 {
                format!("-{digits}")
            } else {
                digits
            }
        }
        Format::Number => format!("{v:.2}"),
    }
}

/// Gambar panel. Mengembalikan filter jika user mengklik kategori.
fn render(ui: &mut egui::Ui, i: usize, panel: &Panel, data: &Data) -> Option<Event> {
    let height = (ui.available_height() - 4.0).max(60.0);
    match data {
        Data::Metric(v) => {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(fmt(panel.format, *v)).size(36.0).strong());
            });
            None
        }
        Data::Series(series) => {
            let format = panel.format;
            let response = Plot::new(("series", i))
                .height(height)
                .legend(Legend::default())
                .allow_scroll(false)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .x_axis_formatter(|m, _| {
                    use chrono::TimeZone;
                    chrono::Local
                        .timestamp_millis_opt(m.value as i64)
                        .single()
                        .map(|d| d.format("%m-%d %H:%M").to_string())
                        .unwrap_or_default()
                })
                .y_axis_formatter(move |m, _| fmt(format, m.value))
                .show(ui, |plot| {
                    let width = series
                        .first()
                        .and_then(|(_, p)| (p.len() > 1).then(|| p[1][0] - p[0][0]))
                        .unwrap_or(1.0);
                    let mut stacked: Vec<BarChart> = Vec::new();
                    for (si, (name, points)) in series.iter().enumerate() {
                        let color = PALETTE[si % PALETTE.len()];
                        let item = series_source(i, si);
                        match panel.style {
                            Style::Bar => {
                                let bars = points
                                    .iter()
                                    .map(|[x, y]| {
                                        Bar::new(x + width / 2.0, if y.is_nan() { 0.0 } else { *y })
                                            .width(width * 0.85)
                                    })
                                    .collect();
                                let refs: Vec<&BarChart> = stacked.iter().collect();
                                let chart = BarChart::new(name.clone(), bars)
                                    .id(item)
                                    .color(color)
                                    .stack_on(&refs);
                                stacked.push(chart);
                            }
                            Style::Line | Style::Area => {
                                let pts: Vec<[f64; 2]> =
                                    points.iter().filter(|p| !p[1].is_nan()).copied().collect();
                                let mut line = Line::new(name.clone(), pts).id(item).color(color);
                                if panel.style == Style::Area {
                                    line = line.fill(0.0);
                                }
                                plot.line(line);
                            }
                        }
                    }
                    for chart in stacked {
                        plot.bar_chart(chart);
                    }
                });
            brush(ui, i, &response).or_else(|| series_click(i, panel, series, &response))
        }
        Data::Terms(list, other) => match panel.kind {
            Kind::Table => terms_table(ui, i, panel, list, *other),
            Kind::Pie => pie(ui, panel, list, *other, height),
            _ => terms_bars(ui, i, panel, list, height),
        }
        .map(Event::Filter),
        Data::Json(v) => {
            egui::ScrollArea::vertical()
                .id_salt(("json", i))
                .show(ui, |ui| {
                    widgets::json_tree(ui, v, egui::Id::unique(("raw_tree", i)));
                });
            None
        }
        Data::Esql(columns, rows) => {
            egui::ScrollArea::both()
                .id_salt(("esql", i))
                .show(ui, |ui| {
                    egui::Grid::new(("esql_grid", i))
                        .striped(true)
                        .show(ui, |ui| {
                            for c in columns {
                                ui.strong(c);
                            }
                            ui.end_row();
                            for row in rows.iter().take(500) {
                                for v in row {
                                    ui.label(query::display(v));
                                }
                                ui.end_row();
                            }
                        });
                });
            None
        }
        Data::Docs(hits, tf) => {
            egui::ScrollArea::vertical()
                .id_salt(("docs", i))
                .show(ui, |ui| {
                    if hits.is_empty() {
                        ui.weak("Tidak ada dokumen pada rentang ini.");
                    }
                    for hit in hits {
                        ui.horizontal(|ui| {
                            if let Some(tf) = tf {
                                let t = crate::ui::discover::hit_value(hit, tf)
                                    .map(query::display)
                                    .unwrap_or_default();
                                ui.monospace(crate::ui::discover::format_hit_time(&t));
                            }
                            let text = widgets::strip_highlight(&crate::ui::discover::summary(hit));
                            ui.add(egui::Label::new(text).truncate());
                        });
                    }
                });
            None
        }
        Data::Error(err) => {
            widgets::error_box(ui, err);
            None
        }
    }
}

/// DSH-14: drag di time series memilih rentang waktu baru.
fn brush(ui: &mut egui::Ui, i: usize, response: &egui_plot::PlotResponse<()>) -> Option<Event> {
    let id = egui::Id::unique(("brush", i));
    let r = &response.response;
    let x = r
        .interact_pointer_pos()
        .or(r.hover_pos())
        .map(|p| response.transform.value_from_position(p).x);
    if r.drag_started() {
        ui.data_mut(|d| d.insert_temp(id, x));
    }
    let from: Option<f64> = ui.data(|d| d.get_temp::<Option<f64>>(id)).flatten();
    if let (Some(a), Some(b)) = (from, x) {
        let (pa, pb) = (
            response.transform.position_from_point_x(a),
            response.transform.position_from_point_x(b),
        );
        let rect = egui::Rect::from_x_y_ranges(pa.min(pb)..=pa.max(pb), r.rect.y_range());
        ui.painter().rect_filled(
            rect,
            0.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.3),
        );
    }
    if r.drag_stopped() {
        ui.data_mut(|d| d.remove::<Option<f64>>(id));
        if let (Some(a), Some(b)) = (from, x)
            && (a - b).abs() > 1_000.0
        {
            return Some(Event::Zoom(a.min(b) as i64, a.max(b) as i64));
        }
    }
    r.clone()
        .on_hover_text("Drag untuk memilih rentang waktu · klik seri untuk memfilter");
    None
}

/// DSH-14: pemetaan id item plot -> nama seri (id dibuat per indeks agar nama
/// kembar tidak bertabrakan).
fn series_source(i: usize, si: usize) -> (&'static str, usize, usize) {
    ("series", i, si)
}

fn series_ids(i: usize, series: &[(String, Vec<[f64; 2]>)]) -> Vec<(egui_plot::ItemId, String)> {
    series
        .iter()
        .enumerate()
        .map(|(si, (name, _))| (egui_plot::ItemId::new(series_source(i, si)), name.clone()))
        .collect()
}

/// DSH-14: klik (bukan drag) pada seri ter-hover menambah filter `split = seri`.
fn series_click(
    i: usize,
    panel: &Panel,
    series: &[(String, Vec<[f64; 2]>)],
    response: &egui_plot::PlotResponse<()>,
) -> Option<Event> {
    if panel.split.is_empty() || !response.response.clicked() {
        return None;
    }
    let hovered = response.hovered_plot_item?;
    let name = series_ids(i, series)
        .into_iter()
        .find_map(|(id, name)| (id == hovered).then_some(name))?;
    Some(Event::Filter(Filter::is(&panel.split, &name, false)))
}

fn terms_bars(
    ui: &mut egui::Ui,
    i: usize,
    panel: &Panel,
    list: &[(String, f64)],
    height: f32,
) -> Option<Filter> {
    let labels: Vec<String> = list.iter().map(|(k, _)| k.clone()).collect();
    let bars: Vec<Bar> = list
        .iter()
        .enumerate()
        .map(|(x, (k, v))| {
            Bar::new(x as f64, if v.is_nan() { 0.0 } else { *v })
                .name(k)
                .fill(PALETTE[x % PALETTE.len()])
        })
        .collect();
    let format = panel.format;
    let response = Plot::new(("bars", i))
        .height(height)
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .x_axis_formatter(move |m, _| {
            let idx = m.value.round();
            if (m.value - idx).abs() < 1e-6 && idx >= 0.0 {
                labels.get(idx as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        })
        .y_axis_formatter(move |m, _| fmt(format, m.value))
        .show(ui, |plot| {
            plot.bar_chart(BarChart::new("bars", bars).width(0.7))
        });
    let r = &response.response;
    if r.clicked()
        && let Some(pos) = r.interact_pointer_pos()
    {
        let x = response.transform.value_from_position(pos).x.round();
        if x >= 0.0
            && let Some((key, _)) = list.get(x as usize)
        {
            return Some(Filter::is(&panel.split, key, false));
        }
    }
    r.clone()
        .on_hover_text("Klik batang untuk memfilter dashboard");
    None
}

fn terms_table(
    ui: &mut egui::Ui,
    i: usize,
    panel: &Panel,
    list: &[(String, f64)],
    other: f64,
) -> Option<Filter> {
    let mut picked = None;
    // Urutan sort disimpan di memori egui per panel: (kolom 0=kunci 1=nilai, naik?).
    let sort_id = egui::Id::unique(("table_sort", i));
    let mut sort: (u8, bool) = ui.data(|d| d.get_temp(sort_id)).unwrap_or((1, false));
    let mut rows: Vec<(String, f64)> = list.to_vec();
    rows.sort_by(|a, b| {
        let ord = if sort.0 == 0 {
            a.0.cmp(&b.0)
        } else {
            a.1.total_cmp(&b.1)
        };
        if sort.1 { ord } else { ord.reverse() }
    });
    ui.horizontal(|ui| {
        if ui.small_button("Ekspor CSV").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!("{}.csv", panel.title))
                .save_file()
        {
            let mut text = format!(
                "{},{}\n",
                widgets::csv_cell(&panel.split),
                widgets::csv_cell(panel.agg.label())
            );
            for (k, v) in &rows {
                text.push_str(&format!("{},{v}\n", widgets::csv_cell(k)));
            }
            if let Err(err) = std::fs::write(&path, text) {
                eprintln!("gagal ekspor CSV: {err}");
            }
        }
    });
    egui::ScrollArea::vertical()
        .id_salt(("table", i))
        .show(ui, |ui| {
            egui::Grid::new(("terms_table", i))
                .striped(true)
                .num_columns(2)
                .show(ui, |ui| {
                    for (col, label) in [(0u8, panel.split.as_str()), (1, panel.agg.label())] {
                        let arrow = match sort {
                            (c, true) if c == col => " ↑",
                            (c, false) if c == col => " ↓",
                            _ => "",
                        };
                        if ui
                            .button(RichText::new(format!("{label}{arrow}")).strong())
                            .clicked()
                        {
                            sort = if sort.0 == col {
                                (col, !sort.1)
                            } else {
                                (col, col == 0)
                            };
                        }
                    }
                    ui.end_row();
                    for (k, v) in &rows {
                        if ui.link(k).on_hover_text("Klik untuk memfilter").clicked() {
                            picked = Some(Filter::is(&panel.split, k, false));
                        }
                        ui.label(fmt(panel.format, *v));
                        ui.end_row();
                    }
                    if other > 0.0 {
                        ui.weak("Lainnya (dokumen)");
                        ui.weak(widgets::format_count(other as u64));
                        ui.end_row();
                    }
                });
        });
    ui.data_mut(|d| d.insert_temp(sort_id, sort));
    picked
}

fn pie(
    ui: &mut egui::Ui,
    panel: &Panel,
    list: &[(String, f64)],
    other: f64,
    height: f32,
) -> Option<Filter> {
    let mut slices: Vec<(String, f64, Color32)> = list
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, (k, v))| (k.clone(), v.max(0.0), PALETTE[i % PALETTE.len()]))
        .collect();
    if other > 0.0 && panel.agg == Agg::Count {
        slices.push(("Lainnya".into(), other, Color32::GRAY));
    }
    let total: f64 = slices.iter().map(|s| s.1).sum();
    let mut picked = None;
    ui.horizontal(|ui| {
        let size = height.min(ui.available_width() * 0.5).max(60.0);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
        let center = rect.center();
        let radius = size / 2.0 - 4.0;
        let painter = ui.painter_at(rect);
        let mut angle = -std::f64::consts::FRAC_PI_2;
        let mut hit = None;
        let pointer = response.hover_pos().map(|p| {
            let d = p - center;
            (d.length(), (d.y as f64).atan2(d.x as f64))
        });
        for (i, (_, value, color)) in slices.iter().enumerate() {
            if total <= 0.0 {
                break;
            }
            let sweep = value / total * std::f64::consts::TAU;
            let steps = ((sweep * 30.0).ceil() as usize).max(2);
            let mut points = vec![center];
            for s in 0..=steps {
                let a = angle + sweep * s as f64 / steps as f64;
                points.push(center + egui::vec2(a.cos() as f32, a.sin() as f32) * radius);
            }
            painter.add(egui::Shape::convex_polygon(
                points,
                *color,
                egui::Stroke::new(1.0, ui.visuals().panel_fill),
            ));
            if let Some((dist, a)) = pointer {
                let mut rel = a - angle;
                while rel < 0.0 {
                    rel += std::f64::consts::TAU;
                }
                if dist <= radius && rel <= sweep {
                    hit = Some(i);
                }
            }
            angle += sweep;
        }
        painter.circle_filled(center, radius * 0.5, ui.visuals().panel_fill);
        if let Some(i) = hit {
            let (k, v, _) = &slices[i];
            let pct = v / total * 100.0;
            response
                .clone()
                .on_hover_text(format!("{k}: {} ({pct:.1}%)", fmt(panel.format, *v)));
            if response.clicked() && k != "Lainnya" {
                picked = Some(Filter::is(&panel.split, k, false));
            }
        }
        ui.vertical(|ui| {
            for (k, v, color) in &slices {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("■").color(*color));
                    let pct = if total > 0.0 { v / total * 100.0 } else { 0.0 };
                    if ui.link(format!("{k} · {pct:.1}%")).clicked() && k != "Lainnya" {
                        picked = Some(Filter::is(&panel.split, k, false));
                    }
                });
            }
        });
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_ids_map_back_to_names() {
        let series = vec![
            ("a".to_owned(), vec![]),
            ("b".to_owned(), vec![]),
            ("a".to_owned(), vec![]),
        ];
        let ids = series_ids(3, &series);
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[1].1, "b");
        assert_ne!(ids[0].0, ids[2].0);
        assert_ne!(ids[0].0, series_ids(4, &series)[0].0);
        // Id yang disimpan egui_plot lewat `.id(source)` harus cocok dengan peta.
        use egui_plot::PlotItem;
        let line = Line::new("a", vec![[0.0, 0.0]]).id(series_source(3, 1));
        let chart = BarChart::new("a", vec![Bar::new(0.0, 1.0)]).id(series_source(3, 1));
        assert_eq!(PlotItem::id(&line), ids[1].0);
        assert_eq!(PlotItem::id(&chart), ids[1].0);
    }

    #[test]
    fn builds_panel_requests() {
        let dash = Dash {
            pattern: "logs-*".into(),
            time_field: "@timestamp".into(),
            ..Dash::default()
        };
        let bounds = Some(TimeBounds {
            from: 0,
            to: 3_600_000,
        });
        let panel = Panel {
            kind: Kind::TimeSeries,
            agg: Agg::Avg,
            field: "bytes".into(),
            split: "host".into(),
            ..Panel::default()
        };
        let body = panel_body(&dash, &panel, bounds, "1m").unwrap();
        assert_eq!(body["aggs"]["t"]["date_histogram"]["fixed_interval"], "1m");
        assert_eq!(body["aggs"]["t"]["aggs"]["s"]["terms"]["field"], "host");
        assert_eq!(
            body["aggs"]["t"]["aggs"]["s"]["aggs"]["m"]["avg"]["field"],
            "bytes"
        );
        let bad = Panel {
            kind: Kind::Bar,
            ..Panel::default()
        };
        assert!(panel_body(&dash, &bad, bounds, "1m").is_err());
    }

    #[test]
    fn builds_saved_search_panel() {
        let saved = vec![SavedSearch {
            name: "error".into(),
            pattern: "logs-*".into(),
            mode: QueryMode::Kql,
            esql: false,
            query: "level:error".into(),
            filters: vec![],
            columns: vec![],
            sort: None,
            time_field: Some("@timestamp".into()),
        }];
        let panel = Panel {
            kind: Kind::SavedSearch,
            saved: "error".into(),
            ..Panel::default()
        };
        let (index, body) =
            saved_body(&saved, &panel, Some(TimeBounds { from: 1, to: 2 })).unwrap();
        assert_eq!(index, "logs-*");
        assert_eq!(body["size"], 20);
        assert_eq!(body["query"]["bool"]["must"][0]["match"]["level"], "error");
        assert_eq!(body["sort"][0]["@timestamp"]["order"], "desc");
        let missing = Panel {
            kind: Kind::SavedSearch,
            ..Panel::default()
        };
        assert!(saved_body(&saved, &missing, None).is_err());
    }

    #[test]
    fn builds_raw_requests() {
        let dash = Dash {
            pattern: "logs-*".into(),
            time_field: "@timestamp".into(),
            query: "level:error".into(),
            ..Dash::default()
        };
        let b = Some(TimeBounds { from: 1, to: 2 });
        let dsl = Panel {
            kind: Kind::Raw,
            raw: r#"{"size": 0, "query": {"term": {"a": 1}}, "aggs": {"x": {"terms": {"field": "h"}}}}"#.into(),
            raw_path: "/aggregations/x/buckets".into(),
            ..Panel::default()
        };
        let body = raw_request(&dash, &dsl, b).unwrap().unwrap();
        assert_eq!(body["query"]["bool"]["must"][0]["term"]["a"], 1);
        assert_eq!(
            body["query"]["bool"]["filter"][0]["bool"]["filter"][0]["range"]["@timestamp"]["gte"],
            1
        );
        let esql = Panel {
            kind: Kind::Raw,
            raw: "FROM logs-* | LIMIT 1".into(),
            ..Panel::default()
        };
        let body = raw_request(&dash, &esql, b).unwrap().unwrap_err();
        assert_eq!(body["query"], "FROM logs-* | LIMIT 1");
        let r = json!({"aggregations": {"x": {"buckets": [{"key": "a", "doc_count": 3}]}}});
        assert!(
            matches!(raw_data(&dsl, &r), Data::Terms(list, _) if list == vec![("a".to_string(), 3.0)])
        );
        let tree = Panel {
            raw_path: String::new(),
            ..dsl
        };
        assert!(matches!(raw_data(&tree, &r), Data::Json(_)));
    }

    #[test]
    fn parses_terms_and_series() {
        let panel = Panel {
            kind: Kind::Bar,
            split: "host".into(),
            ..Panel::default()
        };
        let r = json!({"hits": {"total": {"value": 10}}, "aggregations": {"s": {"sum_other_doc_count": 2,
            "buckets": [{"key": "a", "doc_count": 5}, {"key": 7, "doc_count": 3}]}}});
        match parse_panel(&panel, &r) {
            Data::Terms(list, other) => {
                assert_eq!(list, vec![("a".to_string(), 5.0), ("7".to_string(), 3.0)]);
                assert_eq!(other, 2.0);
            }
            _ => panic!("harus terms"),
        }
        let metric = Panel {
            agg: Agg::P95,
            field: "x".into(),
            ..Panel::default()
        };
        let r = json!({"aggregations": {"m": {"values": {"95.0": 12.5}}}});
        assert!(matches!(parse_panel(&metric, &r), Data::Metric(v) if v == 12.5));
        assert_eq!(fmt(Format::Number, 1234.0), "1.234");
        assert_eq!(fmt(Format::Number, -1234.0), "-1.234");
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_msearch_panels() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
        let fields = crate::es::catalog::parse_field_caps(
            &conn
                .get(&format!("/{pattern}/_field_caps?fields=*"))
                .unwrap(),
        );
        let tf = fields
            .iter()
            .find(|f| f.is_date())
            .map(|f| f.name.clone())
            .unwrap_or_default();
        let split = fields
            .iter()
            .find(|f| f.aggregatable && f.es_type() == "keyword")
            .map(|f| f.name.clone())
            .unwrap();
        let dash = Dash {
            pattern: pattern.clone(),
            time_field: tf,
            range: TimeRange::new("now-1y", "now"),
            ..Dash::default()
        };
        let bounds = dash.range.bounds().ok();
        let panels = [
            Panel {
                kind: Kind::Metric,
                ..Panel::default()
            },
            Panel {
                kind: Kind::Bar,
                split: split.clone(),
                ..Panel::default()
            },
            Panel {
                kind: Kind::TimeSeries,
                split,
                ..Panel::default()
            },
        ];
        let mut lines = String::new();
        for p in &panels {
            lines.push_str(&format!(
                "{}\n{}\n",
                json!({"index": pattern}),
                panel_body(&dash, p, bounds, "7d").unwrap()
            ));
        }
        let value = conn
            .send("POST", "/_msearch", Some(&lines))
            .unwrap()
            .json()
            .unwrap();
        let responses = value["responses"].as_array().unwrap();
        for (p, r) in panels.iter().zip(responses) {
            match parse_panel(p, r) {
                Data::Metric(v) => println!("metric {v}"),
                Data::Terms(list, other) => println!("terms {} (+{other})", list.len()),
                Data::Series(s) => println!("series {}", s.len()),
                Data::Error(e) => panic!("{e}"),
                Data::Docs(hits, _) => println!("docs {}", hits.len()),
                Data::Json(_) | Data::Esql(..) => {}
            }
            assert!(r.get("error").is_none(), "{r}");
        }
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_raw_panels() {
        let conn = crate::es::client::live_conn().expect("env");
        let pattern = std::env::var("LIVE_PATTERN").unwrap_or_else(|_| "*".into());
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
        let dash = Dash {
            pattern: pattern.clone(),
            ..Dash::default()
        };
        let dsl = Panel {
            kind: Kind::Raw,
            raw: format!(
                r#"{{"size": 0, "aggs": {{"top": {{"terms": {{"field": "{kw}", "size": 3}}}}}}}}"#
            ),
            raw_path: "/aggregations/top/buckets".into(),
            ..Panel::default()
        };
        let body = raw_request(&dash, &dsl, None).unwrap().unwrap();
        let r = conn
            .json("POST", &format!("/{pattern}/_search"), Some(&body))
            .unwrap();
        assert!(matches!(raw_data(&dsl, &r), Data::Terms(list, _) if !list.is_empty()));
        let esql = Panel {
            kind: Kind::Raw,
            raw: format!("FROM {pattern} | STATS c = COUNT(*) BY {kw} | LIMIT 3"),
            ..Panel::default()
        };
        let body = raw_request(&dash, &esql, None).unwrap().unwrap_err();
        let v = conn
            .json("POST", "/_query?format=json", Some(&body))
            .unwrap();
        println!(
            "raw ES|QL baris {}",
            v["values"].as_array().map_or(0, Vec::len)
        );
        assert!(!v["values"].as_array().unwrap().is_empty());
    }
}
