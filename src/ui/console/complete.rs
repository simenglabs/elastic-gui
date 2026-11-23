//! Konteks autocomplete Console dan daftar saran (logika murni, tanpa UI).

use super::parse::{Block, METHODS};
use crate::es::spec::{self, Spec};

#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Key(String),
    Index,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Context {
    Method,
    Path {
        method: String,
        done: Vec<String>,
    },
    Param {
        method: String,
        path: String,
    },
    ParamValue {
        method: String,
        path: String,
        param: String,
    },
    BodyKey {
        method: String,
        path: String,
        json_path: Vec<Seg>,
    },
    BodyValue {
        method: String,
        path: String,
        json_path: Vec<Seg>,
    },
}

/// Posisi cursor yang bisa dilengkapi.
#[derive(Clone, Debug, PartialEq)]
pub struct Located {
    pub ctx: Context,
    pub prefix: String,
    /// Awal token yang akan diganti (termasuk kutip pembuka jika `quoted`).
    pub start: usize,
    pub quoted: bool,
}

pub fn locate(text: &str, cursor: usize, blocks: &[Block]) -> Option<Located> {
    let line_start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let block = super::parse::block_at(blocks, cursor);
    let on_request_line = block.is_none_or(|b| b.start == line_start);
    if on_request_line {
        return locate_request_line(&text[line_start..cursor], line_start);
    }
    let block = block?;
    if cursor < block.body_start {
        return None;
    }
    locate_body(&text[block.body_start..cursor], block.body_start, block)
}

fn locate_request_line(before: &str, line_start: usize) -> Option<Located> {
    let Some(space) = before.find(' ') else {
        return Some(Located {
            ctx: Context::Method,
            prefix: before.to_owned(),
            start: line_start,
            quoted: false,
        });
    };
    let method = before[..space].to_ascii_uppercase();
    let rest_offset =
        space + 1 + (before[space + 1..].len() - before[space + 1..].trim_start().len());
    let rest = &before[rest_offset..];
    if let Some((path, query)) = rest.split_once('?') {
        let part_offset = query.rfind('&').map_or(0, |i| i + 1);
        let part = &query[part_offset..];
        let base = line_start + rest_offset + path.len() + 1 + part_offset;
        return Some(match part.split_once('=') {
            Some((param, value)) => Located {
                ctx: Context::ParamValue {
                    method,
                    path: path.to_owned(),
                    param: param.to_owned(),
                },
                prefix: value.to_owned(),
                start: base + param.len() + 1,
                quoted: false,
            },
            None => Located {
                ctx: Context::Param {
                    method,
                    path: path.to_owned(),
                },
                prefix: part.to_owned(),
                start: base,
                quoted: false,
            },
        });
    }
    let last_slash = rest.rfind('/').map_or(0, |i| i + 1);
    let done = spec::segments(&rest[..last_slash])
        .into_iter()
        .map(str::to_owned)
        .collect();
    Some(Located {
        ctx: Context::Path { method, done },
        prefix: rest[last_slash..].to_owned(),
        start: line_start + rest_offset + last_slash,
        quoted: false,
    })
}

struct Frame {
    object: bool,
    key: Option<String>,
    after_colon: bool,
}

fn locate_body(body: &str, base: usize, block: &Block) -> Option<Located> {
    let bytes = body.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    let mut open_string: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' if body[i..].starts_with("\"\"\"") => {
                let close = body[i + 3..].find("\"\"\"")?;
                i += 3 + close + 2;
            }
            b'"' => {
                let start = i;
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 1,
                        b'"' => {
                            closed = true;
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                if !closed {
                    open_string = Some(start);
                    break;
                }
                if let Some(top) = stack.last_mut()
                    && top.object
                    && !top.after_colon
                {
                    top.key = Some(body[start + 1..i].to_owned());
                }
            }
            b'{' | b'[' => stack.push(Frame {
                object: c == b'{',
                key: None,
                after_colon: false,
            }),
            b'}' | b']' => {
                stack.pop();
            }
            b':' => {
                if let Some(top) = stack.last_mut() {
                    top.after_colon = true;
                }
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.key = None;
                    top.after_colon = false;
                }
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let top = stack.last()?;
    let (prefix, start, quoted) = match open_string {
        Some(start) => (body[start + 1..].to_owned(), start, true),
        None => {
            let word_len = body
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'))
                .map(char::len_utf8)
                .sum::<usize>();
            let start = body.len() - word_len;
            (body[start..].to_owned(), start, false)
        }
    };
    let mut json_path = Vec::new();
    for frame in &stack {
        if !frame.object {
            json_path.push(Seg::Index);
        } else if frame.after_colon {
            json_path.push(Seg::Key(frame.key.clone()?));
        }
    }
    let method = block.method.clone();
    let path = block.path.clone();
    let ctx = if top.object && !top.after_colon {
        Context::BodyKey {
            method,
            path,
            json_path,
        }
    } else {
        Context::BodyValue {
            method,
            path,
            json_path,
        }
    };
    Some(Located {
        ctx,
        prefix,
        start: base + start,
        quoted,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub detail: String,
    pub insert: String,
    /// Jumlah karakter dari akhir `insert` ke posisi cursor setelah disisipkan.
    pub cursor_back: usize,
}

impl Suggestion {
    fn plain(label: &str, detail: &str, insert: String) -> Self {
        Self {
            label: label.to_owned(),
            detail: detail.to_owned(),
            insert,
            cursor_back: 0,
        }
    }
}

/// Data live dari cluster untuk saran.
pub struct Live<'a> {
    pub indices: &'a [(String, &'static str)],
    /// `None` = field belum termuat.
    pub fields: Option<&'a [(String, String)]>,
    /// CON-23: nama untuk parameter path (nilai, sumber).
    pub named: &'a [(String, String)],
    /// CON-29: nilai term untuk field di posisi nilai (`None` = belum dimuat).
    pub terms: Option<&'a [String]>,
}

