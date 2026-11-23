//! Filter pills: tambah, aktif/nonaktif, negasi, hapus.

use eframe::egui::{self, RichText};

use crate::es::catalog::Field;
use crate::es::query::{Filter, FilterOp};
use crate::i18n::t;

#[derive(Default)]
pub struct Draft {
    field: String,
    search: String,
    op: Option<FilterOp>,
    value: String,
    value2: String,
}

/// Mengembalikan `true` jika daftar filter berubah.
pub fn filter_bar(
    ui: &mut egui::Ui,
    filters: &mut Vec<Filter>,
    fields: &[Field],
    draft: &mut Draft,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for (i, f) in filters.iter_mut().enumerate() {
            let pin = if f.pinned { "📌 " } else { "" };
            let mut text = RichText::new(format!("{pin}{}", f.label()));
            if !f.enabled {
                text = text.strikethrough().weak();
            } else if f.negate {
                text = text.color(ui.visuals().error_fg_color);
            }
            ui.menu_button(text, |ui| {
                if ui
                    .button(if f.enabled {
                        t("Nonaktifkan")
                    } else {
                        t("Aktifkan")
                    })
                    .clicked()
                {
                    f.enabled = !f.enabled;
                    changed = true;
                    ui.close();
                }
                if ui
                    .button(if f.negate {
                        t("Hapus negasi")
                    } else {
                        t("Negasikan (NOT)")
                    })
                    .clicked()
                {
                    f.negate = !f.negate;
                    changed = true;
                    ui.close();
                }
                if ui
                    .button(if f.pinned {
                        t("Lepas pin")
                    } else {
                        t("Pin (tetap saat ganti sumber)")
                    })
                    .clicked()
                {
                    f.pinned = !f.pinned;
                    ui.close();
                }
                if f.op != FilterOp::Dsl && ui.button(t("Edit sebagai DSL")).clicked() {
                    f.make_dsl();
                }
                if f.op == FilterOp::Dsl {
                    ui.label("Query DSL");
                    let id = ui.make_persistent_id(("dsl_edit", i));
                    let mut text = ui
                        .data_mut(|d| d.get_temp::<String>(id))
                        .unwrap_or_else(|| f.value.clone());
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .code_editor()
                            .desired_rows(6)
                            .desired_width(360.0),
                    );
                    let parsed = serde_json::from_str::<serde_json::Value>(&text);
                    let valid = parsed.as_ref().is_ok_and(serde_json::Value::is_object);
                    if !valid {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            t("Harus objek JSON query yang valid."),
                        );
                    }
                    if ui
                        .add_enabled(valid, egui::Button::new(t("Terapkan")))
                        .clicked()
                    {
                        f.value = text.clone();
                        changed = true;
                        ui.data_mut(|d| d.remove::<String>(id));
                        ui.close();
                    } else {
                        ui.data_mut(|d| d.insert_temp(id, text));
                    }
                }
                if ui.button(t("Hapus")).clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        ui.menu_button("+ Filter", |ui| {
            if add_form(ui, fields, draft, filters) {
                changed = true;
                ui.close();
            }
        });
        if filters.iter().any(|f| !f.pinned) && ui.small_button(t("Hapus semua")).clicked() {
            filters.retain(|f| f.pinned);
            changed = true;
        }
    });
    if let Some(i) = remove {
        filters.remove(i);
        changed = true;
    }
    changed
}

fn add_form(
    ui: &mut egui::Ui,
    fields: &[Field],
    draft: &mut Draft,
    filters: &mut Vec<Filter>,
) -> bool {
    ui.set_min_width(340.0);
    ui.label("Field");
    ui.add(egui::TextEdit::singleline(&mut draft.search).hint_text(t("Cari field…")));
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .id_salt("filter_fields")
        .show(ui, |ui| {
            let needle = draft.search.to_lowercase();
            for f in fields
                .iter()
                .filter(|f| needle.is_empty() || f.name.to_lowercase().contains(&needle))
                .take(200)
            {
                let label = format!("{}  ·  {}", f.name, f.es_type());
                if ui.selectable_label(draft.field == f.name, label).clicked() {
                    draft.field = f.name.clone();
                }
            }
        });
    if fields.is_empty() {
        ui.add(egui::TextEdit::singleline(&mut draft.field).hint_text(t("nama field")));
    }
    let op = draft.op.get_or_insert(FilterOp::Is);
    ui.horizontal(|ui| {
        ui.label("Operator");
        egui::ComboBox::from_id_salt("filter_op")
            .selected_text(op.label())
            .show_ui(ui, |ui| {
                for candidate in FilterOp::ALL {
                    ui.selectable_value(op, candidate, candidate.label());
                }
            });
    });
    let op = *op;
    match op {
        FilterOp::Is => {
            ui.add(egui::TextEdit::singleline(&mut draft.value).hint_text(t("nilai")));
        }
        FilterOp::OneOf => {
            ui.add(egui::TextEdit::singleline(&mut draft.value).hint_text(t("nilai1, nilai2")));
        }
        FilterOp::Between => {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.value)
                        .hint_text(t("dari"))
                        .desired_width(140.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut draft.value2)
                        .hint_text(t("sampai"))
                        .desired_width(140.0),
                );
            });
        }
        FilterOp::Exists => {}
        FilterOp::Dsl => {
            ui.add(
                egui::TextEdit::multiline(&mut draft.value)
                    .code_editor()
                    .desired_rows(5)
                    .hint_text("{\"term\": {\"status\": \"active\"}}"),
            );
            ui.weak(t("Field diabaikan untuk filter DSL."));
        }
    }
    let ready = match op {
        FilterOp::Dsl => {
            serde_json::from_str::<serde_json::Value>(&draft.value).is_ok_and(|v| v.is_object())
        }
        _ => !draft.field.trim().is_empty(),
    } && match op {
        FilterOp::Is | FilterOp::OneOf => !draft.value.trim().is_empty(),
        FilterOp::Between => !draft.value.trim().is_empty() || !draft.value2.trim().is_empty(),
        FilterOp::Exists | FilterOp::Dsl => true,
    };
    if ui
        .add_enabled(ready, egui::Button::new(t("Tambah")))
        .clicked()
    {
        filters.push(Filter {
            field: draft.field.trim().to_owned(),
            op,
            value: draft.value.trim().to_owned(),
            value2: draft.value2.trim().to_owned(),
            negate: false,
            enabled: true,
            pinned: false,
        });
        *draft = Draft::default();
        return true;
    }
    false
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("Nonaktifkan", "Disable"),
    ("Aktifkan", "Enable"),
    ("Hapus negasi", "Remove negation"),
    ("Negasikan (NOT)", "Negate (NOT)"),
    ("Lepas pin", "Unpin"),
    (
        "Pin (tetap saat ganti sumber)",
        "Pin (keep when switching source)",
    ),
    ("Edit sebagai DSL", "Edit as DSL"),
    (
        "Harus objek JSON query yang valid.",
        "Must be a valid JSON query object.",
    ),
    ("Hapus semua", "Clear all"),
    ("Cari field…", "Search fields…"),
    ("nama field", "field name"),
    ("nilai", "value"),
    ("nilai1, nilai2", "value1, value2"),
    ("dari", "from"),
    ("sampai", "to"),
    (
        "Field diabaikan untuk filter DSL.",
        "Field is ignored for DSL filters.",
    ),
    ("Tambah", "Add"),
];

#[cfg(test)]
mod tests {
    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("filters.rs"), super::EN);
    }
}
