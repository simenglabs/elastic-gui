//! Widget bersama: syntax highlight, JSON tree, dialog konfirmasi, util format.

pub mod filters;
pub mod time;

use std::hash::{DefaultHasher, Hash, Hasher};

use eframe::egui::{self, Color32, FontId, RichText, TextFormat, text::LayoutJob};
use serde_json::Value;

pub const MONO_SIZE: f32 = 13.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    /// Teks Console: baris `METHOD /path` + JSON.
    Console,
    Json,
}

struct Palette {
    method: Color32,
    path: Color32,
    key: Color32,
    string: Color32,
    number: Color32,
    keyword: Color32,
    comment: Color32,
    text: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            method: Color32::from_rgb(0x7e, 0xc6, 0x99),
            path: Color32::from_rgb(0xe6, 0xe6, 0xe6),
            key: Color32::from_rgb(0x79, 0xb8, 0xff),
            string: Color32::from_rgb(0xe9, 0xa7, 0x7b),
            number: Color32::from_rgb(0xb5, 0xce, 0xa8),
            keyword: Color32::from_rgb(0xc7, 0x92, 0xea),
            comment: Color32::from_rgb(0x80, 0x80, 0x80),
            text: Color32::from_rgb(0xc8, 0xc8, 0xc8),
        }
    } else {
        Palette {
            method: Color32::from_rgb(0x1a, 0x7f, 0x37),
            path: Color32::from_rgb(0x20, 0x20, 0x20),
            key: Color32::from_rgb(0x00, 0x5c, 0xc5),
            string: Color32::from_rgb(0xa3, 0x48, 0x1c),
            number: Color32::from_rgb(0x09, 0x86, 0x58),
            keyword: Color32::from_rgb(0x8a, 0x2b, 0xb9),
            comment: Color32::from_rgb(0x80, 0x80, 0x80),
            text: Color32::from_rgb(0x30, 0x30, 0x30),
        }
    }
}

/// Highlight dengan cache hasil terakhir (teks sama = tidak di-tokenize ulang).
#[derive(Default)]
pub struct Highlighter {
    key: u64,
    job: Option<LayoutJob>,
}

impl Highlighter {
    pub fn job(&mut self, text: &str, lang: Lang, dark: bool) -> LayoutJob {
        let mut hasher = DefaultHasher::new();
        (text, lang, dark).hash(&mut hasher);
        let key = hasher.finish();
        if self.key != key || self.job.is_none() {
            self.key = key;
            self.job = Some(highlight(text, lang, dark));
        }
        self.job.clone().unwrap_or_default()
    }
}

pub fn highlight(text: &str, lang: Lang, dark: bool) -> LayoutJob {
    let p = palette(dark);
    let font = FontId::monospace(MONO_SIZE);
    let mut job = LayoutJob::default();
    let mut push = |s: &str, color: Color32| {
        if !s.is_empty() {
            job.append(s, 0.0, TextFormat::simple(font.clone(), color));
        }
    };
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            push(line, p.comment);
            continue;
        }
        if lang == Lang::Console && !line.starts_with(char::is_whitespace) {
            let word = line.split(' ').next().unwrap_or_default();
            let upper = word.trim_end().to_ascii_uppercase();
            if crate::ui::console::parse::METHODS.contains(&upper.as_str()) {
                push(word, p.method);
                push(&line[word.len()..], p.path);
                continue;
            }
        }
        json_line(line, &p, &mut push);
    }
    job
}

fn json_line(line: &str, p: &Palette, push: &mut impl FnMut(&str, Color32)) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut plain_start = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let token_end = if c == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            Some((j + 1).min(bytes.len()))
        } else if c.is_ascii_digit()
            || (c == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'.' | b'+' | b'-'))
            {
                j += 1;
            }
            Some(j)
        } else if c.is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            Some(j)
        } else {
            None
        };
        let Some(end) = token_end else {
            i += 1;
            continue;
        };
        push(&line[plain_start..i], p.text);
        let token = &line[i..end];
        let color = if c == b'"' {
            let rest = line[end..].trim_start();
            if rest.starts_with(':') {
                p.key
            } else {
                p.string
            }
        } else if c.is_ascii_alphabetic() {
            if matches!(token, "true" | "false" | "null") {
                p.keyword
            } else {
                p.text
            }
        } else {
            p.number
        };
        push(token, color);
        i = end;
        plain_start = end;
    }
    push(&line[plain_start..], p.text);
}

/// Tampilan JSON berupa tree yang bisa dilipat; anak hanya dirender saat terbuka.
pub fn json_tree(ui: &mut egui::Ui, value: &Value, id: egui::Id) {
    json_node(ui, None, value, id, 0);
}

const TREE_LIMIT: usize = 500;