pub struct Outcome {
    pub items: Vec<Suggestion>,
    /// Pola index yang field-nya perlu dimuat.
    pub wants_fields: Option<String>,
    /// CON-29: (pola index, field) yang nilainya perlu dimuat lewat `_terms_enum`.
    pub wants_terms: Option<(String, String)>,
}

/// Pola index dari path request (segmen pertama yang bukan endpoint `_x`).
pub fn index_of_path(path: &str) -> Option<String> {
    let first = spec::segments(path.split('?').next()?).into_iter().next()?;
    (!first.starts_with('_')).then(|| first.to_owned())
}

pub fn suggest(spec: &Spec, located: &Located, live: &Live) -> Outcome {
    let mut wants_fields = None;
    let mut wants_terms = None;
    let mut items = Vec::new();
    match &located.ctx {
        Context::Method => {
            for m in METHODS {
                items.push(Suggestion::plain(m, "method", format!("{m} ")));
            }
        }
        Context::Path { method, done } => {
            let done: Vec<&str> = done.iter().map(String::as_str).collect();
            let (segments, wants_index) = spec.next_segments(method, &done);
            for (segment, endpoint) in segments {
                items.push(Suggestion::plain(&segment, &endpoint, segment.clone()));
            }
            for (name, source) in live.named {
                items.push(Suggestion::plain(name, source, name.clone()));
            }
            if wants_index {
                for (name, kind) in live.indices {
                    if !name.starts_with('.') || located.prefix.starts_with('.') {
                        items.push(Suggestion::plain(name, kind, name.clone()));
                    }
                }
            }
        }
        Context::Param { method, path } => {
            if let Some(endpoint) = spec.find(method, path) {
                for (name, t) in &endpoint.query {
                    items.push(Suggestion::plain(
                        name,
                        &spec.type_label(t),
                        format!("{name}="),
                    ));
                }
            }
            for common in ["pretty", "human", "error_trace", "filter_path"] {
                if !items.iter().any(|s| s.label == common) {
                    items.push(Suggestion::plain(
                        common,
                        match crate::i18n::lang() {
                            crate::i18n::Lang::Id => "umum",
                            crate::i18n::Lang::En => "common",
                        },
                        format!("{common}="),
                    ));
                }
            }
        }
        Context::ParamValue {
            method,
            path,
            param,
        } => {
            if let Some(t) = spec.find(method, path).and_then(|e| e.query.get(param)) {
                let (values, _) = spec.values(t);
                for v in values {
                    items.push(Suggestion::plain(&v, param, v.clone()));
                }
            }
        }
        Context::BodyKey {
            method,
            path,
            json_path,
        }
        | Context::BodyValue {
            method,
            path,
            json_path,
        } => {
            let is_key = matches!(located.ctx, Context::BodyKey { .. });
            let Some(node) = body_node(spec, method, path, json_path) else {
                return Outcome {
                    items,
                    wants_fields,
                    wants_terms,
                };
            };
            let index = index_of_path(path);
            let mut add_fields = |items: &mut Vec<Suggestion>, as_key: bool| match live.fields {
                Some(fields) => {
                    for (name, t) in fields {
                        let insert = if as_key {
                            format!("\"{name}\": ")
                        } else {
                            format!("\"{name}\"")
                        };
                        items.push(Suggestion::plain(name, t, insert));
                    }
                }
                None => wants_fields = index.clone(),
            };
            if is_key {
                let (keys, needs) = spec.keys(&node);
                for (name, t) in keys {
                    let snippet = spec.snippet(&t);
                    let insert = format!("\"{name}\": {snippet}");
                    items.push(Suggestion {
                        label: name,
                        detail: spec.type_label(&t),
                        insert,
                        cursor_back: snippet.len() / 2,
                    });
                }
                if needs.fields {
                    add_fields(&mut items, true);
                }
            } else {
                let (values, needs) = spec.values(&node);
                let is_string = |v: &str| v != "true" && v != "false" && v.parse::<f64>().is_err();
                for v in values {
                    let insert = if is_string(&v) {
                        format!("\"{v}\"")
                    } else {
                        v.clone()
                    };
                    items.push(Suggestion::plain(
                        &v,
                        match crate::i18n::lang() {
                            crate::i18n::Lang::Id => "nilai",
                            crate::i18n::Lang::En => "value",
                        },
                        insert,
                    ));
                }
                if needs.fields {
                    add_fields(&mut items, false);
                }
                if needs.indices {
                    for (name, kind) in live.indices {
                        items.push(Suggestion::plain(name, kind, format!("\"{name}\"")));
                    }
                }
                // CON-29: nilai langsung field, mis. `"term": {"status": "|"}`.
                if let (Some(Seg::Key(field)), Some(index)) = (json_path.last(), &index)
                    && body_node(spec, method, path, &json_path[..json_path.len() - 1])
                        .is_some_and(|parent| spec.keys(&parent).1.fields)
                {
                    match live.terms {
                        Some(terms) => {
                            for t in terms {
                                items.push(Suggestion::plain(
                                    t,
                                    "term",
                                    format!("\"{}\"", t.replace('"', "\\\"")),
                                ));
                            }
                        }
                        None => wants_terms = Some((index.clone(), field.clone())),
                    }
                }
            }
        }
    }
    Outcome {
        items: rank(items, &located.prefix),
        wants_fields,
        wants_terms,
    }
}

