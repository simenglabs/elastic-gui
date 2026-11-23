//! Index Management: daftar index, detail (overview/settings/mappings), aksi dengan guard.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use serde_json::{Value, json};

use super::{Ctx, Nav};
use crate::es::Conn;
use crate::i18n::{self, t, tf};
use crate::task::{Pending, poll};
use crate::widgets::{self, Confirm};

#[derive(Clone, Debug)]
struct Row {
    name: String,
    health: String,
    status: String,
    pri: u64,
    rep: u64,
    docs: u64,
    deleted: u64,
    size: f64,
    created: i64,
    aliases: Vec<String>,
    data_stream: Option<String>,
}

impl Row {
    fn system(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// Fungsi murni di balik `visible_rows` agar bisa diukur tanpa UI (NFR: 10.000 index).
fn filter_sort_rows<'a>(
    rows: &'a [Row],
    filter: &str,
    show_hidden: bool,
    health_filter: Option<&str>,
    sort: Option<(Col, bool)>,
) -> Vec<&'a Row> {
    let needle = filter.to_lowercase();
    let mut rows: Vec<&Row> = rows
        .iter()
        .filter(|r| show_hidden || !r.system())
        .filter(|r| {
            needle.is_empty()
                || r.name.to_lowercase().contains(&needle)
                || r.aliases.iter().any(|a| a.contains(&needle))
        })
        .filter(|r| health_filter.is_none_or(|h| r.health == h))
        .collect();
    if let Some((col, asc)) = sort {
        rows.sort_by(|a, b| {
            let ord = match col {
                Col::Name => a.name.cmp(&b.name),
                Col::Health => a.health.cmp(&b.health),
                Col::Status => a.status.cmp(&b.status),
                Col::Pri => a.pri.cmp(&b.pri),
                Col::Rep => a.rep.cmp(&b.rep),
                Col::Docs => a.docs.cmp(&b.docs),
                Col::Size => a.size.total_cmp(&b.size),
                Col::Created => a.created.cmp(&b.created),
            };
            if asc { ord } else { ord.reverse() }
        });
    }
    rows
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
    Health,
    Status,
    Pri,
    Rep,
    Docs,
    Size,
    Created,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Overview,
    Settings,
    Mappings,
    Shards,
    Ilm,
    Aliases,
    Docs,
}

struct Detail {
    name: String,
    stats: Value,
    /// Setting eksplisit (flat); dipakai `settings_form`.
    settings: Vec<(String, String)>,
    /// IDX-11: eksplisit + default (flag true = default); default kosong bila tidak diminta.
    settings_all: Vec<(String, String, bool)>,
    /// IDX-11: settings bersarang mentah (`settings`, plus `defaults` bila diminta), sudah
    /// diformat sekali saat dimuat supaya tidak diserialisasi ulang tiap frame.
    settings_text: String,
    mappings: Value,
    /// IDX-13: `mappings` terformat, dihitung sekali saat dimuat.
    mappings_text: String,
    privs: Privs,
}

/// PRF-12: privilege user pada index terpilih. Default diizinkan jika tidak bisa dicek
/// (mis. security nonaktif); ES tetap menolak jika memang tidak berhak.
#[derive(Clone, Copy, Debug)]
struct Privs {
    manage: bool,
    delete_index: bool,
}

impl Default for Privs {
    fn default() -> Self {
        Self {
            manage: true,
            delete_index: true,
        }
    }
}

impl Privs {
    /// `None` = diizinkan; `Some(privilege)` = privilege yang kurang.
    fn missing(self, action: &Action) -> Option<&'static str> {
        match action {
            Action::Create { .. } => None,
            Action::Delete(_) if self.delete_index || self.manage => None,
            Action::Delete(_) => Some("delete_index"),
            _ if self.manage => None,
            _ => Some("manage"),
        }
    }
}

fn load_privs(conn: &Conn, name: &str) -> Privs {
    let body = json!({"index": [{"names": [name], "privileges": ["manage", "delete_index"]}]});
    match conn.json("POST", "/_security/user/_has_privileges", Some(&body)) {
        Ok(v) => {
            let p = &v["index"][name];
            Privs {
                manage: p["manage"].as_bool().unwrap_or(true),
                delete_index: p["delete_index"].as_bool().unwrap_or(true),
            }
        }
        Err(_) => Privs::default(),
    }
}

#[derive(Clone)]
enum Action {
    Create {
        name: String,
        body: Value,
    },
    Delete(String),
    Open(String),
    Close(String),
    Refresh(String),
    Flush(String),
    ClearCache(String),
    UpdateSettings(String, Value),
    AddField(String, Value),
    IlmRetry(String),
    Aliases(String, Value),
    ForceMerge(String),
    Rollover(String),
    Reindex {
        source: String,
        dest: String,
        query: Value,
    },
    /// IDX-12: setting statis: close -> update -> open.
    StaticSettings(String, Value),
    /// IDX-38: `_clone` / `_shrink` / `_split` (source diblok tulis dulu).
    Resize {
        op: &'static str,
        source: String,
        target: String,
        shards: Option<u32>,
    },
    /// IDX-39: `_delete_by_query` / `_update_by_query` (async).
    ByQuery {
        index: String,
        delete: bool,
        query: Value,
        script: Option<String>,
    },
}

impl Action {
    fn request(&self) -> (&'static str, String, Option<Value>) {
        match self {
            Action::Create { name, body } => ("PUT", format!("/{name}"), Some(body.clone())),
            Action::Delete(n) => ("DELETE", format!("/{n}"), None),
            Action::Open(n) => ("POST", format!("/{n}/_open"), None),
            Action::Close(n) => ("POST", format!("/{n}/_close"), None),
            Action::Refresh(n) => ("POST", format!("/{n}/_refresh"), None),
            Action::Flush(n) => ("POST", format!("/{n}/_flush"), None),
            Action::ClearCache(n) => ("POST", format!("/{n}/_cache/clear"), None),
            Action::UpdateSettings(n, v) => ("PUT", format!("/{n}/_settings"), Some(v.clone())),
            Action::AddField(n, v) => ("PUT", format!("/{n}/_mapping"), Some(v.clone())),
            Action::IlmRetry(n) => ("POST", format!("/{n}/_ilm/retry"), None),
            Action::Aliases(_, body) => ("POST", "/_aliases".into(), Some(body.clone())),
            Action::ForceMerge(n) => (
                "POST",
                format!("/{n}/_forcemerge?max_num_segments=1&wait_for_completion=false"),
                None,
            ),
            Action::Rollover(target) => ("POST", format!("/{target}/_rollover"), None),
            Action::Reindex {
                source,
                dest,
                query,
            } => (
                "POST",
                "/_reindex?wait_for_completion=false".into(),
                Some(json!({"source": {"index": source, "query": query}, "dest": {"index": dest}})),
            ),
            Action::ByQuery {
                index,
                delete,
                query,
                script,
            } => {
                let op = if *delete {
                    "_delete_by_query"
                } else {
                    "_update_by_query"
                };
                let mut body = json!({"query": query});
                if let Some(source) = script.as_ref().filter(|s| !s.trim().is_empty()) {
                    body["script"] = json!({"source": source, "lang": "painless"});
                }
                (
                    "POST",
                    format!("/{index}/{op}?wait_for_completion=false&conflicts=proceed"),
                    Some(body),
                )
            }
            Action::StaticSettings(n, v) => ("PUT", format!("/{n}/_settings"), Some(v.clone())),
            Action::Resize {
                op,
                source,
                target,
                shards,
            } => {
                let mut settings = json!({"index.blocks.write": null});
                if let Some(n) = shards {
                    settings["index.number_of_shards"] = json!(n);
                }
                (
                    "POST",
                    format!("/{source}/{op}/{target}"),
                    Some(json!({"settings": settings})),
                )
            }
        }
    }

    /// Urutan request; aksi multi-langkah menambah langkah persiapan/pemulihan.
    fn steps(&self) -> Vec<(&'static str, String, Option<Value>)> {
        match self {
            Action::StaticSettings(n, _) => vec![
                ("POST", format!("/{n}/_close"), None),
                self.request(),
                ("POST", format!("/{n}/_open"), None),
            ],
            Action::Resize { source, .. } => vec![
                (
                    "PUT",
                    format!("/{source}/_settings"),
                    Some(json!({"index.blocks.write": true})),
                ),
                self.request(),
            ],
            _ => vec![self.request()],
        }
    }

