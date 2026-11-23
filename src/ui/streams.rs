//! Data stream (IDX-50) dan index/component template (IDX-51, IDX-52).

use eframe::egui::{self, RichText};
use serde_json::{Value, json};

use super::Ctx;
use crate::i18n::{t, tf};
use crate::task::{Pending, poll};
use crate::widgets::{self, Confirm};

type Job = Pending<Result<String, String>>;

/// Request yang menunggu konfirmasi: (judul, pesan, ketik-untuk-konfirmasi, method, path, body).
struct PendingConfirm {
    title: String,
    message: String,
    expected: Option<String>,
    method: &'static str,
    path: String,
    body: Option<Value>,
}

fn run(
    ctx: &egui::Context,
    cx: &Ctx,
    method: &'static str,
    path: String,
    body: Option<Value>,
    done: String,
) -> Job {
    let conn = cx.conn.clone();
    Pending::spawn(ctx, move || {
        conn.json(method, &path, body.as_ref()).map(|_| done)
    })
}

fn describe(method: &str, path: &str, body: Option<&Value>) -> String {
    match body {
        Some(b) => format!(
            "{method} {path}\n{}",
            serde_json::to_string_pretty(b).unwrap_or_default()
        ),
        None => format!("{method} {path}"),
    }
}

fn confirm_ui(
    ctx: &egui::Context,
    cx: &Ctx,
    confirm: &mut Option<PendingConfirm>,
    typed: &mut String,
    job: &mut Option<Job>,
) {
    let Some(c) = confirm else { return };
    let message = format!(
        "{}\n\nRequest:\n{}",
        c.message,
        describe(c.method, &c.path, c.body.as_ref())
    );
    match widgets::confirm_dialog(ctx, &c.title, &message, c.expected.as_deref(), typed, true) {
        Confirm::Yes => {
            let c = confirm.take().expect("ada konfirmasi");
            *job = Some(run(
                ctx,
                cx,
                c.method,
                c.path,
                c.body,
                tf("{0} selesai.", &[&c.title.trim_end_matches('?')]),
            ));
        }
        Confirm::No => *confirm = None,
        Confirm::Pending => {}
    }
}

fn result_line(ui: &mut egui::Ui, message: &Option<Result<String, String>>) {
    match message {
        Some(Ok(text)) => {
            ui.colored_label(egui::Color32::from_rgb(0x2e, 0xa0, 0x43), text);
        }
        Some(Err(err)) => widgets::error_box(ui, err),
        None => {}
    }
}

#[derive(Default)]
pub struct DataStreams {
    list: Option<Result<Vec<Value>, String>>,
    pending: Option<Pending<Result<Vec<Value>, String>>>,
    selected: Option<String>,
    filter: String,
    job: Option<Job>,
    message: Option<Result<String, String>>,
    confirm: Option<PendingConfirm>,
    typed: String,
}