fn body_node(
    spec: &Spec,
    method: &str,
    path: &str,
    json_path: &[Seg],
) -> Option<serde_json::Value> {
    let mut node = spec.find(method, path)?.body.clone()?;
    for seg in json_path {
        node = match seg {
            Seg::Key(k) => spec.child(&node, Some(k))?,
            Seg::Index => spec.child(&node, None)?,
        };
    }
    Some(node)
}

fn score(prefix: &str, label: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(1);
    }
    let p = prefix.to_lowercase();
    let l = label.to_lowercase();
    if l.starts_with(&p) {
        return Some(0);
    }
    if l.contains(&p) {
        return Some(1);
    }
    let mut chars = l.chars();
    p.chars().all(|c| chars.any(|x| x == c)).then_some(2)
}

const MAX_ITEMS: usize = 60;

fn rank(items: Vec<Suggestion>, prefix: &str) -> Vec<Suggestion> {
    let mut scored: Vec<(u8, Suggestion)> = items
        .into_iter()
        .filter(|s| s.label != prefix)
        .filter_map(|s| score(prefix, &s.label).map(|sc| (sc, s)))
        .collect();
    scored.sort_by(|(a, x), (b, y)| {
        a.cmp(b)
            .then(x.label.len().cmp(&y.label.len()))
            .then(x.label.cmp(&y.label))
    });
    scored.dedup_by(|(_, a), (_, b)| a.label == b.label && a.insert == b.insert);
    scored.truncate(MAX_ITEMS);
    scored.into_iter().map(|(_, s)| s).collect()
}

/// Terapkan saran: ganti token `[start, cursor)` (plus kutip penutup otomatis) dengan `insert`.
/// Mengembalikan teks baru dan posisi cursor (byte).
pub fn apply(text: &str, located: &Located, cursor: usize, s: &Suggestion) -> (String, usize) {
    let mut end = cursor;
    if located.quoted && text[end..].starts_with('"') {
        end += 1;
    }
    // Nilai body tanpa kutip yang ternyata diapit kutip pembuka sebelumnya.
    let mut start = located.start;
    if !located.quoted && s.insert.starts_with('"') && text[..start].ends_with('"') {
        start -= 1;
        if text[end..].starts_with('"') {
            end += 1;
        }
    }
    let mut out = String::with_capacity(text.len() + s.insert.len());
    out.push_str(&text[..start]);
    out.push_str(&s.insert);
    out.push_str(&text[end..]);
    let new_cursor = start + s.insert.len() - s.cursor_back;
    (out, new_cursor)
}