    /// Request ES yang akan dikirim, untuk ditampilkan sebelum eksekusi.
    fn describe(&self) -> String {
        self.steps()
            .into_iter()
            .map(|(method, path, body)| match body {
                Some(b) => format!(
                    "{method} {path}\n{}",
                    serde_json::to_string_pretty(&b).unwrap_or_default()
                ),
                None => format!("{method} {path}"),
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn done_message(&self) -> String {
        match self {
            Action::Create { name, .. } => tf("Index {0} dibuat.", &[name]),
            Action::Delete(n) => tf("Index {0} dihapus.", &[n]),
            Action::Open(n) => tf("Index {0} dibuka.", &[n]),
            Action::Close(n) => tf("Index {0} ditutup.", &[n]),
            Action::Refresh(n) => tf("Index {0} di-refresh.", &[n]),
            Action::Flush(n) => tf("Index {0} di-flush.", &[n]),
            Action::ClearCache(n) => tf("Cache index {0} dibersihkan.", &[n]),
            Action::UpdateSettings(n, _) => tf("Settings {0} diperbarui.", &[n]),
            Action::AddField(n, _) => tf("Field baru ditambahkan ke mapping {0}.", &[n]),
            Action::IlmRetry(n) => tf("ILM {0} dicoba ulang.", &[n]),
            Action::Aliases(n, _) => tf("Alias {0} diperbarui.", &[n]),
            Action::ForceMerge(n) => tf(
                "Force merge {0} berjalan di latar (lihat GET /_tasks?actions=*forcemerge*).",
                &[n],
            ),
            Action::Rollover(target) => tf("Rollover {0} selesai.", &[target]),
            Action::Reindex { source, dest, .. } => {
                tf("Reindex {0} → {1} dimulai.", &[source, dest])
            }
            Action::ByQuery {
                index,
                delete: true,
                ..
            } => tf("Delete by query di {0} dimulai.", &[index]),
            Action::ByQuery { index, .. } => tf("Update by query di {0} dimulai.", &[index]),
            Action::StaticSettings(n, _) => tf(
                "Setting statis {0} diperbarui dan index dibuka kembali.",
                &[n],
            ),
            Action::Resize {
                op, source, target, ..
            } => tf(
                "{0} {1} → {2} dibuat. {1} masih diblok tulis; buka blok setelah {2} green.",
                &[&op.trim_start_matches('_'), source, target],
            ),
        }
    }
}

struct CreateForm {
    name: String,
    shards: u32,
    replicas: u32,
    mappings: String,
}

struct SettingsForm {
    replicas: String,
    refresh_interval: String,
    max_result_window: String,
    block_write: bool,
    read_only: bool,
}

const FIELD_TYPES: [&str; 11] = [
    "keyword",
    "text",
    "long",
    "integer",
    "double",
    "boolean",
    "date",
    "ip",
    "geo_point",
    "object",
    "flattened",
];

#[derive(Default)]
pub struct Indices {
    rows: Vec<Row>,
    loaded: Option<Instant>,
    pending: Option<Pending<Result<Vec<Row>, String>>>,
    error: Option<String>,
    filter: String,
    show_hidden: bool,
    health_filter: Option<&'static str>,
    sort: Option<(Col, bool)>,
    selected: Option<String>,
    tab: Tab,
    detail: Option<Result<Detail, String>>,
    detail_pending: Option<Pending<Result<Detail, String>>>,
    action_pending: Option<Pending<(Action, Result<Value, String>)>>,
    /// IDX-05: index yang dicentang untuk aksi massal.
    checked: std::collections::BTreeSet<String>,
    /// IDX-36: form reindex (sumber, tujuan, query KQL).
    reindex_form: Option<(String, String, String)>,
    /// X-02: aksi terakhir untuk Inspect.
    last_action: Option<Action>,
    /// Task reindex yang dipantau: (task id, status terakhir).
    task: Option<(String, Option<Result<Value, String>>)>,
    task_label: &'static str,
    /// IDX-12: (index, key setting, nilai JSON).
    static_form: Option<(String, String, String)>,
    /// IDX-38: (index sumber, op, target, jumlah shard, shard primer sumber).
    resize_form: Option<(String, &'static str, String, u32, u64)>,
    /// IDX-39: form by-query (index, hapus?, KQL, script, hasil hitung).
    by_query: Option<ByQueryForm>,
    count_pending: Option<Pending<Result<u64, String>>>,
    task_pending: Option<Pending<Result<Value, String>>>,
    task_polled: Option<Instant>,
    /// IDX-17: 20 dokumen contoh.
    docs: Option<Keyed<Vec<Value>>>,
    docs_pending: Option<Pending<Keyed<Vec<Value>>>>,
    message: Option<Result<String, String>>,
    confirm: Option<Action>,
    confirm_typed: String,
    create: Option<CreateForm>,
    settings_form: Option<SettingsForm>,
    /// IDX-11: tampilan settings (JSON?) dan muat default.
    settings_json: bool,
    show_defaults: bool,
    /// Reload hanya-default sedang berjalan: form settings yang sedang diisi jangan ditimpa.
    defaults_reload: bool,
    /// IDX-13: tampilan mappings (JSON?).
    mappings_json: bool,
    highlighter: widgets::Highlighter,
    new_field: (String, String),
    shards: Option<Keyed<Vec<Value>>>,
    shards_pending: Option<Pending<Keyed<Vec<Value>>>>,
    explain: Option<Result<Value, String>>,
    explain_pending: Option<Pending<Result<Value, String>>>,
    ilm: Option<Keyed<Value>>,
    ilm_pending: Option<Pending<Keyed<Value>>>,
    alias_input: (String, bool),
    privs: Privs,
    view: View,
    streams: super::streams::DataStreams,
    templates: super::streams::Templates,
}

/// Hasil yang dikaitkan dengan nama index saat diminta.
type Keyed<T> = (String, Result<T, String>);

struct ByQueryForm {
    index: String,
    delete: bool,
    kql: String,
    script: String,
    count: Option<Result<u64, String>>,
    typed: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum View {
    #[default]
    Indices,
    DataStreams,
    Templates,
}

impl Indices {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let ctx = ui.ctx().clone();
        if let Some(result) = poll(&mut self.pending) {
            match result {
                Ok(rows) => {
                    self.rows = rows;
                    self.error = None;
                }
                Err(err) => self.error = Some(err),
            }
        }
        if let Some(result) = poll(&mut self.detail_pending) {
            if rebuild_settings_form(self.defaults_reload, self.settings_form.is_some()) {
                self.settings_form = result.as_ref().ok().map(settings_form);
            }
            self.defaults_reload = false;
            self.privs = result.as_ref().map(|d| d.privs).unwrap_or_default();
            self.detail = Some(result);
        }
        if let Some((action, result)) = poll(&mut self.action_pending) {
            let ok = result.is_ok();
            if let (Action::Reindex { .. } | Action::ByQuery { .. }, Ok(v)) = (&action, &result)
                && let Some(task) = v["task"].as_str()
            {
                self.task = Some((task.to_owned(), None));
                self.task_label = match &action {
                    Action::ByQuery { delete: true, .. } => "Delete by query",
                    Action::ByQuery { .. } => "Update by query",
                    _ => "Reindex",
                };
                self.task_polled = None;
            }
            self.message = Some(result.map(|_| action.done_message()));
            self.last_action = Some(action.clone());
            if ok {
                if matches!(
                    action,
                    Action::Delete(_) | Action::Close(_) | Action::Open(_)
                ) {
                    self.checked.clear();
                }
                cx.catalog.invalidate();
                self.loaded = None;
                match action {
                    Action::Delete(_) => {
                        self.selected = None;
                        self.detail = None;
                    }
                    Action::Create { name, .. } => self.select(&ctx, cx.conn, &name),
                    _ => {
                        if let Some(name) = self.selected.clone() {
                            self.select(&ctx, cx.conn, &name);
                        }
                    }
                }
            }
        }
        if self.loaded.is_none() && self.pending.is_none() {
            self.loaded = Some(Instant::now());
            let conn = cx.conn.clone();
            self.pending = Some(Pending::spawn(&ctx, move || load_rows(&conn)));
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Indices, "Index");
            ui.selectable_value(&mut self.view, View::DataStreams, "Data stream");
            ui.selectable_value(&mut self.view, View::Templates, "Template");
        });
        ui.separator();
        match self.view {
            View::DataStreams => return self.streams.ui(ui, cx),
            View::Templates => return self.templates.ui(ui, cx),
            View::Indices => {}
        }
        self.toolbar(ui, cx);
        self.task_ui(ui, cx);
        self.bulk_bar(ui, cx);
        if let Some(name) = self.selected.clone() {
            egui::Panel::right("index_detail")
                .resizable(true)
                .default_size(ui.available_width() * 0.45)
                .show(ui, |ui| self.detail_ui(ui, cx, &name));
        }
        egui::CentralPanel::default().show(ui, |ui| self.table(ui, cx));
        self.dialogs(&ctx, cx);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(t("Cari index…"))
                    .desired_width(240.0),
            );
            ui.checkbox(&mut self.show_hidden, t("Tampilkan index sistem (.)"));
            ui.label("Health:");
            for (value, label) in [
                (None, t("semua")),
                (Some("green"), "green"),
                (Some("yellow"), "yellow"),
                (Some("red"), "red"),
            ] {
                ui.selectable_value(&mut self.health_filter, value, label);
            }
            if self.pending.is_some() {
                ui.spinner();
            } else if ui.button(t("⟳ Muat ulang")).clicked() {
                self.loaded = None;
            }
            if !cx.conn.read_only && ui.button(t("+ Buat index")).clicked() {
                self.create = Some(CreateForm {
                    name: String::new(),
                    shards: 1,
                    replicas: 1,
                    mappings: String::new(),
                });
            }
            if self.action_pending.is_some() {
                ui.spinner();
                ui.weak(t("Menjalankan aksi…"));
            }
        });
        if let Some(err) = &self.error {
            widgets::error_box(ui, err);
        }
        match &self.message {
            Some(Ok(text)) => {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xa0, 0x43), text);
            }
            Some(Err(err)) => widgets::error_box(ui, err),
            None => {}
        }
        if self.message.is_some()
            && let Some(action) = &self.last_action
            && ui
                .small_button("Inspect")
                .on_hover_text(tf("Buka di Dev Tools:\n{0}", &[&action.describe()]))
                .clicked()
        {
            let (method, path, body) = action.request();
            *cx.nav = Some(Nav::Console {
                method: method.into(),
                path,
                body,
            });
        }
        ui.separator();
    }

    fn visible_rows(&self) -> Vec<&Row> {
        filter_sort_rows(
            &self.rows,
            &self.filter,
            self.show_hidden,
            self.health_filter,
            self.sort,
        )
    }

    fn table(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let rows: Vec<Row> = self.visible_rows().into_iter().cloned().collect();
        ui.weak(tf(
            "{0} index ditampilkan dari {1}",
            &[&rows.len(), &self.rows.len()],
        ));
        let mut clicked = None;
        let mut sort_click = None;
        let sort = self.sort;
        let selected = self.selected.clone();
        let checked = self.checked.clone();
        let read_only = cx.conn.read_only;
        let mut toggles: Vec<String> = Vec::new();
        let header = |ui: &mut egui::Ui, label: &str, col: Col, sort_click: &mut Option<Col>| {
            let arrow = match sort {
                Some((c, true)) if c == col => " ↑",
                Some((c, false)) if c == col => " ↓",
                _ => "",
            };
            if ui
                .button(RichText::new(format!("{label}{arrow}")).strong())
                .clicked()
            {
                *sort_click = Some(col);
            }
        };
        TableBuilder::new(ui)
            .id_salt("index_table")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(22.0))
            .column(Column::initial(300.0).clip(true).at_least(120.0))
            .column(Column::initial(70.0))
            .column(Column::initial(60.0))
            .column(Column::initial(40.0))
            .column(Column::initial(40.0))
            .column(Column::initial(100.0))
            .column(Column::initial(90.0))
            .column(Column::initial(150.0))
            .column(Column::remainder().clip(true))
            .header(22.0, |mut h| {
                h.col(|_| {});
                h.col(|ui| header(ui, t("Nama"), Col::Name, &mut sort_click));
                h.col(|ui| header(ui, "Health", Col::Health, &mut sort_click));
                h.col(|ui| header(ui, "Status", Col::Status, &mut sort_click));
                h.col(|ui| header(ui, "Pri", Col::Pri, &mut sort_click));
                h.col(|ui| header(ui, "Rep", Col::Rep, &mut sort_click));
                h.col(|ui| header(ui, docs_header(), Col::Docs, &mut sort_click));
                h.col(|ui| header(ui, t("Ukuran"), Col::Size, &mut sort_click));
                h.col(|ui| header(ui, t("Dibuat"), Col::Created, &mut sort_click));
                h.col(|ui| {
                    ui.strong(t("Alias / data stream"));
                });
            })
            .body(|body| {
                body.rows(20.0, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    row.set_selected(selected.as_deref() == Some(r.name.as_str()));
                    row.col(|ui| {
                        if !r.system() && !read_only {
                            let mut on = checked.contains(&r.name);
                            if ui.checkbox(&mut on, "").changed() {
                                toggles.push(r.name.clone());
                            }
                        }
                    });
                    row.col(|ui| {
                        ui.label(&r.name);
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(format!("● {}", r.health)).color(health_color(&r.health)),
                        );
                    });
                    row.col(|ui| {
                        ui.label(&r.status);
                    });
                    row.col(|ui| {
                        ui.label(r.pri.to_string());
                    });
                    row.col(|ui| {
                        ui.label(r.rep.to_string());
                    });
                    row.col(|ui| {
                        ui.label(widgets::format_count(r.docs));
                    });
                    row.col(|ui| {
                        ui.label(widgets::format_bytes(r.size));
                    });
                    row.col(|ui| {
                        ui.label(if r.created > 0 {
                            widgets::format_time_ms(r.created)
                                .chars()
                                .take(16)
                                .collect::<String>()
                        } else {
                            "-".into()
                        });
                    });
                    row.col(|ui| {
                        let mut parts = r.aliases.clone();
                        if let Some(ds) = &r.data_stream {
                            parts.insert(0, format!("⛁ {ds}"));
                        }
                        ui.weak(parts.join(", "));
                    });
                    if row.response().clicked() {
                        clicked = Some(r.name.clone());
                    }
                });
            });
        for name in toggles {
            if !self.checked.remove(&name) {
                self.checked.insert(name);
            }
        }
        if let Some(col) = sort_click {
            self.sort = match self.sort {
                Some((c, asc)) if c == col => Some((col, !asc)),
                _ => Some((col, matches!(col, Col::Name | Col::Health | Col::Status))),
            };
        }
        if let Some(name) = clicked {
            self.tab = Tab::Overview;
            let ctx = ui.ctx().clone();
            self.select(&ctx, cx.conn, &name);
        }
    }

    fn select(&mut self, ctx: &egui::Context, conn: &Arc<Conn>, name: &str) {
        self.selected = Some(name.to_owned());
        self.detail = None;
        self.defaults_reload = false;
        let conn = conn.clone();
        let name = name.to_owned();
        let defaults = self.show_defaults;
        self.detail_pending = Some(Pending::spawn(ctx, move || {
            load_detail(&conn, &name, defaults)
        }));
    }

    /// IDX-11: muat ulang settings (toggle default) tanpa mengosongkan detail/form.
    fn reload_settings(&mut self, ctx: &egui::Context, conn: &Arc<Conn>, name: &str) {
        let conn = conn.clone();
        let name = name.to_owned();
        let defaults = self.show_defaults;
        self.defaults_reload = true;
        self.detail_pending = Some(Pending::spawn(ctx, move || {
            load_detail(&conn, &name, defaults)
        }));
    }

    fn detail_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, name: &str) {
        let row = self.rows.iter().find(|r| r.name == name).cloned();
        ui.horizontal(|ui| {
            ui.heading(name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    self.selected = None;
                    self.detail = None;
                }
            });
        });
        let system = name.starts_with('.');
        let privs = self.privs;
        let closed = row.as_ref().is_some_and(|r| r.status == "close");
        let busy = self.action_pending.is_some();
        ui.horizontal_wrapped(|ui| {
            if ui.button(t("Buka di Discover")).clicked() {
                *cx.nav = Some(Nav::Discover {
                    pattern: name.to_owned(),
                });
            }
            if ui.button(t("Buka di Dev Tools")).clicked() {
                *cx.nav = Some(Nav::Console {
                    method: "GET".into(),
                    path: format!("/{name}/_settings"),
                    body: None,
                });
            }
            if cx.conn.read_only {
                ui.weak(t("Profile read-only: aksi disembunyikan."));
                return;
            }
            ui.add_enabled_ui(!busy, |ui| {
                if !closed {
                    if action_button(ui, privs, "Refresh", &Action::Refresh(name.into())) {
                        self.run(ui.ctx(), cx.conn, Action::Refresh(name.into()));
                    }
                    if action_button(ui, privs, "Flush", &Action::Flush(name.into())) {
                        self.run(ui.ctx(), cx.conn, Action::Flush(name.into()));
                    }
                    if action_button(ui, privs, "Clear cache", &Action::ClearCache(name.into())) {
                        self.run(ui.ctx(), cx.conn, Action::ClearCache(name.into()));
                    }
                }
                if system {
                    ui.weak(t("Index sistem: aksi destruktif dikunci."));
                    return;
                }
                if closed {
                    if action_button(ui, privs, "Open", &Action::Open(name.into())) {
                        self.run(ui.ctx(), cx.conn, Action::Open(name.into()));
                    }
                } else if action_button(ui, privs, "Close", &Action::Close(name.into())) {
                    self.confirm = Some(Action::Close(name.into()));
                    self.confirm_typed.clear();
                }
                if !closed && ui.button("Clone/Shrink/Split…").clicked() {
                    let pri = row.as_ref().map_or(1, |r| r.pri);
                    self.resize_form = Some((
                        name.to_owned(),
                        "_clone",
                        format!("{name}-copy"),
                        pri as u32,
                        pri,
                    ));
                }
                if !closed
                    && ui
                        .button("By query…")
                        .on_hover_text(t("Hapus atau update dokumen yang cocok query"))
                        .clicked()
                {
                    self.by_query = Some(ByQueryForm {
                        index: name.to_owned(),
                        delete: true,
                        kql: String::new(),
                        script: String::new(),
                        count: None,
                        typed: String::new(),
                    });
                }
                if !closed
                    && ui
                        .button("Reindex…")
                        .on_hover_text(t("Salin dokumen ke index lain (async + progress)"))
                        .clicked()
                {
                    self.reindex_form =
                        Some((name.to_owned(), format!("{name}-reindex"), String::new()));
                }
                if !closed
                    && action_button(ui, privs, "Force merge", &Action::ForceMerge(name.into()))
                {
                    self.confirm = Some(Action::ForceMerge(name.into()));
                    self.confirm_typed.clear();
                }
                if let Some(ds) = row.as_ref().and_then(|r| r.data_stream.clone())
                    && action_button(
                        ui,
                        privs,
                        &format!("Rollover {ds}"),
                        &Action::Rollover(ds.clone()),
                    )
                {
                    self.confirm = Some(Action::Rollover(ds));
                    self.confirm_typed.clear();
                }
                let delete =
                    egui::Button::new(RichText::new(t("Hapus")).color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(0xc0, 0x39, 0x2b));
                let delete_missing = privs.missing(&Action::Delete(name.into()));
                if ui
                    .add_enabled(delete_missing.is_none(), delete)
                    .on_hover_text(Action::Delete(name.into()).describe())
                    .on_disabled_hover_text(tf(
                        "User tidak punya privilege `{0}` pada index ini.",
                        &[&delete_missing.unwrap_or_default()],
                    ))
                    .clicked()
                {
                    self.confirm = Some(Action::Delete(name.into()));
                    self.confirm_typed.clear();
                }
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Overview, "Overview");
            ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            ui.selectable_value(&mut self.tab, Tab::Mappings, "Mappings");
            ui.selectable_value(&mut self.tab, Tab::Aliases, "Alias");
            ui.selectable_value(&mut self.tab, Tab::Docs, documents_label());
            // PRF-11: shard & ILM tidak tersedia di serverless.
            if cx.caps.cluster_ops() {
                ui.selectable_value(&mut self.tab, Tab::Shards, "Shards");
                ui.selectable_value(&mut self.tab, Tab::Ilm, "ILM");
            }
        });
        self.poll_tabs();
        if self.tab == Tab::Shards || self.tab == Tab::Ilm {
            let ctx = ui.ctx().clone();
            self.tab_ui(ui, &ctx, cx, name);
            return;
        }
        ui.separator();
        let detail = match &self.detail {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(err)) => {
                widgets::error_box(ui, err);
                return;
            }
            Some(Ok(d)) if d.name != name => {
                ui.spinner();
                return;
            }
            Some(Ok(d)) => d,
        };
        let mut action = None;
        let mut reload_settings = false;
        egui::ScrollArea::vertical()
            .id_salt("index_detail_scroll")
            .auto_shrink(false)
            .show(ui, |ui| match self.tab {
                Tab::Overview => overview(ui, row.as_ref(), detail),
                Tab::Settings => {
                    if !cx.conn.read_only
                        && !closed
                        && let Some(form) = &mut self.settings_form
                    {
                        action = settings_editor(ui, name, form);
                        if !system
                            && ui
                                .button(t("Ubah setting statis…"))
                                .on_hover_text(t(
                                    "Butuh close → update → open (index tidak tersedia sesaat)",
                                ))
                                .clicked()
                        {
                            self.static_form =
                                Some((name.to_owned(), String::new(), String::new()));
                        }
                        ui.separator();
                    }
                    // IDX-11: tabel/JSON + muat default.
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.settings_json, false, t("Tabel"));
                        ui.selectable_value(&mut self.settings_json, true, "JSON");
                        ui.separator();
                        if ui
                            .checkbox(&mut self.show_defaults, t("Tampilkan default"))
                            .changed()
                        {
                            reload_settings = true;
                        }
                        if self.settings_json && ui.button(t("Salin")).clicked() {
                            ui.ctx().copy_text(detail.settings_text.clone());
                        }
                    });
                    if self.settings_json {
                        widgets::code_view(
                            ui,
                            &detail.settings_text,
                            widgets::Lang::Json,
                            &mut self.highlighter,
                        );
                    } else {
                        egui::Grid::new("settings_grid")
                            .striped(true)
                            .num_columns(3)
                            .show(ui, |ui| {
                                for (k, v, is_default) in &detail.settings_all {
                                    if *is_default {
                                        ui.label(RichText::new(k).monospace().weak());
                                        ui.weak(v);
                                        ui.weak("default");
                                    } else {
                                        ui.monospace(k);
                                        ui.label(v);
                                        ui.label("");
                                    }
                                    ui.end_row();
                                }
                            });
                    }
                }
                Tab::Mappings => {
                    if !cx.conn.read_only && !closed {
                        action = add_field_form(ui, name, &mut self.new_field);
                        ui.weak(t(
                            "Mengubah tipe field yang sudah ada butuh reindex ke index baru.",
                        ));
                        ui.separator();
                    }
                    // IDX-13: tree/JSON.
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.mappings_json, false, "Tree");
                        ui.selectable_value(&mut self.mappings_json, true, "JSON");
                        if self.mappings_json && ui.button(t("Salin")).clicked() {
                            ui.ctx().copy_text(detail.mappings_text.clone());
                        }
                    });
                    if self.mappings_json {
                        widgets::code_view(
                            ui,
                            &detail.mappings_text,
                            widgets::Lang::Json,
                            &mut self.highlighter,
                        );
                    } else {
                        widgets::json_tree(
                            ui,
                            &detail.mappings,
                            egui::Id::unique(("mapping", name)),
                        );
                    }
                }
                Tab::Shards | Tab::Ilm => {}
                Tab::Docs => {
                    if let Some(r) = poll(&mut self.docs_pending) {
                        self.docs = Some(r);
                    }
                    let stale = self.docs.as_ref().is_none_or(|(n, _)| n != name);
                    if stale && self.docs_pending.is_none() {
                        let conn = cx.conn.clone();
                        let n = name.to_owned();
                        self.docs_pending = Some(Pending::spawn(ui.ctx(), move || {
                            let r = conn
                                .get(&format!("/{n}/_search?size=20"))
                                .map(|v| v["hits"]["hits"].as_array().cloned().unwrap_or_default());
                            (n, r)
                        }));
                    }
                    match &self.docs {
                        Some((n, Ok(hits))) if n == name => {
                            if hits.is_empty() {
                                ui.weak(t("Index kosong."));
                            }
                            for hit in hits {
                                let id = hit["_id"].as_str().unwrap_or_default();
                                egui::CollapsingHeader::new(RichText::new(id).monospace())
                                    .id_salt(("doc_preview", name, id))
                                    .show(ui, |ui| {
                                        widgets::json_tree(
                                            ui,
                                            &hit["_source"],
                                            egui::Id::unique(("docp", name, id)),
                                        );
                                    });
                            }
                        }
                        Some((n, Err(err))) if n == name => widgets::error_box(ui, err),
                        _ => {
                            ui.spinner();
                        }
                    }
                }
                Tab::Aliases => {
                    action = aliases_ui(
                        ui,
                        privs,
                        name,
                        row.as_ref(),
                        &mut self.alias_input,
                        cx.conn.read_only,
                    );
                }
            });
        if reload_settings {
            let ctx = ui.ctx().clone();
            self.reload_settings(&ctx, cx.conn, name);
        }
        if let Some(action) = action {
            self.run(ui.ctx(), cx.conn, action);
        }
    }

    fn poll_tabs(&mut self) {
        if let Some(r) = poll(&mut self.shards_pending) {
            self.shards = Some(r);
        }
        if let Some(r) = poll(&mut self.explain_pending) {
            self.explain = Some(r);
        }
        if let Some(r) = poll(&mut self.ilm_pending) {
            self.ilm = Some(r);
        }
    }

    /// Tab Shards (IDX-15) dan ILM (IDX-16).
    fn tab_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, cx: &mut Ctx, name: &str) {
        let reload = ui.button(t("⟳ Muat ulang")).clicked();
        if self.tab == Tab::Shards {
            let stale = self.shards.as_ref().is_none_or(|(n, _)| n != name);
            if (stale || reload) && self.shards_pending.is_none() {
                let conn = cx.conn.clone();
                let n = name.to_owned();
                self.explain = None;
                self.shards_pending = Some(Pending::spawn(ctx, move || {
                    let path = format!(
                        "/_cat/shards/{n}?format=json&bytes=b&h=index,shard,prirep,state,docs,store,node,unassigned.reason"
                    );
                    let result = conn
                        .get(&path)
                        .map(|v| v.as_array().cloned().unwrap_or_default());
                    (n, result)
                }));
            }
            self.shards_ui(ui, ctx, cx);
        } else {
            let stale = self.ilm.as_ref().is_none_or(|(n, _)| n != name);
            if (stale || reload) && self.ilm_pending.is_none() {
                let conn = cx.conn.clone();
                let n = name.to_owned();
                self.ilm_pending = Some(Pending::spawn(ctx, move || {
                    let result = conn
                        .get(&format!("/{n}/_ilm/explain"))
                        .map(|v| v["indices"][&n].clone());
                    (n, result)
                }));
            }
            self.ilm_ui(ui, cx, name);
        }
    }

    fn shards_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, cx: &mut Ctx) {
        let rows = match &self.shards {
            Some((_, Ok(rows))) => rows.clone(),
            Some((_, Err(err))) => {
                widgets::error_box(ui, err);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        let mut explain = None;
        egui::ScrollArea::vertical()
            .id_salt("shards")
            .show(ui, |ui| {
                egui::Grid::new("shards_grid")
                    .striped(true)
                    .num_columns(7)
                    .show(ui, |ui| {
                        for h in ["Shard", "Tipe", "State", "Dokumen", "Ukuran", "Node", ""] {
                            ui.strong(if h == "Dokumen" { docs_header() } else { t(h) });
                        }
                        ui.end_row();
                        for r in &rows {
                            let state = r["state"].as_str().unwrap_or("-");
                            let primary = r["prirep"] == "p";
                            ui.label(r["shard"].as_str().unwrap_or("-"));
                            ui.label(if primary { t("primer") } else { t("replika") });
                            let color = match state {
                                "STARTED" => egui::Color32::from_rgb(0x2e, 0xa0, 0x43),
                                "UNASSIGNED" => egui::Color32::from_rgb(0xd0, 0x3b, 0x3b),
                                _ => egui::Color32::from_rgb(0xd4, 0xa7, 0x2c),
                            };
                            let state_label = ui.label(RichText::new(state).color(color));
                            if let Some(reason) = r["unassigned.reason"].as_str() {
                                state_label.on_hover_text(tf("Alasan: {0}", &[&reason]));
                            }
                            ui.label(widgets::format_count(num(&r["docs"])));
                            ui.label(widgets::format_bytes(num(&r["store"]) as f64));
                            ui.label(r["node"].as_str().unwrap_or("-"));
                            if state == "UNASSIGNED" && ui.small_button("Explain").clicked() {
                                explain = Some(json!({
                                    "index": r["index"],
                                    "shard": num(&r["shard"]),
                                    "primary": primary,
                                }));
                            }
                            ui.end_row();
                        }
                    });
                if let Some(body) = explain {
                    let conn = cx.conn.clone();
                    self.explain = None;
                    self.explain_pending = Some(Pending::spawn(ctx, move || {
                        conn.json("GET", "/_cluster/allocation/explain", Some(&body))
                    }));
                }
                if self.explain_pending.is_some() {
                    ui.spinner();
                }
                match &self.explain {
                    Some(Ok(v)) => {
                        ui.separator();
                        ui.strong("Allocation explain");
                        if let Some(text) = v["allocate_explanation"].as_str() {
                            ui.label(text);
                        }
                        if let Some(reason) = v["unassigned_info"]["reason"].as_str() {
                            ui.weak(tf("Alasan unassigned: {0}", &[&reason]));
                        }
                        for node in v["node_allocation_decisions"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            let decisions: Vec<String> = node["deciders"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter(|d| d["decision"] == "NO")
                                .map(|d| {
                                    format!(
                                        "• {}: {}",
                                        d["decider"].as_str().unwrap_or("?"),
                                        d["explanation"].as_str().unwrap_or("")
                                    )
                                })
                                .collect();
                            ui.label(
                                RichText::new(format!(
                                    "{} → {}",
                                    node["node_name"].as_str().unwrap_or("?"),
                                    node["node_decision"].as_str().unwrap_or("?")
                                ))
                                .strong(),
                            );
                            for d in decisions {
                                ui.add(egui::Label::new(d).wrap());
                            }
                        }
                        egui::CollapsingHeader::new(t("Response lengkap")).show(ui, |ui| {
                            widgets::json_tree(ui, v, egui::Id::unique("explain_tree"));
                        });
                    }
                    Some(Err(err)) => widgets::error_box(ui, err),
                    None => {}
                }
            });
    }

    fn ilm_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx, name: &str) {
        let v = match &self.ilm {
            Some((_, Ok(v))) => v.clone(),
            Some((_, Err(err))) => {
                widgets::error_box(ui, err);
                return;
            }
            None => {
                ui.spinner();
                return;
            }
        };
        if v["managed"] != json!(true) {
            ui.weak(t("Index ini tidak dikelola ILM."));
            return;
        }
        egui::Grid::new("ilm_grid")
            .striped(true)
            .num_columns(2)
            .show(ui, |ui| {
                for (label, key) in [
                    ("Policy", "policy"),
                    ("Phase", "phase"),
                    ("Action", "action"),
                    ("Step", "step"),
                    ("Umur", "age"),
                ] {
                    ui.label(t(label));
                    ui.label(v[key].as_str().unwrap_or("-"));
                    ui.end_row();
                }
            });
        if v["step"] == "ERROR" || v.get("step_info").is_some_and(|i| i.get("type").is_some()) {
            let info = &v["step_info"];
            widgets::error_box(
                ui,
                &format!(
                    "{}: {}",
                    info["type"].as_str().unwrap_or("error"),
                    info["reason"].as_str().unwrap_or("")
                ),
            );
            let action = Action::IlmRetry(name.into());
            if !cx.conn.read_only && action_button(ui, self.privs, t("Coba ulang ILM"), &action) {
                self.run(ui.ctx(), cx.conn, action);
            }
        }
        egui::CollapsingHeader::new(t("Response lengkap")).show(ui, |ui| {
            widgets::json_tree(ui, &v, egui::Id::unique("ilm_tree"));
        });
    }

    fn run(&mut self, ctx: &egui::Context, conn: &Arc<Conn>, action: Action) {
        let conn = conn.clone();
        self.message = None;
        self.action_pending = Some(Pending::spawn(ctx, move || {
            let mut result = Ok(Value::Null);
            for (method, path, body) in action.steps() {
                result = conn.json(method, &path, body.as_ref());
                if result.is_err() {
                    break;
                }
            }
            // IDX-12: jangan biarkan index tertutup bila update setting gagal.
            if let (Action::StaticSettings(n, _), Err(e)) = (&action, &result) {
                let reopened = conn.json("POST", &format!("/{n}/_open"), None);
                let note = if reopened.is_ok() {
                    t("index sudah dibuka kembali")
                } else {
                    t("GAGAL membuka kembali index, buka manual")
                };
                result = Err(format!("{e}\n({note})"));
            }
            (action, result)
        }));
    }

    fn static_dialog(&mut self, ctx: &egui::Context) {
        let Some((index, key, value)) = &mut self.static_form else {
            return;
        };
        let mut submit = None;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("static_settings")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading(tf("Setting statis · {0}", &[&index]));
            widgets::warn_box(ui, t("Index akan ditutup sesaat (tidak bisa dicari/ditulis), diubah, lalu dibuka kembali."));
            egui::Grid::new("static_grid").num_columns(2).show(ui, |ui| {
                ui.label(t("Setting"));
                ui.add(egui::TextEdit::singleline(key).hint_text("index.codec"));
                ui.end_row();
                ui.label(t("Nilai (JSON)"));
                ui.add(egui::TextEdit::singleline(value).hint_text("\"best_compression\""));
                ui.end_row();
            });
            ui.weak(t("Nilai berupa JSON: string pakai kutip, angka/boolean/objek apa adanya (mis. analysis)."));
            let parsed: Result<Value, String> = serde_json::from_str(value).map_err(|e| tf("Nilai bukan JSON: {0}", &[&e]));
            let key_ok = key.trim().starts_with("index.") || key.trim().starts_with("analysis");
            if !key_ok {
                ui.colored_label(ui.visuals().error_fg_color, t("Nama setting diawali index. atau analysis"));
            }
            if let Err(e) = &parsed {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let action = parsed.ok().filter(|_| key_ok).map(|v| {
                let mut body = serde_json::Map::new();
                body.insert(key.trim().to_owned(), v);
                Action::StaticSettings(index.clone(), Value::Object(body))
            });
            ui.horizontal(|ui| {
                let hover = action.as_ref().map(Action::describe).unwrap_or_default();
                if ui.add_enabled(action.is_some(), egui::Button::new(t("Terapkan"))).on_hover_text(hover).clicked() {
                    submit = action;
                }
                if ui.button(t("Batal")).clicked() {
                    close = true;
                }
            });
        });
        if let Some(action) = submit {
            self.static_form = None;
            // Selalu konfirmasi: index tidak tersedia sesaat.
            self.confirm = Some(action);
            self.confirm_typed.clear();
        }
        if close {
            self.static_form = None;
        }
    }

    fn resize_dialog(&mut self, ctx: &egui::Context) {
        let Some((source, op, target, shards, pri)) = &mut self.resize_form else {
            return;
        };
        let mut submit = None;
        let mut close = false;
        egui::Modal::new(egui::Id::unique("resize_index")).show(ctx, |ui| {
            ui.set_width(480.0);
            ui.heading(tf("Clone / Shrink / Split · {0}", &[&source]));
            ui.horizontal(|ui| {
                ui.selectable_value(op, "_clone", "Clone");
                ui.selectable_value(op, "_shrink", "Shrink");
                ui.selectable_value(op, "_split", "Split");
            });
            egui::Grid::new("resize_grid").num_columns(2).show(ui, |ui| {
                ui.label(t("Index tujuan"));
                ui.text_edit_singleline(target);
                ui.end_row();
                if *op != "_clone" {
                    ui.label(t("Shard primer"));
                    ui.add(egui::DragValue::new(shards).range(1..=1024));
                    ui.end_row();
                }
            });
            ui.weak(tf("Shard primer sumber: {0}. {1} akan diblok tulis (index.blocks.write) sebelum operasi.", &[&pri, &source]));
            let shard_err = resize_shard_error(op, u64::from(*shards), *pri);
            if *op == "_shrink" {
                ui.weak(t("Shrink butuh satu salinan setiap shard di satu node yang sama (otomatis di cluster single-node)."));
            }
            let name_err = validate_index_name(target.trim()).err();
            for err in [&shard_err, &name_err].into_iter().flatten() {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            let action = Action::Resize {
                op,
                source: source.clone(),
                target: target.trim().to_owned(),
                shards: (*op != "_clone").then_some(*shards),
            };
            ui.horizontal(|ui| {
                let ok = shard_err.is_none() && name_err.is_none();
                if ui.add_enabled(ok, egui::Button::new(t("Jalankan"))).on_hover_text(action.describe()).clicked() {
                    submit = Some(action);
                }
                if ui.button(t("Batal")).clicked() {
                    close = true;
                }
            });
        });
        if let Some(action) = submit {
            self.resize_form = None;
            self.confirm = Some(action);
            self.confirm_typed.clear();
        }
        if close {
            self.resize_form = None;
        }
    }

    /// IDX-36: pantau task reindex tiap detik sampai selesai.
    fn task_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if let Some(r) = poll(&mut self.task_pending)
            && let Some((_, status)) = &mut self.task
        {
            *status = Some(r);
        }
        let Some((id, status)) = &self.task else {
            return;
        };
        let id = id.clone();
        let done = matches!(status, Some(Ok(v)) if v["completed"] == json!(true));
        if !done
            && self.task_pending.is_none()
            && self
                .task_polled
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.task_polled = Some(Instant::now());
            let conn = cx.conn.clone();
            let path = format!("/_tasks/{id}");
            self.task_pending = Some(Pending::spawn(ui.ctx(), move || conn.get(&path)));
        }
        if !done {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        let mut close = false;
        let mut cancel = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                let st = match status {
                    Some(Ok(v)) => v["task"]["status"].clone(),
                    _ => Value::Null,
                };
                let total = st["total"].as_u64().unwrap_or(0);
                let processed = st["created"].as_u64().unwrap_or(0)
                    + st["updated"].as_u64().unwrap_or(0)
                    + st["deleted"].as_u64().unwrap_or(0)
                    + st["noops"].as_u64().unwrap_or(0);
                let pct = if total > 0 {
                    processed as f32 / total as f32
                } else {
                    0.0
                };
                ui.label(RichText::new(tf("{0} task {1}", &[&self.task_label, &id])).strong());
                ui.add(
                    egui::ProgressBar::new(if done { 1.0 } else { pct })
                        .desired_width(240.0)
                        .text(format!(
                            "{} / {}",
                            widgets::format_count(processed),
                            widgets::format_count(total)
                        )),
                );
                if done {
                    let failures = status
                        .as_ref()
                        .and_then(|r| r.as_ref().ok())
                        .and_then(|v| v["response"]["failures"].as_array())
                        .map_or(0, Vec::len);
                    if failures > 0 {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            tf("selesai, {0} gagal", &[&failures]),
                        );
                    } else {
                        ui.label(t("selesai"));
                    }
                    close = ui.small_button(t("Tutup")).clicked();
                } else {
                    ui.spinner();
                    cancel = !cx.conn.read_only && ui.small_button(t("Batalkan")).clicked();
                }
            });
            if let Some(Err(err)) = status {
                widgets::error_box(ui, err);
            }
        });
        if cancel {
            let conn = cx.conn.clone();
            std::thread::spawn(move || {
                let _ = conn.json("POST", &format!("/_tasks/{id}/_cancel"), None);
            });
        }
        if close {
            self.task = None;
            self.loaded = None;
        }
    }

    /// IDX-05: aksi untuk index yang dicentang.
    fn bulk_bar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if self.checked.is_empty() || cx.conn.read_only {
            return;
        }
        let names = self.checked.iter().cloned().collect::<Vec<_>>().join(",");
        let privs = Privs::default();
        ui.horizontal(|ui| {
            ui.strong(tf("{0} dipilih:", &[&self.checked.len()]));
            for (label, action) in [
                ("Refresh", Action::Refresh(names.clone())),
                ("Flush", Action::Flush(names.clone())),
                ("Clear cache", Action::ClearCache(names.clone())),
                ("Open", Action::Open(names.clone())),
            ] {
                if action_button(ui, privs, label, &action) {
                    self.run(ui.ctx(), cx.conn, action);
                }
            }
            for (label, action) in [
                ("Close", Action::Close(names.clone())),
                ("Hapus", Action::Delete(names.clone())),
            ] {
                if action_button(ui, privs, t(label), &action) {
                    self.confirm = Some(action);
                    self.confirm_typed.clear();
                }
            }
            if ui.small_button(t("Batal pilih")).clicked() {
                self.checked.clear();
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, cx: &mut Ctx) {
        self.static_dialog(ctx);
        self.resize_dialog(ctx);
        let mut start = None;
        let mut cancel_form = false;
        if let Some((source, dest, kql)) = &mut self.reindex_form {
            egui::Modal::new(egui::Id::unique("reindex_form")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Reindex");
                egui::Grid::new("reindex_grid").num_columns(2).show(ui, |ui| {
                    ui.label(t("Sumber"));
                    ui.text_edit_singleline(source);
                    ui.end_row();
                    ui.label(t("Tujuan"));
                    ui.text_edit_singleline(dest);
                    ui.end_row();
                    ui.label("Query (KQL)");
                    ui.add(egui::TextEdit::singleline(kql).hint_text(t("opsional, mis. status:active")));
                    ui.end_row();
                });
                let query = crate::es::kql::to_dsl(kql);
                let valid = validate_index_name(dest.trim()).and_then(|_| query.clone().map(|_| ()));
                if let Err(err) = &valid {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                if dest.trim() == source.trim() {
                    ui.colored_label(ui.visuals().error_fg_color, t("Tujuan harus berbeda dari sumber."));
                }
                ui.weak(t("Index tujuan dibuat otomatis jika belum ada (mapping dinamis). Buat dulu dengan mapping yang benar bila perlu."));
                ui.horizontal(|ui| {
                    let ok = valid.is_ok() && dest.trim() != source.trim();
                    let action = query.ok().map(|q| Action::Reindex {
                        source: source.trim().to_owned(),
                        dest: dest.trim().to_owned(),
                        query: q,
                    });
                    let hover = action.as_ref().map(Action::describe).unwrap_or_default();
                    if ui.add_enabled(ok, egui::Button::new(t("Mulai reindex"))).on_hover_text(hover).clicked() {
                        start = action;
                    }
                    if ui.button(t("Batal")).clicked() {
                        cancel_form = true;
                    }
                });
            });
        }
        if let Some(r) = poll(&mut self.count_pending)
            && let Some(form) = &mut self.by_query
        {
            form.count = Some(r);
        }
        let mut by_query_start = None;
        let mut by_query_close = false;
        let mut count_request = None;
        if let Some(form) = &mut self.by_query {
            egui::Modal::new(egui::Id::unique("by_query_form")).show(ctx, |ui| {
                ui.set_width(520.0);
                ui.heading(tf("By query · {0}", &[&form.index]));
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut form.delete, true, t("Hapus dokumen"));
                    ui.selectable_value(&mut form.delete, false, t("Update dokumen"));
                });
                ui.label(t("Query (KQL; kosong = semua dokumen)"));
                if ui.add(egui::TextEdit::singleline(&mut form.kql).desired_width(f32::INFINITY)).changed() {
                    form.count = None;
                }
                if !form.delete {
                    ui.label(t("Script Painless (opsional, mis. ctx._source.status = 'arsip')"));
                    ui.add(egui::TextEdit::multiline(&mut form.script).code_editor().desired_rows(4).desired_width(f32::INFINITY));
                }
                let query = crate::es::kql::to_dsl(&form.kql);
                if let Err(err) = &query {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(query.is_ok(), egui::Button::new(t("Hitung dokumen"))).clicked()
                        && let Ok(q) = &query
                    {
                        count_request = Some((form.index.clone(), q.clone()));
                    }
                    match &form.count {
                        Some(Ok(n)) => {
                            ui.strong(tf("{0} dokumen cocok", &[&widgets::format_count(*n)]));
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                        None => {}
                    }
                });
                // Hapus massal wajib dihitung dulu dan dikonfirmasi dengan mengetik HAPUS.
                if form.delete {
                    ui.label(t("Ketik HAPUS untuk konfirmasi:"));
                    ui.text_edit_singleline(&mut form.typed);
                }
                ui.weak(t("Berjalan async dengan conflicts=proceed; progress tampil di atas daftar index."));
                ui.horizontal(|ui| {
                    let action = query.clone().ok().map(|q| Action::ByQuery {
                        index: form.index.clone(),
                        delete: form.delete,
                        query: q,
                        script: (!form.delete).then(|| form.script.clone()),
                    });
                    let counted = matches!(form.count, Some(Ok(_)));
                    let ok = action.is_some() && (!form.delete || (counted && form.typed.trim() == "HAPUS"));
                    let hover = action.as_ref().map(Action::describe).unwrap_or_default();
                    if ui.add_enabled(ok, egui::Button::new(t("Jalankan"))).on_hover_text(hover).clicked() {
                        by_query_start = action;
                    }
                    if ui.button(t("Batal")).clicked() {
                        by_query_close = true;
                    }
                });
            });
        }
        if let Some((index, query)) = count_request {
            let conn = cx.conn.clone();
            self.count_pending = Some(Pending::spawn(ctx, move || {
                conn.json(
                    "POST",
                    &format!("/{index}/_count"),
                    Some(&json!({"query": query})),
                )
                .map(|v| v["count"].as_u64().unwrap_or(0))
            }));
        }
        if let Some(action) = by_query_start {
            self.by_query = None;
            self.run(ctx, cx.conn, action);
        }
        if by_query_close {
            self.by_query = None;
        }
        if let Some(action) = start {
            self.reindex_form = None;
            self.run(ctx, cx.conn, action);
        }
        if cancel_form {
            self.reindex_form = None;
        }
        if let Some(action) = self.confirm.clone() {
            let prod = cx.profile.is_prod();
            let (title, message, expected) = match &action {
                Action::Delete(n) => (
                    t("Hapus index?"),
                    tf(
                        "Index {0} dan semua dokumennya akan dihapus permanen. Aksi ini tidak bisa dibatalkan.",
                        &[n],
                    ),
                    Some(if n.contains(',') { "HAPUS" } else { n.as_str() }),
                ),
                Action::StaticSettings(n, _) => (
                    t("Ubah setting statis?"),
                    tf(
                        "{0} akan ditutup, diubah, lalu dibuka kembali (sementara tidak tersedia).",
                        &[n],
                    ),
                    Some(n.as_str()),
                ),
                Action::Resize { source, target, .. } => (
                    t("Clone/Shrink/Split?"),
                    tf(
                        "{0} akan diblok tulis, lalu {1} dibuat dari {0}.",
                        &[source, target],
                    ),
                    None,
                ),
                Action::ForceMerge(n) => (
                    "Force merge?",
                    tf(
                        "Force merge {0} ke 1 segment memakan I/O dan CPU besar. Jangan jalankan pada index yang masih aktif ditulisi.",
                        &[n],
                    ),
                    None,
                ),
                Action::Rollover(target) => (
                    t("Rollover?"),
                    tf(
                        "{0} akan menulis ke backing index baru mulai sekarang.",
                        &[target],
                    ),
                    None,
                ),
                Action::Close(n) => (
                    t("Tutup index?"),
                    tf(
                        "Index {0} tidak bisa dicari atau ditulisi sampai dibuka kembali.",
                        &[n],
                    ),
                    prod.then_some(n.as_str()),
                ),
                _ => (t("Lanjutkan?"), String::new(), None),
            };
            let message = format!("{message}\n\nRequest:\n{}", action.describe());
            match widgets::confirm_dialog(
                ctx,
                title,
                &message,
                expected,
                &mut self.confirm_typed,
                true,
            ) {
                Confirm::Yes => {
                    self.confirm = None;
                    self.run(ctx, cx.conn, action);
                }
                Confirm::No => self.confirm = None,
                Confirm::Pending => {}
            }
        }
        let mut submit = None;
        let mut cancel = false;
        if let Some(form) = &mut self.create {
            egui::Modal::new(egui::Id::unique("create_index")).show(ctx, |ui| {
                ui.set_width(480.0);
                ui.heading(t("Buat index"));
                egui::Grid::new("create_form")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(t("Nama"));
                        ui.text_edit_singleline(&mut form.name);
                        ui.end_row();
                        ui.label(t("Shard primer"));
                        ui.add(egui::DragValue::new(&mut form.shards).range(1..=1024));
                        ui.end_row();
                        ui.label(t("Replika"));
                        ui.add(egui::DragValue::new(&mut form.replicas).range(0..=16));
                        ui.end_row();
                    });
                ui.label(t("Mappings (JSON, opsional)"));
                ui.add(
                    egui::TextEdit::multiline(&mut form.mappings)
                        .code_editor()
                        .desired_rows(8)
                        .desired_width(f32::INFINITY)
                        .hint_text("{\"properties\": {\"message\": {\"type\": \"text\"}}}"),
                );
                let validation =
                    validate_index_name(form.name.trim()).and_then(|_| create_body(form));
                if let Err(err) = &validation {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(validation.is_ok(), egui::Button::new(t("Buat")))
                        .clicked()
                        && let Ok(body) = validation
                    {
                        submit = Some(Action::Create {
                            name: form.name.trim().to_owned(),
                            body,
                        });
                    }
                    if ui.button(t("Batal")).clicked() {
                        cancel = true;
                    }
                });
            });
        }
        if let Some(action) = submit {
            self.create = None;
            self.run(ctx, cx.conn, action);
        }
        if cancel {
            self.create = None;
        }
    }
}

