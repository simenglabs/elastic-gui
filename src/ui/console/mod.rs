//! Dev Tools Console: editor multi-request, autocomplete, eksekusi, history.

pub mod complete;
pub mod fold;
pub mod parse;

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, text::CCursor, text::CCursorRange};
use egui::widgets::text_edit::TextEditState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Ctx;
use crate::es::client::{Response, Source, is_ndjson};
use crate::es::{Conn, spec};
use crate::i18n::{self, t, tf};
use crate::profile::AuthKind;
use crate::task::{Pending, poll};
use crate::widgets::{self, Confirm, Highlighter, Lang};
use complete::{Live, Located, Suggestion};
use parse::Block;

const TABS_FILE: &str = "console_tabs.json";
const HISTORY_FILE: &str = "console_history.json";
const VARS_FILE: &str = "console_vars.json";
const HISTORY_MAX: usize = 500;
const VIEW_LIMIT: usize = 2 * 1024 * 1024;
/// Lebar gutter editor: tombol ▶ + bar blok aktif, lalu penanda lipatan (CON-09).
const GUTTER: f32 = 38.0;

const WELCOME: &str = "# Ctrl/Cmd+Enter: jalankan request di cursor. Ctrl+Space: saran.\n\
# Ctrl/Cmd+I: rapikan JSON. Seleksi beberapa request untuk menjalankan semuanya.\n\
GET /_cluster/health\n\n\
GET /_cat/indices?v&s=store.size:desc\n\n\
GET /_search\n{\n  \"query\": {\n    \"match_all\": {}\n  }\n}\n";

#[derive(Clone, Serialize, Deserialize)]
struct Tab {
    name: String,
    text: String,
    /// CON-64: profile target tab ini (`None` = profile aktif).
    #[serde(default)]
    target: Option<String>,
    /// CON-09: offset byte awal baris pembuka region yang terlipat.
    #[serde(default)]
    folds: std::collections::BTreeSet<usize>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryItem {
    ts: i64,
    method: String,
    path: String,
    body: String,
    status: Option<u16>,
    millis: u128,
}

/// Request siap kirim.
#[derive(Clone)]
struct Prepared {
    method: String,
    path: String,
    raw_body: String,
    body: Option<String>,
}

/// (index, field, awalan)
type TermsKey = (String, String, String);

type Executed = (Prepared, Result<Response, String>);

struct RunResult {
    method: String,
    path: String,
    result: Result<Response, String>,
    pretty: String,
    tree: Option<Value>,
}

struct Popup {
    located: Located,
    items: Vec<Suggestion>,
    selected: usize,
    loading_fields: bool,
}

pub struct Console {
    loaded: bool,
    tabs: Vec<Tab>,
    active: usize,
    renaming: Option<usize>,
    dirty_since: Option<Instant>,
    history: Vec<HistoryItem>,
    history_open: bool,
    history_search: String,
    results: Vec<RunResult>,
    pending: Option<Pending<Vec<Executed>>>,
    started: Option<Instant>,
    tree_view: bool,
    highlighter: Highlighter,
    response_highlighter: Highlighter,
    popup: Option<Popup>,
    force_popup: bool,
    confirm: Option<Vec<Prepared>>,
    confirm_typed: String,
    notice: Option<String>,
    /// Teks yang dikirim dari modul lain (inspector).
    incoming: Option<String>,
    curl_input: String,
    /// CON-06: peringatan untuk request di posisi cursor.
    lint: Option<String>,
    vars: Vec<(String, String)>,
    vars_open: bool,
    /// Koneksi + X-Opaque-Id request yang sedang berjalan (CON-42).
    running: Option<(Source, String)>,
    /// CON-29: cache `_terms_enum` per (index, field, awalan).
    terms_cache: std::collections::HashMap<TermsKey, Vec<String>>,
    terms_pending: Option<Pending<(TermsKey, Vec<String>)>>,
    cancel_pending: Option<Pending<Result<usize, String>>>,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            loaded: false,
            tabs: vec![Tab {
                name: "Console".into(),
                text: t(WELCOME).into(),
                target: None,
                folds: Default::default(),
            }],
            active: 0,
            renaming: None,
            dirty_since: None,
            history: Vec::new(),
            history_open: false,
            history_search: String::new(),
            results: Vec::new(),
            pending: None,
            started: None,
            tree_view: false,
            highlighter: Highlighter::default(),
            response_highlighter: Highlighter::default(),
            popup: None,
            force_popup: false,
            confirm: None,
            confirm_typed: String::new(),
            notice: None,
            incoming: None,
            curl_input: String::new(),
            lint: None,
            vars: Vec::new(),
            vars_open: false,
            running: None,
            terms_cache: Default::default(),
            terms_pending: None,
            cancel_pending: None,
        }
    }
}