fn json_node(ui: &mut egui::Ui, key: Option<&str>, value: &Value, id: egui::Id, depth: usize) {
    let dark = ui.visuals().dark_mode;
    let p = palette(dark);
    let name = key.map(|k| format!("{k}: ")).unwrap_or_default();
    match value {
        Value::Object(map) => {
            let label = format!("{name}{{{}}}", map.len());
            egui::CollapsingHeader::new(RichText::new(label).monospace().color(p.key))
                .id_salt(id)
                .default_open(depth < 2)
                .show(ui, |ui| {
                    for (k, v) in map.iter().take(TREE_LIMIT) {
                        json_node(ui, Some(k), v, id.with(k), depth + 1);
                    }
                    if map.len() > TREE_LIMIT {
                        ui.weak(format!("… {} kunci lainnya", map.len() - TREE_LIMIT));
                    }
                });
        }
        Value::Array(items) => {
            let label = format!("{name}[{}]", items.len());
            egui::CollapsingHeader::new(RichText::new(label).monospace().color(p.key))
                .id_salt(id)
                .default_open(depth < 2 && items.len() <= 20)
                .show(ui, |ui| {
                    for (i, v) in items.iter().take(TREE_LIMIT).enumerate() {
                        json_node(ui, Some(&i.to_string()), v, id.with(i), depth + 1);
                    }
                    if items.len() > TREE_LIMIT {
                        ui.weak(format!("… {} item lainnya", items.len() - TREE_LIMIT));
                    }
                });
        }
        leaf => {
            let color = match leaf {
                Value::String(_) => p.string,
                Value::Number(_) => p.number,
                _ => p.keyword,
            };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new(name).monospace().color(p.key));
                let text = leaf.to_string();
                if ui
                    .add(
                        egui::Label::new(RichText::new(&text).monospace().color(color))
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Klik untuk menyalin")
                    .clicked()
                {
                    ui.ctx().copy_text(match leaf {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                }
            });
        }
    }
}

/// Teks read-only yang bisa diseleksi, dengan highlight.
pub fn code_view(ui: &mut egui::Ui, text: &str, lang: Lang, cache: &mut Highlighter) {
    let dark = ui.visuals().dark_mode;
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = cache.job(buf.as_str(), lang, dark);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let mut view = text;
    ui.add(
        egui::TextEdit::multiline(&mut view)
            .code_editor()
            .desired_width(f32::INFINITY)
            .layouter(&mut layouter),
    );
}

/// Hasil dialog konfirmasi.
pub enum Confirm {
    Pending,
    Yes,
    No,
}

/// Dialog konfirmasi modal. Jika `type_to_confirm` diisi, user wajib mengetik teks itu.
pub fn confirm_dialog(
    ctx: &egui::Context,
    title: &str,
    message: &str,
    type_to_confirm: Option<&str>,
    typed: &mut String,
    danger: bool,
) -> Confirm {
    let mut result = Confirm::Pending;
    let response = egui::Modal::new(egui::Id::unique("confirm_dialog")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.heading(title);
        ui.add_space(6.0);
        ui.label(message);
        let mut ok_enabled = true;
        if let Some(expected) = type_to_confirm {
            ui.add_space(6.0);
            ui.label(format!("Ketik {expected} untuk konfirmasi:"));
            ui.text_edit_singleline(typed);
            ok_enabled = typed.trim() == expected;
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let text = RichText::new("Lanjutkan");
            let text = if danger {
                text.color(Color32::WHITE)
            } else {
                text
            };
            let mut button = egui::Button::new(text);
            if danger {
                button = button.fill(Color32::from_rgb(0xc0, 0x39, 0x2b));
            }
            if ui.add_enabled(ok_enabled, button).clicked() {
                result = Confirm::Yes;
            }
            if ui.button("Batal").clicked() {
                result = Confirm::No;
            }
        });
    });
    if response.should_close() && matches!(result, Confirm::Pending) {
        result = Confirm::No;
    }
    result
}

/// Kotak pesan error dengan warna merah.
pub fn error_box(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(ui.visuals().error_fg_color.gamma_multiply(0.12))
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(ui.visuals().error_fg_color));
        });
}

pub fn warn_box(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new()
        .fill(ui.visuals().warn_fg_color.gamma_multiply(0.12))
        .inner_margin(8.0)
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(ui.visuals().warn_fg_color));
        });
}

/// Penanda highlight dari ES (`pre_tags`/`post_tags`); karakter private-use, tidak muncul di data normal.
pub const HL_START: char = '\u{E000}';
pub const HL_END: char = '\u{E001}';

pub fn strip_highlight(text: &str) -> String {
    text.replace([HL_START, HL_END], "")
}

/// Label satu baris dengan bagian yang cocok query diberi latar kuning.
pub fn highlight_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    if !text.contains(HL_START) {
        return ui.label(text);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let normal = TextFormat::simple(font.clone(), ui.visuals().text_color());
    let marked = TextFormat {
        background: Color32::from_rgba_unmultiplied(0xf5, 0xc2, 0x42, 0x70),
        ..TextFormat::simple(font, ui.visuals().strong_text_color())
    };
    let mut job = LayoutJob::default();
    let mut on = false;
    let mut chunk = String::new();
    for c in text.chars() {
        if c == HL_START || c == HL_END {
            job.append(
                &chunk,
                0.0,
                if on { marked.clone() } else { normal.clone() },
            );
            chunk.clear();
            on = c == HL_START;
        } else {
            chunk.push(c);
        }
    }
    job.append(&chunk, 0.0, if on { marked } else { normal });
    ui.label(job)
}

/// Sel CSV (RFC 4180): kutip jika ada koma, kutip, atau baris baru.
pub fn csv_cell(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}

pub fn format_time_ms(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1536.0), "1.5 KB");
        assert_eq!(format_count(1234567), "1.234.567");
        assert_eq!(format_count(12), "12");
    }

    #[test]
    fn highlight_keeps_text() {
        let text = "GET /x/_search\n{\"a\": \"b\", \"n\": -1.5, \"t\": true}\n# c\n";
        let job = highlight(text, Lang::Console, true);
        assert_eq!(job.text, text);
    }
}