/// IDX-14: daftar alias + tambah/hapus atomik lewat `POST /_aliases`.
fn aliases_ui(
    ui: &mut egui::Ui,
    privs: Privs,
    name: &str,
    row: Option<&Row>,
    input: &mut (String, bool),
    read_only: bool,
) -> Option<Action> {
    let mut action = None;
    let aliases = row.map(|r| r.aliases.clone()).unwrap_or_default();
    if aliases.is_empty() {
        ui.weak(t("Index ini belum punya alias."));
    }
    for alias in &aliases {
        ui.horizontal(|ui| {
            ui.monospace(alias);
            let remove = Action::Aliases(
                name.into(),
                json!({"actions": [{"remove": {"index": name, "alias": alias}}]}),
            );
            if !read_only && action_button(ui, privs, t("Hapus"), &remove) {
                action = Some(remove);
            }
        });
    }
    if read_only {
        return action;
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut input.0).hint_text(t("nama alias")).desired_width(180.0));
        ui.checkbox(&mut input.1, "write index");
        let alias = input.0.trim();
        let add = Action::Aliases(
            name.into(),
            json!({"actions": [{"add": {"index": name, "alias": alias, "is_write_index": input.1}}]}),
        );
        let valid = validate_index_name(alias).is_ok();
        if ui
            .add_enabled(valid, egui::Button::new(t("Tambah alias")))
            .on_hover_text(tf("Request:\n{0}", &[&add.describe()]))
            .clicked()
        {
            action = Some(add);
            input.0.clear();
        }
    });
    action
}

