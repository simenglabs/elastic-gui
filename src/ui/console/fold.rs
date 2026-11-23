//! CON-09: folding JSON di editor Console (logika rentang + layout visual).

use std::collections::BTreeSet;

use eframe::egui::{
    Color32,
    text::{ByteIndex, LayoutJob, LayoutSection},
};

use super::parse;

/// Region yang bisa dilipat: `(awal baris pembuka, akhir baris penutup)` dalam offset byte.
/// Akhir baris penutup tidak termasuk `\n`.
pub fn fold_ranges(text: &str) -> Vec<(usize, usize)> {
    // (awal baris pembuka, kurung, bisa dilipat)
    let mut stack: Vec<(usize, char, bool)> = Vec::new();
    let mut out = Vec::new();
    let mut in_triple = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let line_end = start + content.len();
        if !in_triple {
            let t = content.trim_start();
            if t.starts_with('#') || t.starts_with("//") {
                continue;
            }
            // Request baru: kurung terbuka dari body sebelumnya tidak boleh menyeberang.
            if parse::request_line(content).is_some() {
                stack.clear();
                continue;
            }
        }
        // Indeks stack untuk pembuka yang menjadi karakter non-spasi terakhir baris.
        let mut last_opener: Option<usize> = None;
        let mut in_string = false;
        let mut escaped = false;
        let mut chars = content.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if in_triple {
                if content[i..].starts_with("\"\"\"") {
                    in_triple = false;
                    chars.nth(1);
                }
                last_opener = None;
                continue;
            }
            if in_string {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                last_opener = None;
                continue;
            }
            match c {
                '"' if content[i..].starts_with("\"\"\"") => {
                    in_triple = true;
                    chars.nth(1);
                }
                '"' => in_string = true,
                '{' | '[' => {
                    stack.push((start, c, false));
                    last_opener = Some(stack.len() - 1);
                    continue;
                }
                '}' | ']' => {
                    let open = if c == '}' { '{' } else { '[' };
                    // Kurung penutup tanpa pasangan diabaikan.
                    if stack.last().is_some_and(|&(_, k, _)| k == open)
                        && let Some((open_start, _, foldable)) = stack.pop()
                        && foldable
                        && open_start < start
                    {
                        out.push((open_start, line_end));
                    }
                }
                _ if c.is_whitespace() => continue,
                _ => {}
            }
            last_opener = None;
        }
        if let Some(i) = last_opener
            && let Some(entry) = stack.get_mut(i)
        {
            entry.2 = true;
        }
    }
    out.sort_unstable();
    out
}

/// Rentang byte yang disembunyikan untuk lipatan aktif: dari awal baris setelah
/// pembuka sampai akhir baris penutup.
pub fn hidden_ranges(
    text: &str,
    ranges: &[(usize, usize)],
    folds: &BTreeSet<usize>,
) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in ranges {
        if !folds.contains(&start) {
            continue;
        }
        let Some(nl) = text[start..].find('\n') else {
            continue;
        };
        let from = start + nl + 1;
        if from >= end {
            continue;
        }
        // Lipatan bersarang di dalam lipatan lain sudah tertutup.
        match out.last_mut() {
            Some(last) if from <= last.1 => last.1 = last.1.max(end),
            _ => out.push((from, end)),
        }
    }
    out
}

/// Buang lipatan yang pembukanya tidak lagi valid setelah teks berubah.
pub fn prune(folds: &mut BTreeSet<usize>, ranges: &[(usize, usize)]) {
    folds.retain(|s| ranges.iter().any(|&(start, _)| start == *s));
}

/// Sembunyikan rentang di layout: font kecil, warna transparan, tinggi baris nol.
/// Teks job tidak diubah supaya offset cursor/galley tetap cocok dengan buffer.
pub fn apply_hidden(job: &mut LayoutJob, hidden: &[(usize, usize)]) {
    if hidden.is_empty() {
        return;
    }
    let mut sections = Vec::with_capacity(job.sections.len() + hidden.len() * 2);
    for section in job.sections.drain(..) {
        let (s, e) = (section.byte_range.start.0, section.byte_range.end.0);
        let mut cur = s;
        for &(hs, he) in hidden {
            if he <= cur || hs >= e {
                continue;
            }
            if hs > cur {
                sections.push(slice(&section, cur, hs, false));
                cur = hs;
            }
            let until = he.min(e);
            sections.push(slice(&section, cur, until, true));
            cur = until;
        }
        if cur < e {
            sections.push(slice(&section, cur, e, false));
        }
    }
    job.sections = sections;
}

