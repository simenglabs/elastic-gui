//! X-05: bahasa UI. Teks sumber (Bahasa Indonesia) sekaligus menjadi kunci terjemahan.
//!
//! Tiap modul punya tabel `pub const EN: &[(&str, &str)]` berisi (teks sumber, English)
//! yang didaftarkan di [`TABLES`]. Satu kunci hanya boleh ada di satu tabel; teks yang
//! dipakai banyak modul cukup ditaruh di [`COMMON`].

use std::collections::HashMap;
use std::fmt::{Display, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Id,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Id, Lang::En];

    /// Kode singkat untuk pemilih bahasa; tidak diterjemahkan.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Id => "ID",
            Lang::En => "EN",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Id,
    }
}

/// Teks yang dipakai di banyak modul.
pub const COMMON: &[(&str, &str)] = &[
    ("Batal", "Cancel"),
    ("Terapkan", "Apply"),
    ("Hapus", "Delete"),
    ("Simpan", "Save"),
    ("Pilih…", "Browse…"),
    ("Edit profile", "Edit profile"),
    ("Bahasa antarmuka", "Interface language"),
];

/// Semua tabel terjemahan. Task yang menerjemahkan modul baru menambah tabelnya di sini.
const TABLES: &[&[(&str, &str)]] = &[
    COMMON,
    crate::app::EN,
    crate::profile::EN,
    crate::ui::profiles::EN,
    crate::widgets::EN,
    crate::widgets::time::EN,
    crate::widgets::filters::EN,
    crate::es::catalog::EN,
    crate::es::query::EN,
    crate::ui::console::EN,
];

fn english() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| TABLES.iter().flat_map(|t| t.iter().copied()).collect())
}

/// Terjemahan `src` ke bahasa aktif; teks sumber bila bahasa Indonesia atau belum ada di tabel.
pub fn t(src: &'static str) -> &'static str {
    translate(lang(), src)
}

fn translate(lang: Lang, src: &'static str) -> &'static str {
    match lang {
        Lang::Id => src,
        Lang::En => english().get(src).copied().unwrap_or(src),
    }
}

/// Seperti [`t`], lalu `{0}`, `{1}`, … diganti argumen. Placeholder tanpa argumen dibiarkan.
pub fn tf(template: &'static str, args: &[&dyn Display]) -> String {
    fill(t(template), args)
}

fn fill(text: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0
            && after.as_bytes().get(digits) == Some(&b'}')
            && let Some(arg) = after[..digits]
                .parse::<usize>()
                .ok()
                .and_then(|i| args.get(i))
        {
            let _ = write!(out, "{arg}");
            rest = &after[digits + 1..];
            continue;
        }
        out.push('{');
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Literal string pertama sesudah tiap `t(` / `tf(` di teks sumber Rust (sudah di-unescape).
/// Literal raw (`r"…"`) dan argumen non-literal dilewati.
#[cfg(test)]
fn literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    for i in 0..bytes.len() {
        let len = if bytes[i..].starts_with(b"t(") {
            2
        } else if bytes[i..].starts_with(b"tf(") {
            3
        } else {
            continue;
        };
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue;
        }
        let mut j = i + len;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) == Some(&b'"') {
            // Literal yang gagal dibaca tetap dilaporkan supaya tidak lolos diam-diam.
            out.push(
                unescape_literal(&source[j + 1..])
                    .unwrap_or_else(|| format!("<literal tak terbaca di byte {j}>")),
            );
        }
    }
    out
}