/// Tombol aksi; hover menampilkan request ES yang akan dikirim.
fn action_button(ui: &mut egui::Ui, privs: Privs, label: &str, action: &Action) -> bool {
    match privs.missing(action) {
        None => ui
            .button(label)
            .on_hover_text(tf("Request:\n{0}", &[&action.describe()]))
            .clicked(),
        Some(need) => {
            ui.add_enabled(false, egui::Button::new(label))
                .on_disabled_hover_text(tf(
                    "User tidak punya privilege `{0}` pada index ini.",
                    &[&need],
                ));
            false
        }
    }
}

fn create_body(form: &CreateForm) -> Result<Value, String> {
    let mut body =
        json!({"settings": {"number_of_shards": form.shards, "number_of_replicas": form.replicas}});
    if !form.mappings.trim().is_empty() {
        let mappings: Value = serde_json::from_str(&form.mappings)
            .map_err(|e| tf("Mappings tidak valid: {0}", &[&e]))?;
        body["mappings"] = mappings;
    }
    Ok(body)
}

fn overview(ui: &mut egui::Ui, row: Option<&Row>, detail: &Detail) {
    let p = &detail.stats["primaries"];
    let tot = &detail.stats["total"];
    let n = |v: &Value| widgets::format_count(v.as_u64().unwrap_or(0));
    egui::Grid::new("overview_grid")
        .striped(true)
        .num_columns(2)
        .show(ui, |ui| {
            if let Some(r) = row {
                for (k, v) in [
                    ("Health", r.health.clone()),
                    ("Status", r.status.clone()),
                    (
                        t("Shard primer / replika"),
                        format!("{} / {}", r.pri, r.rep),
                    ),
                    (documents_label(), widgets::format_count(r.docs)),
                    (t("Dokumen terhapus"), widgets::format_count(r.deleted)),
                    (t("Ukuran total"), widgets::format_bytes(r.size)),
                    (
                        "Alias",
                        if r.aliases.is_empty() {
                            "-".into()
                        } else {
                            r.aliases.join(", ")
                        },
                    ),
                    (
                        "Data stream",
                        r.data_stream.clone().unwrap_or_else(|| "-".into()),
                    ),
                ] {
                    ui.label(k);
                    ui.label(v);
                    ui.end_row();
                }
            }
            for (k, v) in [
                (
                    t("Ukuran primer"),
                    widgets::format_bytes(p["store"]["size_in_bytes"].as_f64().unwrap_or(0.0)),
                ),
                ("Total indexing", n(&tot["indexing"]["index_total"])),
                ("Total query", n(&tot["search"]["query_total"])),
                ("Segment", n(&tot["segments"]["count"])),
                ("Refresh total", n(&tot["refresh"]["total"])),
            ] {
                ui.label(k);
                ui.label(v);
                ui.end_row();
            }
        });
}