#[cfg(test)]
mod tests {
    use super::super::parse::blocks;
    use super::*;

    fn at(text_with_cursor: &str) -> (String, usize, Option<Located>) {
        let cursor = text_with_cursor.find('|').unwrap();
        let text = text_with_cursor.replace('|', "");
        let located = locate(&text, cursor, &blocks(&text));
        (text, cursor, located)
    }

    #[test]
    fn locates_request_line_contexts() {
        let (_, _, l) = at("GE|");
        assert_eq!(l.unwrap().ctx, Context::Method);
        let (_, _, l) = at("GET _cluster/he|");
        let l = l.unwrap();
        assert_eq!(
            l.ctx,
            Context::Path {
                method: "GET".into(),
                done: vec!["_cluster".into()]
            }
        );
        assert_eq!((l.prefix.as_str(), l.start), ("he", 13));
        let (_, _, l) = at("GET _cluster/health?wait_for_status=ye|");
        let l = l.unwrap();
        assert!(
            matches!(l.ctx, Context::ParamValue { ref param, .. } if param == "wait_for_status")
        );
        assert_eq!(l.prefix, "ye");
    }

    #[test]
    fn locates_body_contexts() {
        let (_, _, l) =
            at("GET logs/_search\n{\n  \"query\": {\n    \"bool\": {\n      \"must\": [ { \"ma|");
        let l = l.unwrap();
        assert_eq!(
            l.ctx,
            Context::BodyKey {
                method: "GET".into(),
                path: "logs/_search".into(),
                json_path: vec![
                    Seg::Key("query".into()),
                    Seg::Key("bool".into()),
                    Seg::Key("must".into()),
                    Seg::Index
                ]
            }
        );
        assert_eq!((l.prefix.as_str(), l.quoted), ("ma", true));
        let (_, _, l) = at("GET x/_search\n{\"size\": 1, \"sort\": [{\"a\": {\"order\": \"|\"}}]}");
        assert!(matches!(l.unwrap().ctx, Context::BodyValue { .. }));
    }

    #[test]
    fn suggests_and_applies_body_keys() {
        let spec = crate::es::spec::for_major(9);
        let (text, cursor, l) = at("GET logs/_search\n{\n  \"qu|\"\n}");
        let l = l.unwrap();
        let live = Live {
            indices: &[],
            fields: None,
            named: &[],
            terms: None,
        };
        let out = suggest(spec, &l, &live);
        let first = &out.items[0];
        assert_eq!(first.label, "query");
        let (new_text, new_cursor) = apply(&text, &l, cursor, first);
        assert_eq!(new_text, "GET logs/_search\n{\n  \"query\": {}\n}");
        assert_eq!(&new_text[new_cursor..new_cursor + 1], "}");
    }

    #[test]
    fn field_positions_request_fields() {
        let spec = crate::es::spec::for_major(9);
        let (_, _, l) = at("GET logs/_search\n{\"query\": {\"match\": {\"|");
        let l = l.unwrap();
        let none = Live {
            indices: &[],
            fields: None,
            named: &[],
            terms: None,
        };
        assert_eq!(
            suggest(spec, &l, &none).wants_fields.as_deref(),
            Some("logs")
        );
        let fields = [("message".to_string(), "text".to_string())];
        let live = Live {
            indices: &[],
            fields: Some(&fields),
            named: &[],
            terms: None,
        };
        assert_eq!(suggest(spec, &l, &live).items[0].label, "message");
    }

    #[test]
    fn suggests_index_names_in_path() {
        let spec = crate::es::spec::for_major(9);
        let (_, _, l) = at("GET onl|");
        let indices = [("online-news".to_string(), "index")];
        let live = Live {
            indices: &indices,
            fields: None,
            named: &[],
            terms: None,
        };
        let out = suggest(spec, &l.unwrap(), &live);
        assert_eq!(out.items[0].label, "online-news");
    }

    #[test]
    fn term_values_request_terms_enum() {
        let spec = crate::es::spec::for_major(9);
        let (_, _, l) = at("GET logs/_search\n{\"query\": {\"term\": {\"status\": \"ac|");
        let l = l.unwrap();
        let none = Live {
            indices: &[],
            fields: None,
            named: &[],
            terms: None,
        };
        let out = suggest(spec, &l, &none);
        assert_eq!(
            out.wants_terms,
            Some(("logs".to_string(), "status".to_string()))
        );
        let terms = ["active".to_string(), "archived".to_string()];
        let live = Live {
            indices: &[],
            fields: None,
            named: &[],
            terms: Some(&terms),
        };
        assert_eq!(suggest(spec, &l, &live).items[0].label, "active");
    }
}