impl DataStreams {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let ctx = ui.ctx().clone();
        if let Some(r) = poll(&mut self.pending) {
            self.list = Some(r);
        }
        if let Some(r) = poll(&mut self.job) {
            if r.is_ok() {
                self.list = None;
                cx.catalog.invalidate();
            }
            self.message = Some(r);
        }
        if self.list.is_none() && self.pending.is_none() {
            let conn = cx.conn.clone();
            self.pending = Some(Pending::spawn(&ctx, move || {
                conn.get("/_data_stream?expand_wildcards=all")
                    .map(|v| v["data_streams"].as_array().cloned().unwrap_or_default())
            }));
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(t("Cari data stream…"))
                    .desired_width(240.0),
            );
            if self.pending.is_some() || self.job.is_some() {
                ui.spinner();
            } else if ui.button(t("⟳ Muat ulang")).clicked() {
                self.list = None;
            }
        });
        result_line(ui, &self.message);
        let streams = match &self.list {
            Some(Ok(list)) => list.clone(),
            Some(Err(err)) => {
                widgets::error_box(ui, err);
                return;
            }
            None => return,
        };
        let selected = self
            .selected
            .as_ref()
            .and_then(|n| streams.iter().find(|s| s["name"] == n.as_str()))
            .cloned();
        if let Some(ds) = selected {
            egui::Panel::right("ds_detail").resizable(true).default_size(420.0).show(ui, |ui| {
                let name = ds["name"].as_str().unwrap_or_default().to_owned();
                ui.horizontal(|ui| {
                    ui.heading(&name);
                    if ui.small_button("✕").clicked() {
                        self.selected = None;
                    }
                });
                if !cx.conn.read_only {
                    ui.horizontal(|ui| {
                        let path = format!("/{name}/_rollover");
                        if ui.button("Rollover").on_hover_text(describe("POST", &path, None)).clicked() {
                            self.confirm = Some(PendingConfirm {
                                title: t("Rollover?").into(),
                                message: tf("{0} akan menulis ke backing index baru.", &[&name]),
                                expected: None,
                                method: "POST",
                                path,
                                body: None,
                            });
                        }
                        let path = format!("/_data_stream/{name}");
                        if ui.button(t("Hapus")).on_hover_text(describe("DELETE", &path, None)).clicked() {
                            self.typed.clear();
                            self.confirm = Some(PendingConfirm {
                                title: t("Hapus data stream?").into(),
                                message: tf("{0} beserta SEMUA backing index dan dokumennya akan dihapus permanen.", &[&name]),
                                expected: Some(name.clone()),
                                method: "DELETE",
                                path,
                                body: None,
                            });
                        }
                    });
                }
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    widgets::json_tree(ui, &ds, egui::Id::unique(("ds", name.as_str())));
                });
            });
        }
        let needle = self.filter.to_lowercase();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("ds_grid")
                    .striped(true)
                    .num_columns(6)
                    .show(ui, |ui| {
                        for h in [
                            "Nama",
                            "Health",
                            "Template",
                            "Lifecycle",
                            "Generasi",
                            "Backing index",
                        ] {
                            ui.strong(t(h));
                        }
                        ui.end_row();
                        for ds in streams.iter().filter(|d| {
                            needle.is_empty()
                                || d["name"].as_str().unwrap_or_default().contains(&needle)
                        }) {
                            let name = ds["name"].as_str().unwrap_or_default();
                            if ui
                                .selectable_label(self.selected.as_deref() == Some(name), name)
                                .clicked()
                            {
                                self.selected = Some(name.to_owned());
                            }
                            ui.label(ds["status"].as_str().unwrap_or("-").to_lowercase());
                            ui.label(ds["template"].as_str().unwrap_or("-"));
                            let lifecycle = ds["ilm_policy"]
                                .as_str()
                                .map(|p| format!("ILM: {p}"))
                                .or_else(|| {
                                    ds["lifecycle"]["data_retention"]
                                        .as_str()
                                        .map(|r| tf("retensi {0}", &[&r]))
                                })
                                .unwrap_or_else(|| "-".into());
                            ui.label(lifecycle);
                            ui.label(ds["generation"].to_string());
                            ui.label(ds["indices"].as_array().map_or(0, Vec::len).to_string());
                            ui.end_row();
                        }
                    });
                if streams.is_empty() {
                    ui.weak(t("Tidak ada data stream."));
                }
            });
        });
        confirm_ui(&ctx, cx, &mut self.confirm, &mut self.typed, &mut self.job);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Index,
    Component,
    /// IDX-53: legacy `_template` (ES 7.x, deprecated).
    Legacy,
}

impl Kind {
    fn path(self, name: &str) -> String {
        match self {
            Kind::Index => format!("/_index_template/{name}"),
            Kind::Component => format!("/_component_template/{name}"),
            Kind::Legacy => format!("/_template/{name}"),
        }
    }
}

/// (jenis, nama, isi template)
type Template = (Kind, String, Value);