fn settings_form(detail: &Detail) -> SettingsForm {
    let get = |k: &str| {
        detail
            .settings
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    SettingsForm {
        replicas: get("index.number_of_replicas"),
        refresh_interval: get("index.refresh_interval"),
        max_result_window: get("index.max_result_window"),
        block_write: get("index.blocks.write") == "true",
        read_only: get("index.blocks.read_only") == "true",
    }
}

fn settings_editor(ui: &mut egui::Ui, name: &str, form: &mut SettingsForm) -> Option<Action> {
    ui.label(RichText::new(t("Setting dinamis")).strong());
    egui::Grid::new("settings_form")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("number_of_replicas");
            ui.text_edit_singleline(&mut form.replicas);
            ui.end_row();
            ui.label("refresh_interval");
            ui.add(
                egui::TextEdit::singleline(&mut form.refresh_interval)
                    .hint_text(t("default 1s, -1 = mati")),
            );
            ui.end_row();
            ui.label("max_result_window");
            ui.add(
                egui::TextEdit::singleline(&mut form.max_result_window)
                    .hint_text(t("default 10000")),
            );
            ui.end_row();
            ui.label("blocks.write");
            ui.checkbox(&mut form.block_write, "");
            ui.end_row();
            ui.label("blocks.read_only");
            ui.checkbox(&mut form.read_only, "");
            ui.end_row();
        });
    let mut index = serde_json::Map::new();
    let mut error = None;
    if !form.replicas.trim().is_empty() {
        match form.replicas.trim().parse::<u32>() {
            Ok(n) => {
                index.insert("number_of_replicas".into(), json!(n));
            }
            Err(_) => error = Some(t("number_of_replicas harus angka.")),
        }
    }
    if !form.refresh_interval.trim().is_empty() {
        index.insert(
            "refresh_interval".into(),
            json!(form.refresh_interval.trim()),
        );
    }
    if !form.max_result_window.trim().is_empty() {
        match form.max_result_window.trim().parse::<u64>() {
            Ok(n) => {
                index.insert("max_result_window".into(), json!(n));
            }
            Err(_) => error = Some(t("max_result_window harus angka.")),
        }
    }
    index.insert("blocks.write".into(), json!(form.block_write));
    index.insert("blocks.read_only".into(), json!(form.read_only));
    if let Some(err) = error {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
    let action = Action::UpdateSettings(name.to_owned(), json!({"index": index}));
    let clicked = ui
        .add_enabled(error.is_none(), egui::Button::new(t("Simpan settings")))
        .on_hover_text(tf("Request:\n{0}", &[&action.describe()]))
        .clicked();
    clicked.then_some(action)
}