/// Isi literal string Rust sampai tanda kutip penutup.
#[cfg(test)]
fn unescape_literal(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                'u' => {
                    chars.next()?; // '{'
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                'x' => {
                    let hex: String = chars.by_ref().take(2).collect();
                    out.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
                }
                '\n' => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Test helper: gagal bila ada literal `t("…")` / `tf("…", …)` di `source` yang tidak punya
/// terjemahan, atau bila `table` belum didaftarkan di [`TABLES`]. Kunci dicari di semua tabel
/// terdaftar, jadi teks yang sudah ada di tabel modul lain atau [`COMMON`] tidak perlu diulang.
#[cfg(test)]
pub(crate) fn assert_translated(source: &str, table: &[(&str, &str)]) {
    let map = english();
    for (src, en) in table {
        assert_eq!(
            map.get(src),
            Some(en),
            "tabel belum didaftarkan di i18n::TABLES (kunci {src:?})"
        );
    }
    let missing: Vec<String> = literals(source)
        .into_iter()
        .filter(|lit| !map.contains_key(lit.as_str()))
        .collect();
    assert!(missing.is_empty(), "belum ada terjemahan: {missing:#?}");
}

/// Test helper untuk teks yang tidak lewat literal `t("…")`, mis. label di array `const`.
#[cfg(test)]
pub(crate) fn assert_keys<'a>(keys: impl IntoIterator<Item = &'a str>) {
    let missing: Vec<&str> = keys
        .into_iter()
        .filter(|k| !english().contains_key(k))
        .collect();
    assert!(missing.is_empty(), "belum ada terjemahan: {missing:#?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(text: &str) -> Vec<String> {
        let mut found: Vec<String> = text
            .split('{')
            .skip(1)
            .filter_map(|part| {
                let (n, _) = part.split_once('}')?;
                (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.to_owned())
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    #[test]
    fn tidak_ada_kunci_ganda_antar_tabel() {
        let mut seen = std::collections::HashSet::new();
        let dupes: Vec<&str> = TABLES
            .iter()
            .flat_map(|t| t.iter())
            .filter(|(k, _)| !seen.insert(*k))
            .map(|(k, _)| *k)
            .collect();
        assert!(dupes.is_empty(), "kunci ganda: {dupes:?}");
    }

    #[test]
    fn placeholder_terjemahan_sama_dengan_sumber() {
        for (src, en) in TABLES.iter().flat_map(|t| t.iter()) {
            assert_eq!(placeholders(src), placeholders(en), "{src:?} → {en:?}");
        }
    }

    #[test]
    fn fill_mengganti_placeholder_berulang() {
        assert_eq!(fill("{0} + {0} = {1}", &[&2, &4]), "2 + 2 = 4");
        assert_eq!(fill("{1} sebelum {0}", &[&"a", &"b"]), "b sebelum a");
    }

    #[test]
    fn fill_membiarkan_argumen_hilang_dan_kurung_lain() {
        assert_eq!(fill("{0} dan {2}", &[&"x"]), "x dan {2}");
        assert_eq!(fill("{\"a\": {0}} {} {x", &[&1]), "{\"a\": 1} {} {x");
    }

    // Bahasa global tidak diubah di test supaya test lain yang memanggil `t` tetap
    // melihat teks Indonesia walau berjalan paralel.
    #[test]
    fn translate_memakai_tabel_hanya_untuk_english() {
        assert_eq!(translate(Lang::En, "Batal"), "Cancel");
        assert_eq!(
            translate(Lang::En, "tidak ada di tabel"),
            "tidak ada di tabel"
        );
        assert_eq!(translate(Lang::Id, "Batal"), "Batal");
        assert_eq!(lang(), Lang::Id);
    }

    #[test]
    fn lang_disimpan_sebagai_kode_kecil() {
        assert_eq!(serde_json::to_string(&Lang::En).unwrap(), "\"en\"");
        assert_eq!(serde_json::from_str::<Lang>("\"id\"").unwrap(), Lang::Id);
    }

    #[test]
    fn literals_membaca_t_dan_tf() {
        let source = r#"
            ui.label(t("Halo"));
            hint_text("bukan");
            let s = tf(
                "Ada {0} \"kutip\" \
                 dan sambungan",
                &[&n],
            );
            t(label);
            t("\u{2192} panah");
        "#;
        assert_eq!(
            literals(source),
            vec!["Halo", "Ada {0} \"kutip\" dan sambungan", "→ panah"]
        );
    }

    #[test]
    fn assert_translated_lolos_untuk_kunci_terdaftar() {
        assert_translated("t(\"Batal\"); tf(\"Simpan\", &[])", COMMON);
    }

    #[test]
    #[should_panic(expected = "belum ada terjemahan")]
    fn assert_translated_gagal_untuk_kunci_baru() {
        assert_translated("t(\"Teks yang belum diterjemahkan\")", COMMON);
    }

    #[test]
    #[should_panic(expected = "belum didaftarkan")]
    fn assert_translated_gagal_untuk_tabel_tak_terdaftar() {
        assert_translated("", &[("Teks lepas", "Loose text")]);
    }
}