impl Console {
    /// Tambahkan request ke akhir tab aktif (dipakai "Buka di Console").
    pub fn append_request(&mut self, method: &str, path: &str, body: Option<&Value>) {
        let mut text = format!("{method} {path}\n");
        if let Some(body) = body {
            text.push_str(&serde_json::to_string_pretty(body).unwrap_or_default());
            text.push('\n');
        }
        self.incoming = Some(text);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if !self.loaded {
            self.loaded = true;
            if let Some(tabs) = cx.store.load::<Vec<Tab>>(&cx.profile.id, TABS_FILE)
                && !tabs.is_empty()
            {
                self.tabs = tabs;
            }
            self.history = cx
                .store
                .load(&cx.profile.id, HISTORY_FILE)
                .unwrap_or_default();
            self.vars = cx.store.load(&cx.profile.id, VARS_FILE).unwrap_or_default();
        }
        if let Some(text) = self.incoming.take() {
            let tab = &mut self.tabs[self.active];
            if !tab.text.ends_with("\n\n") {
                tab.text.push_str(if tab.text.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                });
            }
            tab.text.push_str(&text);
            self.dirty_since = Some(Instant::now());
        }
        self.poll_results(cx);
        if self
            .dirty_since
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
        {
            cx.store.save(&cx.profile.id, TABS_FILE, &self.tabs);
            self.dirty_since = None;
        } else if self.dirty_since.is_some() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }

        self.toolbar(ui, cx);
        if self.vars_open {
            egui::Panel::right("console_vars")
                .resizable(true)
                .default_size(280.0)
                .show(ui, |ui| self.vars_ui(ui, cx));
        }
        if self.history_open {
            egui::Panel::right("console_history")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| self.history_ui(ui));
        }
        let width = ui.available_width();
        egui::Panel::left("console_editor")
            .resizable(true)
            .default_size(width * 0.5)
            .show(ui, |ui| self.editor_ui(ui, cx));
        egui::CentralPanel::default().show(ui, |ui| self.response_ui(ui, cx));
        self.confirm_ui(ui.ctx(), cx);
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        ui.horizontal(|ui| {
            let mut close = None;
            for i in 0..self.tabs.len() {
                if self.renaming == Some(i) {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.tabs[i].name).desired_width(120.0),
                    );
                    r.request_focus();
                    if r.lost_focus() {
                        self.renaming = None;
                        self.dirty_since = Some(Instant::now());
                    }
                    continue;
                }
                let r = ui
                    .selectable_label(self.active == i, &self.tabs[i].name)
                    .on_hover_text(t("Klik dua kali untuk ganti nama"));
                if r.clicked() {
                    self.active = i;
                    self.popup = None;
                }
                if r.double_clicked() {
                    self.renaming = Some(i);
                }
                if self.tabs.len() > 1 && ui.small_button("×").clicked() {
                    close = Some(i);
                }
            }
            if ui.small_button("+").on_hover_text(t("Tab baru")).clicked() {
                self.tabs.push(Tab {
                    name: format!("Console {}", self.tabs.len() + 1),
                    text: String::new(),
                    target: None,
                    folds: Default::default(),
                });
                self.active = self.tabs.len() - 1;
                self.dirty_since = Some(Instant::now());
            }
            if let Some(i) = close {
                self.tabs.remove(i);
                self.active = self.active.min(self.tabs.len() - 1);
                self.dirty_since = Some(Instant::now());
            }
            ui.separator();
            let running = self.pending.is_some();
            if running {
                ui.spinner();
                if ui.button(t("■ Batal")).clicked() {
                    self.pending = None;
                    self.started = None;
                    self.notice = Some(t("Dibatalkan. Membatalkan task di server…").into());
                    if let Some((source, opaque)) = self.running.take() {
                        self.cancel_pending = Some(Pending::spawn(ui.ctx(), move || {
                            crate::es::client::cancel_opaque(&*source.open()?, &opaque)
                        }));
                    }
                }
            } else if ui
                .button(t("▶ Jalankan"))
                .on_hover_text("Ctrl/Cmd+Enter")
                .clicked()
            {
                self.run_at_cursor(ui.ctx(), cx);
            }
            if ui
                .button(t("Rapikan"))
                .on_hover_text("Ctrl/Cmd+I")
                .clicked()
            {
                self.format_at_cursor(ui.ctx());
            }
            if ui
                .button(t("Salin cURL"))
                .on_hover_text(t("Kredensial disamarkan"))
                .clicked()
                && let Ok((source, _)) = self.target(cx)
            {
                // cURL hanya butuh URL dan jenis auth (secret disamarkan): keychain tidak dibaca.
                let conn = match source {
                    Source::Ready(conn) => Ok(conn),
                    Source::Profile(profile) => Conn::new(&profile, None).map(Arc::new),
                };
                match conn {
                    Ok(conn) => self.copy_curl(ui.ctx(), &conn),
                    Err(err) => self.notice = Some(err),
                }
            }
            ui.menu_button("Snippet", |ui| {
                for (name, text) in parse::SNIPPETS {
                    if ui.button(t(name)).clicked() {
                        self.incoming = Some((*text).to_owned());
                        ui.close();
                    }
                }
            });
            ui.menu_button(t("Impor cURL"), |ui| {
                ui.set_min_width(420.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.curl_input)
                        .code_editor()
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("curl -X GET 'https://host:9200/_search' -d '{...}'"),
                );
                if ui.button(t("Impor")).clicked() {
                    match parse::from_curl(&self.curl_input) {
                        Ok(text) => {
                            self.incoming = Some(text);
                            self.curl_input.clear();
                            self.notice = None;
                            ui.close();
                        }
                        Err(err) => self.notice = Some(err),
                    }
                }
            });
            ui.toggle_value(&mut self.history_open, "History");
            ui.toggle_value(
                &mut self.vars_open,
                tf("Variabel ({0})", &[&self.vars.len()]),
            );
            if let Some(block) = self.selected_blocks(ui.ctx()).first()
                && let Some(endpoint) =
                    spec::for_major(cx.caps.major).find(&block.method, &block.path)
                && !endpoint.doc.is_empty()
            {
                ui.hyperlink_to(tf("Dok: {0}", &[&endpoint.name]), &endpoint.doc);
            }
            // CON-64: cluster target tab ini.
            let current = self.tabs[self.active].target.clone();
            let label = current
                .as_ref()
                .and_then(|id| cx.profiles.iter().find(|p| &p.id == id))
                .map_or_else(
                    || tf("{0} (aktif)", &[&cx.profile.name]),
                    |p| p.name.clone(),
                );
            let mut chosen = current.clone();
            egui::ComboBox::from_id_salt(("console_target", self.active))
                .selected_text(format!("→ {label}"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen, None, tf("{0} (aktif)", &[&cx.profile.name]));
                    for p in cx.profiles.iter().filter(|p| p.id != cx.profile.id) {
                        let text = format!("{} · {}", p.name, p.env.label());
                        ui.selectable_value(&mut chosen, Some(p.id.clone()), text);
                    }
                })
                .response
                .on_hover_text(t("Cluster tujuan request di tab ini"));
            if chosen != current {
                self.tabs[self.active].target = chosen;
                self.dirty_since = Some(Instant::now());
            }
            if cx.conn.read_only {
                ui.label(
                    RichText::new("READ-ONLY")
                        .color(ui.visuals().warn_fg_color)
                        .strong(),
                );
            }
        });
        ui.separator();
    }

    fn editor_id(&self) -> egui::Id {
        egui::Id::unique(("console_editor", self.active))
    }

    fn cursor_byte(&self, ctx: &egui::Context) -> (usize, Option<(usize, usize)>) {
        let text = &self.tabs[self.active].text;
        let range = TextEditState::load(ctx, self.editor_id()).and_then(|s| s.cursor.char_range());
        let Some(range) = range else {
            return (text.len(), None);
        };
        let primary = char_to_byte(text, range.primary.index.0);
        let secondary = char_to_byte(text, range.secondary.index.0);
        let selection =
            (primary != secondary).then(|| (primary.min(secondary), primary.max(secondary)));
        (primary, selection)
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if let Some(lint) = &self.lint {
            ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {lint}"));
        }
        let id = self.editor_id();
        let focused = ui.memory(|m| m.has_focus(id));
        let ctx = ui.ctx().clone();
        let command = egui::Modifiers::COMMAND;

        // Tombol khusus diambil sebelum TextEdit supaya tidak menyisipkan karakter.
        let mut accept = false;
        if focused {
            if ui.input_mut(|i| i.consume_key(command, egui::Key::Enter)) {
                self.run_at_cursor(&ctx, cx);
            }
            if ui.input_mut(|i| i.consume_key(command, egui::Key::I)) {
                self.format_at_cursor(&ctx);
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space)) {
                self.force_popup = true;
            }
            if let Some(popup) = &mut self.popup {
                let n = popup.items.len().max(1);
                ui.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        popup.selected = (popup.selected + 1) % n;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        popup.selected = (popup.selected + n - 1) % n;
                    }
                    if !popup.items.is_empty()
                        && (i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                            || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
                    {
                        accept = true;
                    }
                });
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    self.popup = None;
                }
            }
        }
        if accept {
            self.accept_suggestion(&ctx, None);
        }

        let before = focused.then(|| self.tabs[self.active].text.clone());
        let dark = ui.visuals().dark_mode;
        let Self {
            tabs,
            active,
            highlighter,
            ..
        } = self;
        let Tab { text, folds, .. } = &mut tabs[*active];
        let folds = &*folds;
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let buf = buf.as_str();
            let mut job = highlighter.job(buf, Lang::Console, dark);
            if !folds.is_empty() {
                // CON-09: region terlipat hanya disembunyikan di layout; buffer utuh.
                let hidden = fold::hidden_ranges(buf, &fold::fold_ranges(buf), folds);
                fold::apply_hidden(&mut job, &hidden);
            }
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let output = egui::ScrollArea::both()
            .id_salt(("console_scroll", *active))
            .auto_shrink(false)
            .show(ui, |ui| {
                egui::TextEdit::multiline(text)
                    .id(id)
                    .code_editor()
                    .lock_focus(true)
                    .desired_width(f32::INFINITY)
                    .desired_rows(40)
                    .margin(egui::Margin {
                        left: GUTTER as i8,
                        right: 4,
                        top: 4,
                        bottom: 4,
                    })
                    .layouter(&mut layouter)
                    .show(ui)
            })
            .inner;

        let text = &self.tabs[self.active].text;
        let blocks = parse::blocks(text);
        let cursor_char = output.cursor_range.map(|r| r.primary.index.0);
        let cursor = cursor_char.map(|c| char_to_byte(text, c));

        // Gutter: tombol ▶ per request + penanda blok aktif.
        let galley_pos = output.galley_pos;
        let painter = ui.painter_at(output.response.rect);
        let mut run_block = None;
        let active_block = cursor.and_then(|c| parse::block_at(&blocks, c));
        if output.response.changed()
            || output.response.gained_focus()
            || self.lint.is_some()
            || cursor.is_some()
        {
            self.lint = active_block.and_then(|b| lint(spec::for_major(cx.caps.major), b, text));
        }
        for (i, block) in blocks.iter().enumerate() {
            let start = output
                .galley
                .pos_from_cursor(CCursor::new(byte_to_char(text, block.start)));
            let y = galley_pos.y + start.min.y;
            let rect = egui::Rect::from_min_size(
                egui::pos2(output.response.rect.left() + 2.0, y),
                egui::vec2(20.0, start.height()),
            );
            if active_block == Some(block) {
                let end = output.galley.pos_from_cursor(CCursor::new(byte_to_char(
                    text,
                    block.end.saturating_sub(1).max(block.start),
                )));
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 18.0, y),
                    egui::pos2(rect.left() + 20.0, galley_pos.y + end.max.y),
                );
                painter.rect_filled(bar, 0.0, ui.visuals().selection.bg_fill);
            }
            let response = ui.interact(rect, id.with(("run", i)), egui::Sense::click());
            let color = if response.hovered() {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "▶",
                egui::FontId::proportional(11.0),
                color,
            );
            if response.on_hover_text(t("Jalankan request ini")).clicked() {
                run_block = Some(block.clone());
            }
        }
        if let Some(block) = run_block {
            let text = self.tabs[self.active].text.clone();
            self.run_blocks(&ctx, cx, &text, &[block]);
        }
        self.fold_gutter(ui, &output, cursor_char);

        // Ketikan: auto-close, auto-indent, dan pemicu autocomplete.
        let mut trigger = std::mem::take(&mut self.force_popup);
        if output.response.changed() {
            self.dirty_since = Some(Instant::now());
            if let (Some(before), Some(cursor_char)) = (before, cursor_char) {
                let inserted = self.after_edit(&ctx, &before, cursor_char);
                trigger |=
                    inserted.is_some_and(|c| c.is_alphanumeric() || "_/?&\".:=@-".contains(c));
                trigger |= self.popup.is_some() && inserted.is_none();
            }
        } else if self.popup.is_some() && output.response.clicked() {
            self.popup = None;
        }
        if trigger {
            self.update_popup(&ctx, cx);
        } else if let Some(popup) = &self.popup
            && popup.loading_fields
        {
            self.update_popup(&ctx, cx);
        }
        if !focused {
            self.popup = None;
        }
        self.popup_ui(&ctx, &output);
    }

    /// CON-09: penanda ▾/▸ di gutter, placeholder region terlipat, dan sinkron status lipatan.
    fn fold_gutter(
        &mut self,
        ui: &egui::Ui,
        output: &egui::text_edit::TextEditOutput,
        cursor_char: Option<usize>,
    ) {
        let id = self.editor_id();
        let tab = &mut self.tabs[self.active];
        let text = tab.text.as_str();
        let ranges = fold::fold_ranges(text);
        fold::prune(&mut tab.folds, &ranges);
        // Cursor masuk ke region tersembunyi (mis. panah bawah): buka lipatannya.
        if let Some(c) = cursor_char.map(|c| char_to_byte(text, c)) {
            for &(start, end) in &ranges {
                let opener_end = text[start..].find('\n').map_or(end, |i| start + i);
                if tab.folds.contains(&start) && opener_end < c && c <= end {
                    tab.folds.remove(&start);
                }
            }
        }
        let hidden = fold::hidden_ranges(text, &ranges, &tab.folds);
        let galley_pos = output.galley_pos;
        let painter = ui.painter_at(output.response.rect);
        let clip = ui.clip_rect();
        // Rentang terurut naik: konversi char dan pencarian baris galley cukup satu lintasan.
        let mut chars = fold::CharCounter::new(text);
        let mut rows = fold::RowWalker::new(&output.galley);
        let mut hidden_iter = hidden.iter().peekable();
        let mut toggle = None;
        for (i, &(start, end)) in ranges.iter().enumerate() {
            while hidden_iter.next_if(|&&(_, he)| he <= start).is_some() {}
            // Pembuka yang berada di dalam region terlipat lain tidak terlihat.
            if hidden_iter.peek().is_some_and(|&&(hs, _)| hs <= start) {
                continue;
            }
            let opener_end = text[start..].find('\n').map_or(end, |i| start + i);
            let row = rows.pos(chars.at(start));
            let y = galley_pos.y + row.min.y;
            if y > clip.bottom() {
                break;
            }
            if y + row.height() < clip.top() {
                continue;
            }
            let rect = egui::Rect::from_min_size(
                egui::pos2(output.response.rect.left() + 22.0, y),
                egui::vec2(GUTTER - 24.0, row.height()),
            );
            let folded = tab.folds.contains(&start);
            let response = ui.interact(rect, id.with(("fold", i)), egui::Sense::click());
            let color = if response.hovered() {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if folded { "▸" } else { "▾" },
                egui::FontId::proportional(11.0),
                color,
            );
            if folded {
                let at = rows.pos(chars.at(opener_end));
                painter.text(
                    egui::pos2(galley_pos.x + at.max.x + 6.0, galley_pos.y + at.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("… {}", fold::closer(text, start)),
                    egui::FontId::monospace(widgets::MONO_SIZE),
                    ui.visuals().weak_text_color(),
                );
            }
            let tip = if folded {
                t("Buka lipatan")
            } else {
                t("Lipat")
            };
            if response.on_hover_text(tip).clicked() {
                toggle = Some((start, opener_end, end, folded));
            }
        }
        if let Some((start, opener_end, end, folded)) = toggle {
            if folded {
                tab.folds.remove(&start);
            } else {
                tab.folds.insert(start);
                // Cursor di dalam region yang dilipat dipindah ke akhir baris pembuka.
                if cursor_char.is_some_and(|c| {
                    let c = char_to_byte(text, c);
                    opener_end < c && c <= end
                }) {
                    set_cursor(ui.ctx(), id, byte_to_char(text, opener_end));
                }
            }
            self.dirty_since = Some(Instant::now());
        }
    }

    /// Tangani satu karakter yang baru diketik. Mengembalikan karakter itu.
    fn after_edit(
        &mut self,
        ctx: &egui::Context,
        before: &str,
        cursor_char: usize,
    ) -> Option<char> {
        let text = &mut self.tabs[self.active].text;
        if text.chars().count() != before.chars().count() + 1 || cursor_char == 0 {
            return None;
        }
        let at = char_to_byte(text, cursor_char - 1);
        let typed = text[at..].chars().next()?;
        let pos = at + typed.len_utf8();
        let next = text[pos..].chars().next();
        let prev = text[..at].chars().next_back();
        let mut insert = String::new();
        let mut cursor_after = cursor_char;
        match typed {
            '{' | '[' if next.is_none_or(|c| c.is_whitespace() || ",}]".contains(c)) => {
                insert.push(if typed == '{' { '}' } else { ']' });
            }
            '"' if next == Some('"')
                && prev != Some('\\')
                && text[..at].matches('"').count() % 2 == 1 =>
            {
                // Mengetik kutip penutup di depan kutip otomatis: lewati.
                text.remove(pos);
            }
            '"' if next.is_none_or(|c| c.is_whitespace() || ",}]:".contains(c))
                && prev.is_none_or(|c| !c.is_alphanumeric()) =>
            {
                insert.push('"');
            }
            '}' | ']' if next == Some(typed) => {
                text.remove(pos);
            }
            '\n' => {
                let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                let indent: String = text[line_start..at]
                    .chars()
                    .take_while(|c| *c == ' ')
                    .collect();
                let opener = text[..at].trim_end_matches(' ').ends_with(['{', '[']);
                let closer = next.is_some_and(|c| c == '}' || c == ']');
                let inner = if opener {
                    format!("{indent}  ")
                } else {
                    indent.clone()
                };
                insert.push_str(&inner);
                cursor_after += inner.chars().count();
                if opener && closer {
                    insert.push('\n');
                    insert.push_str(&indent);
                }
            }
            _ => {}
        }
        if !insert.is_empty() {
            text.insert_str(pos, &insert);
        }
        if cursor_after != cursor_char || !insert.is_empty() {
            set_cursor(ctx, self.editor_id(), cursor_after);
        }
        Some(typed)
    }

    fn update_popup(&mut self, ctx: &egui::Context, cx: &mut Ctx) {
        let (cursor, _) = self.cursor_byte(ctx);
        let text = &self.tabs[self.active].text;
        let blocks = parse::blocks(text);
        let Some(located) = complete::locate(text, cursor, &blocks) else {
            self.popup = None;
            return;
        };
        let indices: Vec<(String, &'static str)> = cx
            .catalog
            .targets(ctx, cx.conn)
            .iter()
            .map(|t| (t.name.clone(), t.kind.label()))
            .collect();
        let spec = spec::for_major(cx.caps.major);
        let mut named: Vec<(String, String)> = Vec::new();
        let mut loading_names = false;
        if let complete::Context::Path { method, done } = &located.ctx {
            let done: Vec<&str> = done.iter().map(String::as_str).collect();
            for key in spec.param_sources(method, &done) {
                match cx.catalog.names(ctx, cx.conn, &key) {
                    Some(names) => named.extend(names.iter().map(|n| (n.clone(), key.clone()))),
                    None => loading_names = true,
                }
            }
        }
        let mut outcome = complete::suggest(
            spec,
            &located,
            &Live {
                indices: &indices,
                fields: None,
                named: &named,
                terms: None,
            },
        );
        let mut loading_fields = loading_names;
        if let Some(pattern) = outcome.wants_fields.take() {
            match cx.catalog.fields(ctx, cx.conn, &pattern) {
                Some(Ok(fields)) => {
                    let fields: Vec<(String, String)> = fields
                        .iter()
                        .map(|f| (f.name.clone(), f.types.join("|")))
                        .collect();
                    outcome = complete::suggest(
                        spec,
                        &located,
                        &Live {
                            indices: &indices,
                            fields: Some(&fields),
                            named: &named,
                            terms: None,
                        },
                    );
                }
                Some(Err(_)) => {}
                None => loading_fields = true,
            }
        }
        if let Some((index, field)) = outcome.wants_terms.take() {
            let key = (index.clone(), field.clone(), located.prefix.clone());
            match self.terms_cache.get(&key) {
                Some(terms) => {
                    let terms = terms.clone();
                    outcome = complete::suggest(
                        spec,
                        &located,
                        &Live {
                            indices: &indices,
                            fields: None,
                            named: &named,
                            terms: Some(&terms),
                        },
                    );
                }
                None => {
                    loading_fields = true;
                    if self.terms_pending.is_none() {
                        let conn = cx.conn.clone();
                        let body = serde_json::json!({"field": field, "string": located.prefix, "size": 20});
                        self.terms_pending = Some(Pending::spawn(ctx, move || {
                            let terms = conn
                                .json("POST", &format!("/{index}/_terms_enum"), Some(&body))
                                .map(|v| {
                                    v["terms"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .filter_map(|t| t.as_str().map(str::to_owned))
                                        .collect()
                                })
                                .unwrap_or_default();
                            (key, terms)
                        }));
                    }
                }
            }
        }
        if outcome.items.is_empty() && !loading_fields {
            self.popup = None;
            return;
        }
        let selected = self
            .popup
            .as_ref()
            .and_then(|p| p.items.get(p.selected))
            .and_then(|s| outcome.items.iter().position(|x| x.label == s.label))
            .unwrap_or(0);
        self.popup = Some(Popup {
            located,
            items: outcome.items,
            selected,
            loading_fields,
        });
    }

    fn accept_suggestion(&mut self, ctx: &egui::Context, index: Option<usize>) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some(item) = popup.items.get(index.unwrap_or(popup.selected)) else {
            return;
        };
        let (cursor, _) = self.cursor_byte(ctx);
        let text = &mut self.tabs[self.active].text;
        if cursor < popup.located.start || cursor > text.len() {
            return;
        }
        let (new_text, new_cursor) = complete::apply(text, &popup.located, cursor, item);
        *text = new_text;
        let char_index = byte_to_char(text, new_cursor);
        set_cursor(ctx, self.editor_id(), char_index);
        ctx.memory_mut(|m| m.request_focus(self.editor_id()));
        self.dirty_since = Some(Instant::now());
        // Lanjutkan saran setelah path/method, mis. `GET ` -> daftar endpoint.
        if item.insert.ends_with(' ') || item.insert.ends_with('=') {
            self.force_popup = true;
        }
    }

    fn popup_ui(&mut self, ctx: &egui::Context, output: &egui::text_edit::TextEditOutput) {
        let Some(popup) = &self.popup else {
            return;
        };
        let Some(range) = output.cursor_range else {
            return;
        };
        let rect = output.galley.pos_from_cursor(range.primary);
        let pos = output.galley_pos + rect.left_bottom().to_vec2() + egui::vec2(0.0, 2.0);
        let mut clicked = None;
        egui::Area::new(egui::Id::unique("console_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(280.0);
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            if popup.loading_fields {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.weak(t("memuat saran dari cluster…"));
                                });
                            }
                            for (i, item) in popup.items.iter().enumerate() {
                                let selected = i == popup.selected;
                                let response = ui
                                    .horizontal(|ui| {
                                        let r = ui.selectable_label(
                                            selected,
                                            RichText::new(&item.label).monospace(),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.weak(&item.detail);
                                            },
                                        );
                                        r
                                    })
                                    .inner;
                                if selected {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                    ui.weak(t("↑↓ pilih · Enter/Tab terima · Esc tutup"));
                });
            });
        if let Some(i) = clicked {
            self.accept_suggestion(ctx, Some(i));
        }
    }

    fn selected_blocks(&self, ctx: &egui::Context) -> Vec<Block> {
        let text = &self.tabs[self.active].text;
        let blocks = parse::blocks(text);
        let (cursor, selection) = self.cursor_byte(ctx);
        match selection {
            Some((from, to)) => blocks
                .into_iter()
                .filter(|b| b.start < to && b.end > from)
                .collect(),
            None => parse::block_at(&blocks, cursor)
                .cloned()
                .into_iter()
                .collect(),
        }
    }

    fn run_at_cursor(&mut self, ctx: &egui::Context, cx: &mut Ctx) {
        let blocks = self.selected_blocks(ctx);
        if blocks.is_empty() {
            self.notice = Some(t("Tidak ada request di posisi cursor.").into());
            return;
        }
        let text = self.tabs[self.active].text.clone();
        self.run_blocks(ctx, cx, &text, &blocks);
    }

    fn run_blocks(&mut self, ctx: &egui::Context, cx: &mut Ctx, text: &str, blocks: &[Block]) {
        self.popup = None;
        let mut prepared = Vec::new();
        for block in blocks {
            let raw_body = block.body(text).trim().to_owned();
            let expanded = parse::substitute(&block.path, &self.vars).and_then(|path| {
                let body = parse::substitute(&raw_body, &self.vars)?;
                parse::prepare_body(&body, is_ndjson(&path)).map(|b| (path, b))
            });
            match expanded {
                Ok((path, body)) => prepared.push(Prepared {
                    method: block.method.clone(),
                    path,
                    raw_body,
                    body,
                }),
                Err(err) => {
                    self.results = vec![RunResult {
                        method: block.method.clone(),
                        path: block.path.clone(),
                        result: Err(err),
                        pretty: String::new(),
                        tree: None,
                    }];
                    return;
                }
            }
        }
        let (source, profile) = match self.target(cx) {
            Ok(t) => t,
            Err(err) => {
                self.notice = Some(err);
                return;
            }
        };
        let dangerous = prepared.iter().any(|p| is_dangerous(&p.method, &p.path));
        if profile.is_prod() && dangerous && !profile.read_only {
            self.confirm = Some(prepared);
            self.confirm_typed.clear();
            return;
        }
        self.execute(ctx, source, prepared);
    }

    /// Koneksi + profile untuk tab aktif (CON-64).
    /// `Conn` profile lain dibangun di thread eksekusi, bukan di sini (membaca keychain OS).
    fn target(&self, cx: &Ctx) -> Result<(Source, crate::profile::Profile), String> {
        let Some(id) = self.tabs[self.active]
            .target
            .clone()
            .filter(|id| *id != cx.profile.id)
        else {
            return Ok((Source::Ready(cx.conn.clone()), cx.profile.clone()));
        };
        let profile = cx
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or(t("Profile target tab ini sudah dihapus."))?;
        // Dibuat per eksekusi supaya perubahan profile target langsung berlaku.
        Ok((Source::Profile(Box::new(profile.clone())), profile))
    }

    fn execute(&mut self, ctx: &egui::Context, source: Source, prepared: Vec<Prepared>) {
        let opaque = format!(
            "elastic-gui-{:x}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        self.running = Some((source.clone(), opaque.clone()));
        self.notice = None;
        self.started = Some(Instant::now());
        self.pending = Some(Pending::spawn(ctx, move || {
            let conn = match source.open() {
                Ok(conn) => conn,
                Err(err) => {
                    return prepared
                        .into_iter()
                        .map(|p| (p, Err(err.clone())))
                        .collect();
                }
            };
            prepared
                .into_iter()
                .map(|p| {
                    let result =
                        conn.send_tagged(&p.method, &p.path, p.body.as_deref(), Some(&opaque));
                    (p, result)
                })
                .collect()
        }));
    }

    fn poll_results(&mut self, cx: &mut Ctx) {
        if let Some((key, terms)) = poll(&mut self.terms_pending) {
            self.terms_cache.insert(key, terms);
        }
        if let Some(r) = poll(&mut self.cancel_pending) {
            self.notice = Some(match r {
                Ok(0) => {
                    t("Dibatalkan. Task server sudah selesai atau tidak bisa dibatalkan.").into()
                }
                Ok(n) => tf("Dibatalkan. {0} task di server dihentikan.", &[&n]),
                Err(e) => tf(
                    "Dibatalkan di aplikasi, tetapi gagal membatalkan task server: {0}",
                    &[&e],
                ),
            });
        }
        let Some(done) = poll(&mut self.pending) else {
            return;
        };
        self.running = None;
        self.started = None;
        let now = chrono::Utc::now().timestamp_millis();
        self.results = done
            .into_iter()
            .map(|(p, result)| {
                let (status, millis) = match &result {
                    Ok(r) => (Some(r.status), r.millis),
                    Err(_) => (None, 0),
                };
                self.history.insert(
                    0,
                    HistoryItem {
                        ts: now,
                        method: p.method.clone(),
                        path: p.path.clone(),
                        body: p.raw_body.clone(),
                        status,
                        millis,
                    },
                );
                let (pretty, tree) = match &result {
                    Ok(r) => pretty_body(&r.body),
                    Err(_) => (String::new(), None),
                };
                RunResult {
                    method: p.method,
                    path: p.path,
                    result,
                    pretty,
                    tree,
                }
            })
            .collect();
        self.history.truncate(HISTORY_MAX);
        cx.store.save(&cx.profile.id, HISTORY_FILE, &self.history);
        // Perubahan index bisa membuat cache autocomplete basi.
        if self
            .results
            .iter()
            .any(|r| r.method != "GET" && r.method != "HEAD")
        {
            cx.catalog.invalidate();
        }
    }

    fn response_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        if let Some(started) = self.started {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tf(
                    "Menjalankan… {0} dtk",
                    &[&format!("{:.1}", started.elapsed().as_secs_f32())],
                ));
            });
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        if let Some(notice) = &self.notice {
            widgets::warn_box(ui, notice);
        }
        if self.results.is_empty() {
            if self.started.is_none() {
                ui.weak(t("Hasil request akan tampil di sini."));
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tree_view, false, t("Teks"));
            ui.selectable_value(&mut self.tree_view, true, "Tree");
        });
        let multiple = self.results.len() > 1;
        let tree_view = self.tree_view;
        let Self {
            results,
            response_highlighter,
            ..
        } = self;
        egui::ScrollArea::both()
            .id_salt("console_response")
            .auto_shrink(false)
            .show(ui, |ui| {
                for (i, r) in results.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if multiple {
                            ui.monospace(format!("{} {}", r.method, r.path));
                        }
                        match &r.result {
                            Ok(resp) => {
                                let color = if resp.is_ok() {
                                    egui::Color32::from_rgb(0x2e, 0xa0, 0x43)
                                } else {
                                    ui.visuals().error_fg_color
                                };
                                ui.label(RichText::new(resp.status.to_string()).color(color).strong());
                                ui.weak(format!(
                                    "{} ms · {}",
                                    resp.millis,
                                    widgets::format_bytes(resp.body.len() as f64)
                                ));
                                if ui.small_button(t("Salin")).clicked() {
                                    ui.ctx().copy_text(resp.body.clone());
                                }
                                if resp.body.len() > VIEW_LIMIT && ui.small_button(t("Simpan ke file")).clicked() {
                                    save_to_file(&resp.body);
                                }
                            }
                            Err(_) => {
                                ui.label(RichText::new(t("Gagal")).color(ui.visuals().error_fg_color).strong());
                            }
                        }
                    });
                    match &r.result {
                        Err(err) => widgets::error_box(ui, err),
                        Ok(resp) => {
                            for w in &resp.warnings {
                                widgets::warn_box(ui, &format!("Deprecation: {w}"));
                            }
                            if tree_view && let Some(tree) = &r.tree {
                                widgets::json_tree(ui, tree, egui::Id::unique(("resp_tree", i)));
                            } else if !multiple {
                                widgets::code_view(ui, &r.pretty, Lang::Json, response_highlighter);
                            } else {
                                // Banyak response: tanpa cache highlight bersama.
                                widgets::code_view(ui, &r.pretty, Lang::Json, &mut Highlighter::default());
                            }
                            if resp.body.len() > VIEW_LIMIT {
                                ui.weak(tf(
                                    "Tampilan dipotong ({0}). Simpan ke file untuk melihat semuanya.",
                                    &[&widgets::format_bytes(resp.body.len() as f64)],
                                ));
                            }
                        }
                    }
                    ui.separator();
                }
            });
        let _ = cx;
    }

    fn vars_ui(&mut self, ui: &mut egui::Ui, cx: &mut Ctx) {
        ui.heading(t("Variabel"));
        ui.weak(t("Pakai di path atau body sebagai ${nama}."));
        let mut remove = None;
        let mut changed = false;
        egui::Grid::new("console_vars_grid")
            .num_columns(3)
            .show(ui, |ui| {
                for (i, (name, value)) in self.vars.iter_mut().enumerate() {
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(name)
                                .desired_width(90.0)
                                .hint_text(match i18n::lang() {
                                    i18n::Lang::Id => "nama",
                                    i18n::Lang::En => "name",
                                }),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(value)
                                .desired_width(140.0)
                                .hint_text(match i18n::lang() {
                                    i18n::Lang::Id => "nilai",
                                    i18n::Lang::En => "value",
                                }),
                        )
                        .changed();
                    if ui.small_button("×").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            self.vars.remove(i);
            changed = true;
        }
        if ui.small_button(t("+ variabel")).clicked() {
            self.vars.push((String::new(), String::new()));
            changed = true;
        }
        if changed {
            cx.store.save(&cx.profile.id, VARS_FILE, &self.vars);
        }
    }

    fn history_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("History");
        ui.add(egui::TextEdit::singleline(&mut self.history_search).hint_text(t("Cari…")));
        let needle = self.history_search.to_lowercase();
        let mut insert = None;
        egui::ScrollArea::vertical()
            .id_salt("history_list")
            .show(ui, |ui| {
                for item in self
                    .history
                    .iter()
                    .filter(|h| {
                        needle.is_empty()
                            || format!("{} {} {}", h.method, h.path, h.body)
                                .to_lowercase()
                                .contains(&needle)
                    })
                    .take(300)
                {
                    let status = item.status.map_or(t("gagal").to_owned(), |s| s.to_string());
                    let when = widgets::format_time_ms(item.ts);
                    let response = ui
                        .add(
                            egui::Button::new(
                                RichText::new(format!("{} {}", item.method, item.path)).monospace(),
                            )
                            .wrap(),
                        )
                        .on_hover_text(tf(
                            "{0} · {1} · {2} ms\nKlik untuk menyisipkan",
                            &[&when, &status, &item.millis],
                        ));
                    if response.clicked() {
                        insert = Some(item.clone());
                    }
                    ui.weak(format!("{when} · {status}"));
                    ui.add_space(4.0);
                }
            });
        if let Some(item) = insert {
            let body = (!item.body.is_empty()).then(|| item.body.clone());
            let mut text = format!("{} {}\n", item.method, item.path);
            if let Some(body) = body {
                text.push_str(&body);
                text.push('\n');
            }
            self.incoming = Some(text);
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, cx: &mut Ctx) {
        let Some(prepared) = &self.confirm else {
            return;
        };
        let list: Vec<String> = prepared
            .iter()
            .map(|p| format!("{} {}", p.method, p.path))
            .collect();
        let message = tf(
            "Profile PROD. Request berikut dapat menghapus atau mengubah data:\n\n{0}",
            &[&list.join("\n")],
        );
        let wildcard = prepared
            .iter()
            .any(|p| p.method == "DELETE" && p.path.contains('*'));
        let expected = wildcard.then_some("HAPUS");
        match widgets::confirm_dialog(
            ctx,
            t("Konfirmasi request berbahaya"),
            &message,
            expected,
            &mut self.confirm_typed,
            true,
        ) {
            Confirm::Yes => {
                if let Some(prepared) = self.confirm.take() {
                    match self.target(cx) {
                        Ok((source, _)) => self.execute(ctx, source, prepared),
                        Err(err) => self.notice = Some(err),
                    }
                }
            }
            Confirm::No => self.confirm = None,
            Confirm::Pending => {}
        }
    }

    fn format_at_cursor(&mut self, ctx: &egui::Context) {
        let blocks = self.selected_blocks(ctx);
        let text = &mut self.tabs[self.active].text;
        // Dari belakang supaya offset blok sebelumnya tetap valid.
        for block in blocks.iter().rev() {
            let body = block.body(text);
            if body.trim().is_empty() {
                continue;
            }
            match parse::format_body(body, is_ndjson(&block.path)) {
                Ok(formatted) => {
                    let trailing = if body.ends_with("\n\n") { "\n" } else { "" };
                    text.replace_range(
                        block.body_start..block.end,
                        &format!("{formatted}{trailing}"),
                    );
                    self.dirty_since = Some(Instant::now());
                }
                Err(err) => self.notice = Some(err),
            }
        }
    }

    fn copy_curl(&mut self, ctx: &egui::Context, conn: &Conn) {
        let Some(block) = self.selected_blocks(ctx).into_iter().next() else {
            return;
        };
        let text = &self.tabs[self.active].text;
        let raw = parse::substitute(block.body(text), &self.vars)
            .unwrap_or_else(|_| block.body(text).to_owned());
        let body = parse::prepare_body(&raw, is_ndjson(&block.path))
            .ok()
            .flatten();
        let path =
            parse::substitute(&block.path, &self.vars).unwrap_or_else(|_| block.path.clone());
        let path = format!("/{}", path.trim_start_matches('/'));
        let mut curl = format!("curl -X {} \"{}{}\"", block.method, conn.base_url(), path);
        match conn.auth_kind {
            AuthKind::Basic => curl.push_str(&format!(" -u '{}:****'", conn.username)),
            AuthKind::ApiKey => curl.push_str(" -H 'Authorization: ApiKey ****'"),
            AuthKind::Bearer => curl.push_str(" -H 'Authorization: Bearer ****'"),
            AuthKind::None => {}
        }
        if let Some(body) = body {
            let content_type = if is_ndjson(&block.path) {
                "application/x-ndjson"
            } else {
                "application/json"
            };
            curl.push_str(&format!(
                " -H 'Content-Type: {content_type}' --data-binary '{}'",
                body.replace('\'', "'\\''")
            ));
        }
        ctx.copy_text(curl);
        self.notice = Some(t("cURL disalin (kredensial disamarkan).").into());
    }
}