fn add_field_form(ui: &mut egui::Ui, name: &str, field: &mut (String, String)) -> Option<Action> {
    ui.label(RichText::new(t("Tambah field")).strong());
    if field.1.is_empty() {
        field.1 = "keyword".into();
    }
    let mut add = None;
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut field.0)
                .hint_text(t("nama.field"))
                .desired_width(200.0),
        );
        egui::ComboBox::from_id_salt("new_field_type")
            .selected_text(field.1.as_str())
            .show_ui(ui, |ui| {
                for t in FIELD_TYPES {
                    ui.selectable_value(&mut field.1, t.to_owned(), t);
                }
            });
        let action = Action::AddField(
            name.to_owned(),
            json!({"properties": {field.0.trim(): {"type": field.1}}}),
        );
        add = ui
            .add_enabled(!field.0.trim().is_empty(), egui::Button::new(t("Tambah")))
            .on_hover_text(tf("Request:\n{0}", &[&action.describe()]))
            .clicked()
            .then_some(action);
    });
    if add.is_some() {
        field.0.clear();
    }
    add
}

fn health_color(health: &str) -> egui::Color32 {
    match health {
        "green" => egui::Color32::from_rgb(0x2e, 0xa0, 0x43),
        "yellow" => egui::Color32::from_rgb(0xd4, 0xa7, 0x2c),
        "red" => egui::Color32::from_rgb(0xd0, 0x3b, 0x3b),
        _ => egui::Color32::GRAY,
    }
}