fn slice(section: &LayoutSection, start: usize, end: usize, hide: bool) -> LayoutSection {
    let mut out = section.clone();
    out.byte_range = ByteIndex(start)..ByteIndex(end);
    if start != section.byte_range.start.0 {
        out.leading_space = 0.0;
    }
    if hide {
        out.format.font_id.size = 1.0;
        out.format.line_height = Some(0.0);
        out.format.color = Color32::TRANSPARENT;
        out.format.background = Color32::TRANSPARENT;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{FontId, TextFormat};

    fn lines(text: &str, ranges: &[(usize, usize)]) -> Vec<(String, String)> {
        ranges
            .iter()
            .map(|&(s, e)| {
                let first = text[s..].lines().next().unwrap_or_default().to_owned();
                let last = text[..e].lines().last().unwrap_or_default().to_owned();
                (first, last)
            })
            .collect()
    }

    #[test]
    fn nested_objects_and_arrays() {
        let text =
            "GET /_search\n{\n  \"query\": {\n    \"terms\": [\n      1,\n      2\n    ]\n  }\n}\n";
        let ranges = fold_ranges(text);
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].0, text.find("{\n").unwrap());
        assert_eq!(&text[ranges[0].1 - 1..ranges[0].1], "}");
        assert_eq!(text.as_bytes()[ranges[0].1], b'\n');
        assert_eq!(
            lines(text, &ranges),
            vec![
                ("{".into(), "}".into()),
                ("  \"query\": {".into(), "  }".into()),
                ("    \"terms\": [".into(), "    ]".into()),
            ]
        );
    }

    #[test]
    fn single_line_and_not_last_char_are_not_foldable() {
        assert!(fold_ranges("GET /x\n{ \"a\": 1 }\n").is_empty());
        assert!(fold_ranges("GET /x\n{ \"a\": 1,\n\"b\": 2 }\n").is_empty());
    }

    #[test]
    fn brackets_inside_strings_are_ignored() {
        let text = "GET /x\n{\n  \"a\": \"{ [\",\n  \"b\": \"x\\\"}\"\n}\n";
        assert_eq!(
            lines(text, &fold_ranges(text)),
            vec![("{".into(), "}".into())]
        );
        // Pembuka di dalam string di akhir baris bukan pembuka.
        assert!(fold_ranges("GET /x\n\"{\n}\n").is_empty());
    }

    #[test]
    fn triple_quotes_and_comments_are_ignored() {
        let text = "POST /x/_doc\n{\n  \"script\": \"\"\"\n    if (x) {\n      y();\n  \"\"\",\n  # komentar {\n  // komentar [\n  \"a\": 1\n}\n";
        assert_eq!(
            lines(text, &fold_ranges(text)),
            vec![("{".into(), "}".into())]
        );
    }

    #[test]
    fn unbalanced_brackets() {
        // Pembuka tanpa penutup: tidak ada region, dan tidak menyeberang ke request berikutnya.
        let text = "POST /a\n{\n  \"x\": {\n\nGET /b\n{\n  \"y\": 1\n}\n";
        assert_eq!(
            lines(text, &fold_ranges(text)),
            vec![("{".into(), "}".into())]
        );
        assert_eq!(fold_ranges(text)[0].0, text.rfind("{\n").unwrap());
        // Penutup tanpa pasangan atau salah jenis diabaikan.
        let text = "GET /x\n}\n{\n  ]\n}\n";
        assert_eq!(
            lines(text, &fold_ranges(text)),
            vec![("{".into(), "}".into())]
        );
    }

    #[test]
    fn hidden_ranges_and_prune() {
        let text = "GET /x\n{\n  \"a\": {\n    \"b\": 1\n  }\n}\n";
        let ranges = fold_ranges(text);
        let outer = ranges[0].0;
        let inner = ranges[1].0;
        let only_inner = hidden_ranges(text, &ranges, &BTreeSet::from([inner]));
        assert_eq!(only_inner.len(), 1);
        assert_eq!(&text[only_inner[0].0..only_inner[0].1], "    \"b\": 1\n  }");
        let both = hidden_ranges(text, &ranges, &BTreeSet::from([outer, inner]));
        assert_eq!(both.len(), 1);
        assert_eq!(
            &text[both[0].0..both[0].1],
            "  \"a\": {\n    \"b\": 1\n  }\n}"
        );

        let mut folds = BTreeSet::from([outer, inner, 3]);
        prune(&mut folds, &fold_ranges("GET /x\n{\n  \"a\": 1\n}\n"));
        assert_eq!(folds, BTreeSet::from([outer]));
    }

    #[test]
    fn apply_hidden_keeps_text_and_covers_bytes() {
        let text = "{\n  \"a\": 1\n}\nGET /x\n";
        let mut job = LayoutJob::default();
        job.append(
            &text[..5],
            0.0,
            TextFormat::simple(FontId::monospace(12.0), Color32::RED),
        );
        job.append(
            &text[5..],
            0.0,
            TextFormat::simple(FontId::monospace(12.0), Color32::BLUE),
        );
        apply_hidden(&mut job, &[(2, 12)]);
        assert_eq!(job.text, text);
        let mut cur = 0;
        for s in &job.sections {
            assert_eq!(s.byte_range.start.0, cur);
            let hidden = (2..12).contains(&cur);
            assert_eq!(s.format.line_height == Some(0.0), hidden);
            assert_eq!(s.format.color == Color32::TRANSPARENT, hidden);
            cur = s.byte_range.end.0;
        }
        assert_eq!(cur, text.len());
        assert_eq!(job.sections.len(), 4);
    }

    #[test]
    fn hidden_rows_collapse_in_galley() {
        let text = "GET /x\n{\n  \"a\": {\n\n    \"b\": 1\n  }\n}\nGET /y\n";
        let ranges = fold_ranges(text);
        let hidden = hidden_ranges(text, &ranges, &BTreeSet::from([ranges[0].0]));
        let mut job = crate::widgets::highlight(text, crate::widgets::Lang::Console, true);
        apply_hidden(&mut job, &hidden);
        let ctx = eframe::egui::Context::default();
        let mut heights = Vec::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
            assert_eq!(galley.text(), text);
            heights = galley.rows.iter().map(|r| r.height()).collect();
        });
        output.textures_delta.clear();
        // GET /x, `{`, 5 baris tersembunyi, GET /y, baris kosong terakhir.
        assert_eq!(heights.len(), 9);
        assert!(heights[..2].iter().all(|&h| h > 5.0));
        assert!(heights[2..7].iter().all(|&h| h == 0.0), "{heights:?}");
        assert!(heights[7] > 5.0);
    }
}