/// CON-06: periksa endpoint, method, dan JSON body satu request.
fn lint(spec: &spec::Spec, block: &Block, text: &str) -> Option<String> {
    let path = block.path.trim();
    if path.is_empty() {
        return Some(t("Path request kosong.").into());
    }
    if spec.find(&block.method, path).is_none() {
        let methods = spec.methods_for(path);
        return Some(if methods.is_empty() {
            tf(
                "Endpoint {0} tidak dikenal di spesifikasi API cluster ini.",
                &[&path],
            )
        } else {
            tf(
                "{0} tidak didukung untuk {1}. Gunakan: {2}.",
                &[&block.method, &path, &methods.join(", ")],
            )
        });
    }
    parse::prepare_body(block.body(text), is_ndjson(path)).err()
}

fn is_dangerous(method: &str, path: &str) -> bool {
    method == "DELETE"
        || [
            "_delete_by_query",
            "_close",
            "_update_by_query",
            "_shrink",
            "_split",
            "_forcemerge",
        ]
        .iter()
        .any(|s| path.contains(s))
}

fn pretty_body(body: &str) -> (String, Option<Value>) {
    let shown = if body.len() > VIEW_LIMIT {
        let mut end = VIEW_LIMIT;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        &body[..end]
    } else {
        body
    };
    match serde_json::from_str::<Value>(body) {
        Ok(value) if body.len() <= VIEW_LIMIT => {
            let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_owned());
            (pretty, Some(value))
        }
        Ok(value) => (shown.to_owned(), Some(value)),
        Err(_) => (shown.to_owned(), None),
    }
}