fn num(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn load_rows(conn: &Conn) -> Result<Vec<Row>, String> {
    // Kolom tertentu tidak ada di semua flavor (mis. serverless): ulangi tanpa `h=`.
    let cat = conn
        .get(
            "/_cat/indices?format=json&bytes=b&expand_wildcards=all\
             &h=index,health,status,pri,rep,docs.count,docs.deleted,store.size,creation.date",
        )
        .or_else(|_| conn.get("/_cat/indices?format=json&bytes=b"))?;
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(value) = conn.get("/_alias?expand_wildcards=all") {
        for (index, v) in value.as_object().into_iter().flatten() {
            let names: Vec<String> = v["aliases"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, _)| k.clone())
                .collect();
            aliases.insert(index.clone(), names);
        }
    }
    let mut streams: HashMap<String, String> = HashMap::new();
    if let Ok(value) = conn.get("/_data_stream?expand_wildcards=all") {
        for ds in value["data_streams"].as_array().into_iter().flatten() {
            let name = ds["name"].as_str().unwrap_or_default().to_owned();
            for idx in ds["indices"].as_array().into_iter().flatten() {
                if let Some(index) = idx["index_name"].as_str() {
                    streams.insert(index.to_owned(), name.clone());
                }
            }
        }
    }
    let mut rows: Vec<Row> = cat
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let name = r["index"].as_str().unwrap_or_default().to_owned();
            Row {
                health: r["health"].as_str().unwrap_or("-").to_owned(),
                status: r["status"].as_str().unwrap_or("-").to_owned(),
                pri: num(&r["pri"]),
                rep: num(&r["rep"]),
                docs: num(&r["docs.count"]),
                deleted: num(&r["docs.deleted"]),
                size: num(&r["store.size"]) as f64,
                created: num(&r["creation.date"]) as i64,
                aliases: aliases.remove(&name).unwrap_or_default(),
                data_stream: streams.remove(&name),
                name,
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

fn load_detail(conn: &Conn, name: &str, include_defaults: bool) -> Result<Detail, String> {
    // Tanpa flat_settings: bentuk bersarang untuk tampilan JSON; flat dihitung lokal.
    let path = if include_defaults {
        format!("/{name}/_settings?include_defaults=true")
    } else {
        format!("/{name}/_settings")
    };
    let resp = conn.get(&path)?;
    let entry = &resp[name];
    let settings_all = flatten_settings(entry, include_defaults);
    let flat = settings_all
        .iter()
        .filter(|(_, _, d)| !d)
        .map(|(k, v, _)| (k.clone(), v.clone()))
        .collect();
    let mut settings_raw = json!({"settings": entry["settings"].clone()});
    if include_defaults {
        settings_raw["defaults"] = entry["defaults"].clone();
    }
    let mappings = conn
        .get(&format!("/{name}/_mapping"))
        .map(|v| v[name]["mappings"].clone())
        .unwrap_or(Value::Null);
    let stats = conn
        .get(&format!("/{name}/_stats"))
        .map(|v| v["indices"][name].clone())
        .unwrap_or(Value::Null);
    Ok(Detail {
        name: name.to_owned(),
        stats,
        settings: flat,
        settings_all,
        settings_text: pretty(&settings_raw),
        mappings_text: pretty(&mappings),
        mappings,
        privs: load_privs(conn, name),
    })
}

/// IDX-11: ratakan `settings` (dan `defaults` bila `defaults`) dari response ES menjadi
/// (key, nilai, default?). Menerima bentuk flat maupun bersarang; array tetap satu nilai.
fn flatten_settings(v: &Value, defaults: bool) -> Vec<(String, String, bool)> {
    fn walk(prefix: &str, v: &Value, is_default: bool, out: &mut Vec<(String, String, bool)>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&key, child, is_default, out);
                }
            }
            Value::Null if prefix.is_empty() => {}
            leaf => out.push((
                prefix.to_owned(),
                leaf.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| leaf.to_string()),
                is_default,
            )),
        }
    }
    let mut out = Vec::new();
    walk("", &v["settings"], false, &mut out);
    if defaults {
        walk("", &v["defaults"], true, &mut out);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Form settings dibangun ulang kecuali reload hanya-default menimpa form yang sudah ada.
fn rebuild_settings_form(defaults_reload: bool, has_form: bool) -> bool {
    !(defaults_reload && has_form)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// IDX-38: validasi jumlah shard untuk shrink/split.
fn resize_shard_error(op: &str, n: u64, pri: u64) -> Option<String> {
    match op {
        "_shrink" if n >= pri || n == 0 || !pri.is_multiple_of(n) => Some(tf(
            "Shrink: jumlah shard harus faktor dari {0} dan lebih kecil.",
            &[&pri],
        )),
        "_split" if n <= pri || !n.is_multiple_of(pri.max(1)) => Some(tf(
            "Split: jumlah shard harus kelipatan {0} dan lebih besar.",
            &[&pri],
        )),
        _ => None,
    }
}

/// Aturan nama index Elasticsearch.
pub fn validate_index_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(t("Nama index wajib diisi.").into());
    }
    if name.len() > 255 {
        return Err(t("Nama index maksimal 255 byte.").into());
    }
    if name != name.to_lowercase() {
        return Err(t("Nama index harus huruf kecil.").into());
    }
    if name.starts_with(['-', '_', '+']) {
        return Err(t("Nama index tidak boleh diawali -, _, atau +.").into());
    }
    if name == "." || name == ".." {
        return Err(t("Nama index tidak boleh . atau ..").into());
    }
    if let Some(c) = name.chars().find(|c| r#"\/*?"<>|, #:"#.contains(*c)) {
        return Err(tf("Karakter '{0}' tidak diizinkan di nama index.", &[&c]));
    }
    Ok(())
}

/// Header kolom jumlah dokumen: singkat di English supaya kolom tetap sempit.
fn docs_header() -> &'static str {
    match i18n::lang() {
        i18n::Lang::Id => "Dokumen",
        i18n::Lang::En => "Docs",
    }
}