#[derive(Default)]
pub struct Templates {
    list: Option<Result<Vec<Template>, String>>,
    pending: Option<Pending<Result<Vec<Template>, String>>>,
    filter: String,
    show_system: bool,
    /// (jenis, nama, teks JSON editor, baru?)
    editing: Option<(Kind, String, String, bool)>,
    simulate: Option<Result<Value, String>>,
    simulate_pending: Option<Pending<Result<Value, String>>>,
    job: Option<Job>,
    message: Option<Result<String, String>>,
    confirm: Option<PendingConfirm>,
    typed: String,
    /// PRF-12: privilege cluster `manage_index_templates`.
    can_manage: Option<bool>,
    can_manage_pending: Option<Pending<bool>>,
}

impl Templates {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        let ctx = ui.ctx().clone();
        if let Some(r) = poll(&mut self.pending) {
            self.list = Some(r);
        }
        if let Some(r) = poll(&mut self.simulate_pending) {
            self.simulate = Some(r);
        }
        if let Some(r) = poll(&mut self.job) {
            if r.is_ok() {
                self.list = None;
                if self.editing.as_ref().is_some_and(|e| e.3) {
                    self.editing = None;
                }
            }
            self.message = Some(r);
        }
        if self.list.is_none() && self.pending.is_none() {
            let conn = cx.conn.clone();
            self.pending = Some(Pending::spawn(&ctx, move || load_templates(&conn)));
        }
        if let Some(v) = poll(&mut self.can_manage_pending) {
            self.can_manage = Some(v);
        }
        if self.can_manage.is_none() && self.can_manage_pending.is_none() {
            let conn = cx.conn.clone();
            self.can_manage_pending = Some(Pending::spawn(&ctx, move || {
                let body = json!({"cluster": ["manage_index_templates"]});
                conn.json("POST", "/_security/user/_has_privileges", Some(&body))
                    .map(|v| {
                        v["cluster"]["manage_index_templates"]
                            .as_bool()
                            .unwrap_or(true)
                    })
                    .unwrap_or(true)
            }));
        }
        let writable = !cx.conn.read_only && self.can_manage != Some(false);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(t("Cari template…")).desired_width(240.0));
            ui.checkbox(&mut self.show_system, t("Tampilkan template bawaan"));
            if self.pending.is_some() || self.job.is_some() {
                ui.spinner();
            } else if ui.button(t("⟳ Muat ulang")).clicked() {
                self.list = None;
            }
            if self.can_manage == Some(false) {
                ui.weak(t("Tanpa privilege manage_index_templates: hanya baca."));
            }
            if writable {
                if ui.button("+ Index template").clicked() {
                    let body = json!({"index_patterns": ["nama-*"], "priority": 200, "template": {"settings": {"number_of_shards": 1}, "mappings": {"properties": {}}}});
                    self.editing = Some((Kind::Index, String::new(), pretty(&body), true));
                    self.simulate = None;
                }
                if ui.button("+ Component template").clicked() {
                    let body = json!({"template": {"mappings": {"properties": {}}}});
                    self.editing = Some((Kind::Component, String::new(), pretty(&body), true));
                    self.simulate = None;
                }
            }
        });
        result_line(ui, &self.message);
        let list = match &self.list {
            Some(Ok(list)) => list.clone(),
            Some(Err(err)) => {
                widgets::error_box(ui, err);
                return;
            }
            None => return,
        };
        if self.editing.is_some() {
            egui::Panel::right("template_editor")
                .resizable(true)
                .default_size(ui.available_width() * 0.5)
                .show(ui, |ui| self.editor_ui(ui, &ctx, cx, writable));
        }
        let needle = self.filter.to_lowercase();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("tpl_grid")
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        for h in ["Nama", "Jenis", "Pola index", "Prioritas", "Komponen"] {
                            ui.strong(t(h));
                        }
                        ui.end_row();
                        for (kind, name, body) in &list {
                            let system =
                                body["_meta"]["managed"] == json!(true) || name.starts_with('.');
                            if (system && !self.show_system)
                                || (!needle.is_empty() && !name.to_lowercase().contains(&needle))
                            {
                                continue;
                            }
                            let active = self
                                .editing
                                .as_ref()
                                .is_some_and(|e| e.0 == *kind && e.1 == *name);
                            if ui.selectable_label(active, name).clicked() {
                                self.editing = Some((*kind, name.clone(), pretty(body), false));
                                self.simulate = None;
                            }
                            ui.label(match kind {
                                Kind::Index => "index",
                                Kind::Component => "component",
                                Kind::Legacy => "legacy",
                            });
                            let patterns: Vec<&str> = body["index_patterns"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect();
                            ui.label(patterns.join(", "));
                            ui.label(
                                body["priority"]
                                    .as_u64()
                                    .map_or("-".into(), |p| p.to_string()),
                            );
                            let composed: Vec<&str> = body["composed_of"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect();
                            ui.label(composed.join(", "));
                            ui.end_row();
                        }
                    });
            });
        });
        confirm_ui(&ctx, cx, &mut self.confirm, &mut self.typed, &mut self.job);
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, cx: &mut Ctx, writable: bool) {
        let Some((_, name, _, is_new)) = &mut self.editing else {
            return;
        };
        let mut close = false;
        ui.horizontal(|ui| {
            if *is_new {
                ui.label(t("Nama"));
                ui.add(egui::TextEdit::singleline(name).desired_width(200.0));
            } else {
                ui.heading(name.as_str());
            }
            close = ui.small_button("✕").clicked();
        });
        if close {
            self.editing = None;
            return;
        }
        let Some((kind, name, text, is_new)) = &mut self.editing else {
            return;
        };
        let parsed: Result<Value, String> =
            serde_json::from_str(text).map_err(|e| tf("JSON tidak valid: {0}", &[&e]));
        let path = kind.path(name.trim());
        ui.horizontal(|ui| {
            if *kind == Kind::Index && !name.trim().is_empty() {
                // `_simulate` dengan body = hasil merge tanpa menyimpan.
                let sim_path = format!("/_index_template/_simulate/{}", name.trim());
                if ui
                    .button(t("Simulasi"))
                    .on_hover_text(t("Hasil gabungan template + komponen (tanpa menyimpan)"))
                    .clicked()
                {
                    let conn = cx.conn.clone();
                    let body = parsed.clone().ok();
                    self.simulate_pending = Some(Pending::spawn(ctx, move || {
                        conn.json("POST", &sim_path, body.as_ref())
                    }));
                }
            }
            if writable {
                let ok = parsed.is_ok() && !name.trim().is_empty();
                if ui
                    .add_enabled(ok, egui::Button::new(t("Simpan")))
                    .on_hover_text(describe("PUT", &path, parsed.as_ref().ok()))
                    .clicked()
                    && let Ok(body) = &parsed
                {
                    self.job = Some(run(
                        ctx,
                        cx,
                        "PUT",
                        path.clone(),
                        Some(body.clone()),
                        tf("Template {0} disimpan.", &[&name.trim()]),
                    ));
                }
                if !*is_new
                    && ui
                        .button(t("Hapus"))
                        .on_hover_text(describe("DELETE", &path, None))
                        .clicked()
                {
                    self.typed.clear();
                    self.confirm = Some(PendingConfirm {
                        title: t("Hapus template?").into(),
                        message: tf(
                            "Template {0} akan dihapus. Index yang sudah ada tidak terpengaruh.",
                            &[&name],
                        ),
                        expected: None,
                        method: "DELETE",
                        path: path.clone(),
                        body: None,
                    });
                }
            }
        });
        if let Err(err) = &parsed {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        egui::ScrollArea::vertical()
            .id_salt("tpl_edit")
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(20),
                );
                if self.simulate_pending.is_some() {
                    ui.spinner();
                }
                match &self.simulate {
                    Some(Ok(v)) => {
                        ui.separator();
                        ui.label(RichText::new(t("Hasil simulasi")).strong());
                        if let Some(overlaps) =
                            v["overlapping"].as_array().filter(|o| !o.is_empty())
                        {
                            let names: Vec<&str> =
                                overlaps.iter().filter_map(|o| o["name"].as_str()).collect();
                            widgets::warn_box(
                                ui,
                                &tf("Pola tumpang tindih dengan: {0}", &[&names.join(", ")]),
                            );
                        }
                        widgets::json_tree(ui, &v["template"], egui::Id::unique("tpl_sim"));
                    }
                    Some(Err(err)) => widgets::error_box(ui, err),
                    None => {}
                }
            });
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Buang properti yang dikelola sistem: ada di `GET` tetapi ditolak saat `PUT`/simulasi.
fn strip_managed(mut body: Value) -> Value {
    if let Some(o) = body.as_object_mut() {
        o.retain(|k, _| !k.starts_with("created_date") && !k.starts_with("modified_date"));
    }
    body
}