fn save_to_file(body: &str) {
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name("response.json")
        .save_file()
        && let Err(err) = std::fs::write(&path, body)
    {
        eprintln!("gagal menyimpan {}: {err}", path.display());
    }
}

fn set_cursor(ctx: &egui::Context, id: egui::Id, char_index: usize) {
    if let Some(mut state) = TextEditState::load(ctx, id) {
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(char_index))));
        state.store(ctx, id);
    }
}

pub fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(i, _)| i)
}

pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    (
        "# Ctrl/Cmd+Enter: jalankan request di cursor. Ctrl+Space: saran.\n\
# Ctrl/Cmd+I: rapikan JSON. Seleksi beberapa request untuk menjalankan semuanya.\n\
GET /_cluster/health\n\n\
GET /_cat/indices?v&s=store.size:desc\n\n\
GET /_search\n{\n  \"query\": {\n    \"match_all\": {}\n  }\n}\n",
        "# Ctrl/Cmd+Enter: run the request at the cursor. Ctrl+Space: suggestions.\n\
# Ctrl/Cmd+I: format JSON. Select several requests to run them all.\n\
GET /_cluster/health\n\n\
GET /_cat/indices?v&s=store.size:desc\n\n\
GET /_search\n{\n  \"query\": {\n    \"match_all\": {}\n  }\n}\n",
    ),
    ("Klik dua kali untuk ganti nama", "Double-click to rename"),
    ("Tab baru", "New tab"),
    ("■ Batal", "■ Cancel"),
    (
        "Dibatalkan. Membatalkan task di server…",
        "Cancelled. Cancelling the task on the server…",
    ),
    ("▶ Jalankan", "▶ Run"),
    ("Rapikan", "Format"),
    ("Salin cURL", "Copy cURL"),
    ("Kredensial disamarkan", "Credentials are masked"),
    ("Impor cURL", "Import cURL"),
    ("Impor", "Import"),
    ("Variabel ({0})", "Variables ({0})"),
    ("Dok: {0}", "Docs: {0}"),
    ("{0} (aktif)", "{0} (active)"),
    (
        "Cluster tujuan request di tab ini",
        "Target cluster for requests in this tab",
    ),
    ("Jalankan request ini", "Run this request"),
    ("Buka lipatan", "Unfold"),
    ("Lipat", "Fold"),
    (
        "memuat saran dari cluster…",
        "loading suggestions from the cluster…",
    ),
    (
        "↑↓ pilih · Enter/Tab terima · Esc tutup",
        "↑↓ select · Enter/Tab accept · Esc close",
    ),
    (
        "Tidak ada request di posisi cursor.",
        "No request at the cursor position.",
    ),
    (
        "Profile target tab ini sudah dihapus.",
        "The target profile of this tab has been deleted.",
    ),
    (
        "Dibatalkan. Task server sudah selesai atau tidak bisa dibatalkan.",
        "Cancelled. The server task has already finished or cannot be cancelled.",
    ),
    (
        "Dibatalkan. {0} task di server dihentikan.",
        "Cancelled. {0} task(s) stopped on the server.",
    ),
    (
        "Dibatalkan di aplikasi, tetapi gagal membatalkan task server: {0}",
        "Cancelled in the app, but cancelling the server task failed: {0}",
    ),
    ("Menjalankan… {0} dtk", "Running… {0} s"),
    (
        "Hasil request akan tampil di sini.",
        "Request results will appear here.",
    ),
    ("Teks", "Text"),
    ("Salin", "Copy"),
    ("Simpan ke file", "Save to file"),
    ("Gagal", "Failed"),
    (
        "Tampilan dipotong ({0}). Simpan ke file untuk melihat semuanya.",
        "Display truncated ({0}). Save to file to see everything.",
    ),
    ("Variabel", "Variables"),
    (
        "Pakai di path atau body sebagai ${nama}.",
        "Use in the path or body as ${name}.",
    ),
    ("+ variabel", "+ variable"),
    ("Cari…", "Search…"),
    ("gagal", "failed"),
    (
        "{0} · {1} · {2} ms\nKlik untuk menyisipkan",
        "{0} · {1} · {2} ms\nClick to insert",
    ),
    (
        "Profile PROD. Request berikut dapat menghapus atau mengubah data:\n\n{0}",
        "PROD profile. The following requests can delete or modify data:\n\n{0}",
    ),
    ("Konfirmasi request berbahaya", "Confirm dangerous request"),
    (
        "cURL disalin (kredensial disamarkan).",
        "cURL copied (credentials masked).",
    ),
    ("Path request kosong.", "The request path is empty."),
    (
        "Endpoint {0} tidak dikenal di spesifikasi API cluster ini.",
        "Endpoint {0} is not known in this cluster's API specification.",
    ),
    (
        "{0} tidak didukung untuk {1}. Gunakan: {2}.",
        "{0} is not supported for {1}. Use: {2}.",
    ),
    // Nama snippet bawaan (parse::SNIPPETS).
    ("Index terbesar", "Largest indices"),
    ("Node & disk", "Nodes & disk"),
    ("Shard unassigned", "Unassigned shards"),
    ("Task berjalan", "Running tasks"),
    ("Pencarian dasar", "Basic search"),
    ("Agregasi per waktu", "Aggregation over time"),
    ("Reindex (async)", "Reindex (async)"),
    // Pesan parser/cURL/variabel (parse.rs).
    (
        "JSON tidak valid (baris {0}, kolom {1}): {2}",
        "Invalid JSON (line {0}, column {1}): {2}",
    ),
    (
        "Body berisi lebih dari satu objek JSON. Pisahkan menjadi request terpisah.",
        "The body contains more than one JSON object. Split it into separate requests.",
    ),
    (
        "cURL: kutip tunggal tidak ditutup",
        "cURL: unclosed single quote",
    ),
    (
        "cURL: kutip ganda tidak ditutup",
        "cURL: unclosed double quote",
    ),
    (
        "Teks harus diawali `curl`.",
        "The text must start with `curl`.",
    ),
    (
        "URL tidak ditemukan di perintah cURL.",
        "No URL found in the cURL command.",
    ),
    (
        "Variabel `${` tidak ditutup dengan `}`.",
        "Variable `${` is not closed with `}`.",
    ),
    (
        "Variabel ${{0}} belum didefinisikan (panel Variabel).",
        "Variable ${{0}} is not defined (Variables panel).",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("mod.rs"), EN);
        // Nama yang sama di kedua bahasa memang tidak masuk tabel.
        let same = ["Cluster health", "Hot threads", "ES|QL"];
        crate::i18n::assert_keys(
            parse::SNIPPETS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| !same.contains(name)),
        );
        crate::i18n::assert_keys([WELCOME]);
    }

    #[test]
    fn lints_requests() {
        let spec = spec::for_major(9);
        let check = |text: &str| {
            let blocks = parse::blocks(text);
            lint(spec, &blocks[0], text)
        };
        assert_eq!(check("GET /_cluster/health\n"), None);
        assert!(check("GET /_bulk\n").unwrap().contains("POST"));
        assert!(
            check("GET /_tidak_ada_endpoint\n")
                .unwrap()
                .contains("tidak dikenal")
        );
        assert!(
            check("POST /x/_search\n{\"a\":\n")
                .unwrap()
                .contains("JSON")
        );
    }
}