/// "Dokumen" sebagai label penuh (tab, ringkasan); kunci global `Dokumen` bermakna Discover.
fn documents_label() -> &'static str {
    match i18n::lang() {
        i18n::Lang::Id => "Dokumen",
        i18n::Lang::En => "Documents",
    }
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("Index {0} dibuat.", "Index {0} created."),
    ("Index {0} dihapus.", "Index {0} deleted."),
    ("Index {0} dibuka.", "Index {0} opened."),
    ("Index {0} ditutup.", "Index {0} closed."),
    ("Index {0} di-refresh.", "Index {0} refreshed."),
    ("Index {0} di-flush.", "Index {0} flushed."),
    (
        "Cache index {0} dibersihkan.",
        "Cache of index {0} cleared.",
    ),
    ("Settings {0} diperbarui.", "Settings of {0} updated."),
    (
        "Field baru ditambahkan ke mapping {0}.",
        "New field added to the mapping of {0}.",
    ),
    ("ILM {0} dicoba ulang.", "ILM for {0} retried."),
    ("Alias {0} diperbarui.", "Aliases of {0} updated."),
    (
        "Force merge {0} berjalan di latar (lihat GET /_tasks?actions=*forcemerge*).",
        "Force merge of {0} is running in the background (see GET /_tasks?actions=*forcemerge*).",
    ),
    ("Rollover {0} selesai.", "Rollover of {0} finished."),
    ("Reindex {0} → {1} dimulai.", "Reindex {0} → {1} started."),
    (
        "Delete by query di {0} dimulai.",
        "Delete by query on {0} started.",
    ),
    (
        "Update by query di {0} dimulai.",
        "Update by query on {0} started.",
    ),
    (
        "Setting statis {0} diperbarui dan index dibuka kembali.",
        "Static settings of {0} updated and the index was reopened.",
    ),
    (
        "{0} {1} → {2} dibuat. {1} masih diblok tulis; buka blok setelah {2} green.",
        "{0} {1} → {2} created. {1} is still write-blocked; remove the block once {2} is green.",
    ),
    ("Cari index…", "Search indices…"),
    ("Tampilkan index sistem (.)", "Show system indices (.)"),
    ("semua", "all"),
    ("⟳ Muat ulang", "⟳ Reload"),
    ("+ Buat index", "+ Create index"),
    ("Menjalankan aksi…", "Running action…"),
    ("Buka di Dev Tools:\n{0}", "Open in Dev Tools:\n{0}"),
    ("{0} index ditampilkan dari {1}", "{0} indices shown of {1}"),
    ("Ukuran", "Size"),
    ("Dibuat", "Created"),
    ("Alias / data stream", "Aliases / data stream"),
    ("Buka di Discover", "Open in Discover"),
    ("Buka di Dev Tools", "Open in Dev Tools"),
    (
        "Profile read-only: aksi disembunyikan.",
        "Read-only profile: actions hidden.",
    ),
    (
        "Index sistem: aksi destruktif dikunci.",
        "System index: destructive actions locked.",
    ),
    (
        "Hapus atau update dokumen yang cocok query",
        "Delete or update documents matching a query",
    ),
    (
        "Salin dokumen ke index lain (async + progress)",
        "Copy documents to another index (async + progress)",
    ),
    (
        "User tidak punya privilege `{0}` pada index ini.",
        "The user lacks the `{0}` privilege on this index.",
    ),
    ("Ubah setting statis…", "Change static setting…"),
    (
        "Butuh close → update → open (index tidak tersedia sesaat)",
        "Needs close → update → open (the index is briefly unavailable)",
    ),
    ("Tampilkan default", "Show defaults"),
    (
        "Mengubah tipe field yang sudah ada butuh reindex ke index baru.",
        "Changing the type of an existing field requires reindexing into a new index.",
    ),
    ("Index kosong.", "Index is empty."),
    ("primer", "primary"),
    ("replika", "replica"),
    ("Alasan: {0}", "Reason: {0}"),
    ("Alasan unassigned: {0}", "Unassigned reason: {0}"),
    ("Response lengkap", "Full response"),
    (
        "Index ini tidak dikelola ILM.",
        "This index is not managed by ILM.",
    ),
    ("Coba ulang ILM", "Retry ILM"),
    ("index sudah dibuka kembali", "the index was reopened"),
    (
        "GAGAL membuka kembali index, buka manual",
        "FAILED to reopen the index, open it manually",
    ),
    ("Setting statis · {0}", "Static setting · {0}"),
    (
        "Index akan ditutup sesaat (tidak bisa dicari/ditulis), diubah, lalu dibuka kembali.",
        "The index will be closed briefly (not searchable/writable), modified, then reopened.",
    ),
    ("Setting", "Setting"),
    ("Nilai (JSON)", "Value (JSON)"),
    (
        "Nilai berupa JSON: string pakai kutip, angka/boolean/objek apa adanya (mis. analysis).",
        "The value is JSON: strings in quotes, numbers/booleans/objects as-is (e.g. analysis).",
    ),
    ("Nilai bukan JSON: {0}", "Value is not JSON: {0}"),
    (
        "Nama setting diawali index. atau analysis",
        "The setting name must start with index. or analysis",
    ),
    (
        "Clone / Shrink / Split · {0}",
        "Clone / Shrink / Split · {0}",
    ),
    ("Index tujuan", "Target index"),
    ("Shard primer", "Primary shards"),
    (
        "Shard primer sumber: {0}. {1} akan diblok tulis (index.blocks.write) sebelum operasi.",
        "Source primary shards: {0}. {1} will be write-blocked (index.blocks.write) before the operation.",
    ),
    (
        "Shrink butuh satu salinan setiap shard di satu node yang sama (otomatis di cluster single-node).",
        "Shrink needs one copy of every shard on the same node (automatic on a single-node cluster).",
    ),
    ("Jalankan", "Run"),
    ("{0} task {1}", "{0} task {1}"),
    ("selesai, {0} gagal", "finished, {0} failed"),
    ("selesai", "finished"),
    ("Batalkan", "Cancel"),
    ("{0} dipilih:", "{0} selected:"),
    ("Batal pilih", "Deselect"),
    ("Sumber", "Source"),
    ("Tujuan", "Target"),
    (
        "opsional, mis. status:active",
        "optional, e.g. status:active",
    ),
    (
        "Tujuan harus berbeda dari sumber.",
        "The target must differ from the source.",
    ),
    (
        "Index tujuan dibuat otomatis jika belum ada (mapping dinamis). Buat dulu dengan mapping yang benar bila perlu.",
        "The target index is created automatically if it does not exist (dynamic mapping). Create it first with the right mapping if needed.",
    ),
    ("Mulai reindex", "Start reindex"),
    ("By query · {0}", "By query · {0}"),
    ("Hapus dokumen", "Delete documents"),
    ("Update dokumen", "Update documents"),
    (
        "Query (KQL; kosong = semua dokumen)",
        "Query (KQL; empty = all documents)",
    ),
    (
        "Script Painless (opsional, mis. ctx._source.status = 'arsip')",
        "Painless script (optional, e.g. ctx._source.status = 'archived')",
    ),
    ("Hitung dokumen", "Count documents"),
    ("{0} dokumen cocok", "{0} documents match"),
    ("Ketik HAPUS untuk konfirmasi:", "Type HAPUS to confirm:"),
    (
        "Berjalan async dengan conflicts=proceed; progress tampil di atas daftar index.",
        "Runs asynchronously with conflicts=proceed; progress is shown above the index list.",
    ),
    ("Hapus index?", "Delete index?"),
    (
        "Index {0} dan semua dokumennya akan dihapus permanen. Aksi ini tidak bisa dibatalkan.",
        "Index {0} and all its documents will be permanently deleted. This cannot be undone.",
    ),
    ("Ubah setting statis?", "Change static setting?"),
    (
        "{0} akan ditutup, diubah, lalu dibuka kembali (sementara tidak tersedia).",
        "{0} will be closed, modified, then reopened (temporarily unavailable).",
    ),
    ("Clone/Shrink/Split?", "Clone/Shrink/Split?"),
    (
        "{0} akan diblok tulis, lalu {1} dibuat dari {0}.",
        "{0} will be write-blocked, then {1} is created from {0}.",
    ),
    (
        "Force merge {0} ke 1 segment memakan I/O dan CPU besar. Jangan jalankan pada index yang masih aktif ditulisi.",
        "Force merging {0} down to 1 segment uses a lot of I/O and CPU. Do not run it on an index that is still being written to.",
    ),
    (
        "{0} akan menulis ke backing index baru mulai sekarang.",
        "{0} will write to a new backing index from now on.",
    ),
    ("Tutup index?", "Close index?"),
    (
        "Index {0} tidak bisa dicari atau ditulisi sampai dibuka kembali.",
        "Index {0} cannot be searched or written to until it is reopened.",
    ),
    ("Lanjutkan?", "Continue?"),
    ("Buat index", "Create index"),
    ("Replika", "Replicas"),
    ("Mappings (JSON, opsional)", "Mappings (JSON, optional)"),
    ("Buat", "Create"),
    (
        "Index ini belum punya alias.",
        "This index has no aliases yet.",
    ),
    ("nama alias", "alias name"),
    ("Tambah alias", "Add alias"),
    ("Request:\n{0}", "Request:\n{0}"),
    ("Mappings tidak valid: {0}", "Invalid mappings: {0}"),
    ("Shard primer / replika", "Primary / replica shards"),
    ("Dokumen terhapus", "Deleted documents"),
    ("Ukuran total", "Total size"),
    ("Ukuran primer", "Primary size"),
    ("Setting dinamis", "Dynamic settings"),
    ("default 1s, -1 = mati", "default 1s, -1 = off"),
    ("default 10000", "default 10000"),
    (
        "number_of_replicas harus angka.",
        "number_of_replicas must be a number.",
    ),
    (
        "max_result_window harus angka.",
        "max_result_window must be a number.",
    ),
    ("Simpan settings", "Save settings"),
    ("Tambah field", "Add field"),
    ("nama.field", "field.name"),
    (
        "Shrink: jumlah shard harus faktor dari {0} dan lebih kecil.",
        "Shrink: the shard count must be a factor of {0} and smaller.",
    ),
    (
        "Split: jumlah shard harus kelipatan {0} dan lebih besar.",
        "Split: the shard count must be a multiple of {0} and larger.",
    ),
    ("Nama index wajib diisi.", "The index name is required."),
    (
        "Nama index maksimal 255 byte.",
        "The index name can be at most 255 bytes.",
    ),
    (
        "Nama index harus huruf kecil.",
        "The index name must be lowercase.",
    ),
    (
        "Nama index tidak boleh diawali -, _, atau +.",
        "The index name must not start with -, _ or +.",
    ),
    (
        "Nama index tidak boleh . atau ..",
        "The index name must not be . or ..",
    ),
    (
        "Karakter '{0}' tidak diizinkan di nama index.",
        "The character '{0}' is not allowed in an index name.",
    ),
    ("Umur", "Age"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("indices.rs"), EN);
        crate::i18n::assert_keys(["Tipe", "Umur", "Hapus"]);
    }

    // NFR: filter + sort 10.000 index < 20 ms (release).
    #[test]
    #[ignore = "perf"]
    fn perf_filter_sort_10k_rows() {
        let rows: Vec<Row> = (0..10_000u64)
            .map(|i| Row {
                name: if i % 50 == 0 {
                    format!(".sys-{i}")
                } else {
                    format!("logs-app-{}-{i:06}", i % 17)
                },
                health: ["green", "yellow", "red"][(i % 3) as usize].into(),
                status: "open".into(),
                pri: i % 5 + 1,
                rep: i % 2,
                docs: i * 37 % 1_000_003,
                deleted: 0,
                size: (i * 7919 % 100_003) as f64,
                created: 1_700_000_000_000 + i as i64,
                aliases: vec![format!("alias-{}", i % 100)],
                data_stream: None,
            })
            .collect();
        let start = std::time::Instant::now();
        let out = filter_sort_rows(
            &rows,
            "logs-app",
            false,
            Some("green"),
            Some((Col::Docs, false)),
        );
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        eprintln!("PERF filter_sort_10k: {ms:.2} ms ({} baris)", out.len());
        assert!(!out.is_empty());
        assert!(ms < 20.0, "terlalu lambat: {ms:.2} ms");
    }

    #[test]
    fn validates_resize_shards() {
        assert!(resize_shard_error("_shrink", 2, 4).is_none());
        assert!(resize_shard_error("_shrink", 3, 4).is_some());
        assert!(resize_shard_error("_shrink", 4, 4).is_some());
        assert!(resize_shard_error("_split", 8, 4).is_none());
        assert!(resize_shard_error("_split", 6, 4).is_some());
        assert!(resize_shard_error("_clone", 1, 4).is_none());
        let a = Action::StaticSettings("x".into(), json!({"index.codec": "best_compression"}));
        let steps: Vec<String> = a
            .steps()
            .into_iter()
            .map(|(m, p, _)| format!("{m} {p}"))
            .collect();
        assert_eq!(
            steps,
            ["POST /x/_close", "PUT /x/_settings", "POST /x/_open"]
        );
    }

    #[test]
    fn validates_index_names() {
        assert!(validate_index_name("logs-2026.11").is_ok());
        assert!(validate_index_name("").is_err());
        assert!(validate_index_name("Logs").is_err());
        assert!(validate_index_name("_x").is_err());
        assert!(validate_index_name("a b").is_err());
        assert!(validate_index_name("a*").is_err());
        assert!(validate_index_name("a:b").is_err());
        assert!(validate_index_name(&"a".repeat(256)).is_err());
    }

    #[test]
    fn form_dipertahankan_saat_reload_default() {
        assert!(!rebuild_settings_form(true, true));
        assert!(rebuild_settings_form(true, false));
        assert!(rebuild_settings_form(false, true));
        assert!(rebuild_settings_form(false, false));
    }

    #[test]
    fn flatten_settings_memisahkan_default() {
        let nested = json!({
            "settings": {"index": {"number_of_shards": "1", "blocks": {"write": "true"}}},
            "defaults": {"index": {"refresh_interval": "1s", "routing": {"x": ["a", "b"]}}}
        });
        let all = flatten_settings(&nested, true);
        assert!(all.contains(&("index.number_of_shards".into(), "1".into(), false)));
        assert!(all.contains(&("index.blocks.write".into(), "true".into(), false)));
        assert!(all.contains(&("index.refresh_interval".into(), "1s".into(), true)));
        assert!(all.contains(&("index.routing.x".into(), "[\"a\",\"b\"]".into(), true)));
        let explicit = flatten_settings(&nested, false);
        assert_eq!(explicit.len(), 2);
        assert!(explicit.iter().all(|(_, _, d)| !d));
        let flat = json!({"settings": {"index.number_of_replicas": "0"}});
        assert_eq!(
            flatten_settings(&flat, true),
            vec![("index.number_of_replicas".into(), "0".into(), false)]
        );
        assert!(flatten_settings(&Value::Null, true).is_empty());
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_list_and_detail() {
        let conn = crate::es::client::live_conn().expect("env");
        let rows = load_rows(&conn).unwrap();
        assert!(!rows.is_empty());
        let row = rows
            .iter()
            .find(|r| !r.system() && r.status == "open")
            .unwrap();
        let detail = load_detail(&conn, &row.name, false).unwrap();
        println!(
            "{} index; {}: {} settings, docs {}",
            rows.len(),
            row.name,
            detail.settings.len(),
            row.docs
        );
        assert!(
            detail
                .settings
                .iter()
                .any(|(k, _)| k == "index.number_of_shards")
        );
        assert!(detail.mappings.is_object());
        assert_eq!(detail.mappings_text, pretty(&detail.mappings));
        assert!(detail.settings_text.contains("\"settings\""));
        let with_defaults = load_detail(&conn, &row.name, true).unwrap();
        assert!(with_defaults.settings_all.iter().any(|(_, _, d)| *d));
        assert!(detail.stats["total"].is_object());
        println!("privilege: {:?}", detail.privs);
        assert!(detail.privs.manage, "user elastic punya manage");
        assert!(
            conn.send("POST", &format!("/{}/_close", row.name), None)
                .is_err(),
            "guard read-only"
        );
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_allocation_explain() {
        let conn = crate::es::client::live_conn().expect("env");
        let shards = conn
            .get("/_cat/shards?format=json&h=index,shard,prirep,state")
            .unwrap();
        let Some(r) = shards
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["state"] == "UNASSIGNED")
        else {
            println!("tidak ada shard unassigned, dilewati");
            return;
        };
        let body =
            json!({"index": r["index"], "shard": num(&r["shard"]), "primary": r["prirep"] == "p"});
        let v = conn
            .json("GET", "/_cluster/allocation/explain", Some(&body))
            .unwrap();
        println!("explain: {}", v["allocate_explanation"]);
        assert!(v["allocate_explanation"].is_string() || v["can_allocate"].is_string());
    }
}