fn load_templates(conn: &crate::es::Conn) -> Result<Vec<Template>, String> {
    let mut out = Vec::new();
    let index = conn.get("/_index_template")?;
    for t in index["index_templates"].as_array().into_iter().flatten() {
        out.push((
            Kind::Index,
            t["name"].as_str().unwrap_or_default().to_owned(),
            strip_managed(t["index_template"].clone()),
        ));
    }
    if let Ok(component) = conn.get("/_component_template") {
        for t in component["component_templates"]
            .as_array()
            .into_iter()
            .flatten()
        {
            out.push((
                Kind::Component,
                t["name"].as_str().unwrap_or_default().to_owned(),
                strip_managed(t["component_template"].clone()),
            ));
        }
    }
    if let Ok(legacy) = conn.get("/_template") {
        for (name, body) in legacy.as_object().into_iter().flatten() {
            out.push((Kind::Legacy, name.clone(), body.clone()));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("Generasi", "Generation"),
    ("Jenis", "Kind"),
    ("Pola index", "Index pattern"),
    ("Prioritas", "Priority"),
    ("Komponen", "Components"),
    ("{0} selesai.", "{0} finished."),
    ("Cari data stream…", "Search data streams…"),
    (
        "{0} akan menulis ke backing index baru.",
        "{0} will write to a new backing index.",
    ),
    ("Hapus data stream?", "Delete data stream?"),
    (
        "{0} beserta SEMUA backing index dan dokumennya akan dihapus permanen.",
        "{0} and ALL its backing indices and documents will be permanently deleted.",
    ),
    ("retensi {0}", "retention {0}"),
    ("Tidak ada data stream.", "No data streams."),
    ("Cari template…", "Search templates…"),
    ("Tampilkan template bawaan", "Show built-in templates"),
    (
        "Tanpa privilege manage_index_templates: hanya baca.",
        "Without the manage_index_templates privilege: read-only.",
    ),
    ("JSON tidak valid: {0}", "Invalid JSON: {0}"),
    ("Simulasi", "Simulate"),
    (
        "Hasil gabungan template + komponen (tanpa menyimpan)",
        "Merged result of template + components (without saving)",
    ),
    ("Template {0} disimpan.", "Template {0} saved."),
    ("Hapus template?", "Delete template?"),
    (
        "Template {0} akan dihapus. Index yang sudah ada tidak terpengaruh.",
        "Template {0} will be deleted. Existing indices are not affected.",
    ),
    ("Hasil simulasi", "Simulation result"),
    (
        "Pola tumpang tindih dengan: {0}",
        "Pattern overlaps with: {0}",
    ),
    ("Rollover?", "Rollover?"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("streams.rs"), EN);
        crate::i18n::assert_keys([
            "Nama",
            "Generasi",
            "Jenis",
            "Pola index",
            "Prioritas",
            "Komponen",
        ]);
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_templates_and_streams() {
        let conn = crate::es::client::live_conn().expect("env");
        let list = load_templates(&conn).unwrap();
        println!("{} template", list.len());
        assert!(!list.is_empty());
        let (_, name, body) = list.iter().find(|(k, ..)| *k == Kind::Index).unwrap();
        let sim = conn
            .json(
                "POST",
                &format!("/_index_template/_simulate/{name}"),
                Some(body),
            )
            .unwrap();
        assert!(sim["template"].is_object());
        let ds = conn.get("/_data_stream?expand_wildcards=all").unwrap();
        println!(
            "{} data stream",
            ds["data_streams"].as_array().map_or(0, Vec::len)
        );
    }
}
