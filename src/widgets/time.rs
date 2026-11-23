//! Pemilih rentang waktu + auto-refresh.

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::es::query::{TimeBounds, resolve_time};
use crate::i18n::{t, tf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub from: String,
    pub to: String,
}

impl Default for TimeRange {
    fn default() -> Self {
        Self::new("now-15m", "now")
    }
}

const QUICK: [(&str, &str, &str); 10] = [
    ("15 menit terakhir", "now-15m", "now"),
    ("1 jam terakhir", "now-1h", "now"),
    ("4 jam terakhir", "now-4h", "now"),
    ("24 jam terakhir", "now-24h", "now"),
    ("7 hari terakhir", "now-7d", "now"),
    ("30 hari terakhir", "now-30d", "now"),
    ("90 hari terakhir", "now-90d", "now"),
    ("1 tahun terakhir", "now-1y", "now"),
    ("Hari ini", "now/d", "now"),
    ("Minggu ini", "now/w", "now"),
];

pub const REFRESH: [(u64, &str); 6] = [
    (0, "Mati"),
    (10, "10 detik"),
    (30, "30 detik"),
    (60, "1 menit"),
    (300, "5 menit"),
    (900, "15 menit"),
];

impl TimeRange {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }

    pub fn bounds(&self) -> Result<TimeBounds, String> {
        let now = chrono::Utc::now().timestamp_millis();
        let from = resolve_time(&self.from, now)
            .ok_or_else(|| tf("Waktu awal tidak dikenal: {0}", &[&self.from]))?;
        let to = resolve_time(&self.to, now)
            .ok_or_else(|| tf("Waktu akhir tidak dikenal: {0}", &[&self.to]))?;
        if from >= to {
            return Err(t("Waktu awal harus sebelum waktu akhir.").into());
        }
        Ok(TimeBounds { from, to })
    }

    pub fn label(&self) -> String {
        QUICK
            .iter()
            .find(|(_, f, t)| *f == self.from && *t == self.to)
            .map(|(label, _, _)| t(label).to_owned())
            .unwrap_or_else(|| format!("{} → {}", self.from, self.to))
    }
}

/// Mengembalikan `true` jika rentang berubah.
pub fn time_picker(ui: &mut egui::Ui, range: &mut TimeRange, refresh_secs: &mut u64) -> bool {
    let mut changed = false;
    let id = ui.scope_id().with("time_draft");
    ui.menu_button(format!("🕒 {}", range.label()), |ui| {
        ui.set_min_width(320.0);
        ui.label(egui::RichText::new(t("Cepat")).strong());
        egui::Grid::new("quick_ranges")
            .num_columns(2)
            .show(ui, |ui| {
                for (i, (label, from, to)) in QUICK.iter().enumerate() {
                    if ui
                        .selectable_label(range.from == *from && range.to == *to, t(label))
                        .clicked()
                    {
                        *range = TimeRange::new(from, to);
                        changed = true;
                        ui.close();
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        ui.separator();
        ui.label(egui::RichText::new(t("Kustom")).strong());
        ui.weak(t(
            "Date math (now-15m, now/d) atau tanggal (2026-11-01 08:00)",
        ));
        let mut draft = ui
            .data_mut(|d| d.get_temp::<(String, String)>(id))
            .unwrap_or_else(|| (range.from.clone(), range.to.clone()));
        egui::Grid::new("custom_range")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(t("Dari"));
                ui.text_edit_singleline(&mut draft.0);
                ui.end_row();
                ui.label(t("Sampai"));
                ui.text_edit_singleline(&mut draft.1);
                ui.end_row();
            });
        let candidate = TimeRange::new(draft.0.trim(), draft.1.trim());
        let check = candidate.bounds();
        if let Err(err) = &check {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        if ui
            .add_enabled(check.is_ok(), egui::Button::new(t("Terapkan")))
            .clicked()
        {
            *range = candidate;
            changed = true;
            ui.close();
        }
        ui.data_mut(|d| d.insert_temp(id, draft));
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Auto-refresh");
            for (secs, label) in REFRESH {
                ui.selectable_value(refresh_secs, secs, t(label));
            }
        });
    });
    if changed {
        ui.data_mut(|d| d.remove::<(String, String)>(id));
    }
    changed
}

/// X-05: terjemahan English untuk teks di file ini, termasuk label `QUICK` dan `REFRESH`.
pub const EN: &[(&str, &str)] = &[
    ("15 menit terakhir", "Last 15 minutes"),
    ("1 jam terakhir", "Last 1 hour"),
    ("4 jam terakhir", "Last 4 hours"),
    ("24 jam terakhir", "Last 24 hours"),
    ("7 hari terakhir", "Last 7 days"),
    ("30 hari terakhir", "Last 30 days"),
    ("90 hari terakhir", "Last 90 days"),
    ("1 tahun terakhir", "Last 1 year"),
    ("Hari ini", "Today"),
    ("Minggu ini", "This week"),
    ("Mati", "Off"),
    ("10 detik", "10 seconds"),
    ("30 detik", "30 seconds"),
    ("1 menit", "1 minute"),
    ("5 menit", "5 minutes"),
    ("15 menit", "15 minutes"),
    ("Waktu awal tidak dikenal: {0}", "Unknown start time: {0}"),
    ("Waktu akhir tidak dikenal: {0}", "Unknown end time: {0}"),
    (
        "Waktu awal harus sebelum waktu akhir.",
        "Start time must be before end time.",
    ),
    ("Cepat", "Quick"),
    ("Kustom", "Custom"),
    (
        "Date math (now-15m, now/d) atau tanggal (2026-11-01 08:00)",
        "Date math (now-15m, now/d) or a date (2026-11-01 08:00)",
    ),
    ("Dari", "From"),
    ("Sampai", "To"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("time.rs"), EN);
        crate::i18n::assert_keys(QUICK.iter().map(|(label, _, _)| *label));
        crate::i18n::assert_keys(REFRESH.iter().map(|(_, label)| *label));
    }
}
