//! Parser teks Console: blok `METHOD /path` + body JSON/NDJSON.

use serde_json::Value;

use crate::i18n::{t, tf};

pub const METHODS: [&str; 6] = ["GET", "POST", "PUT", "DELETE", "HEAD", "PATCH"];

/// Offset byte di teks editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub method: String,
    pub path: String,
    pub start: usize,
    pub body_start: usize,
    pub end: usize,
}

impl Block {
    pub fn body<'a>(&self, text: &'a str) -> &'a str {
        &text[self.body_start.min(self.end)..self.end]
    }
}

pub fn request_line(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let (method, rest) = line.split_once(' ').unwrap_or((line, ""));
    let upper = method.to_ascii_uppercase();
    METHODS
        .contains(&upper.as_str())
        .then(|| (method, rest.trim()))
}

pub fn blocks(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut in_triple = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if !in_triple && let Some((method, path)) = request_line(content) {
            if let Some(last) = out.last_mut() {
                last.end = offset;
            }
            out.push(Block {
                method: method.to_ascii_uppercase(),
                path: path.to_owned(),
                start: offset,
                body_start: offset + line.len(),
                end: text.len(),
            });
        }
        if content.matches("\"\"\"").count() % 2 == 1 {
            in_triple = !in_triple;
        }
        offset += line.len();
    }
    out
}

pub fn block_at(blocks: &[Block], cursor: usize) -> Option<&Block> {
    blocks
        .iter()
        .rev()
        .find(|b| b.start <= cursor && cursor <= b.end)
}

/// `"""teks"""` -> string JSON ter-escape.
fn replace_triple_quotes(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("\"\"\"") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 3..];
        match after.find("\"\"\"") {
            Some(close) => {
                out.push_str(&serde_json::to_string(&after[..close]).unwrap_or_default());
                rest = &after[close + 3..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn strip_comments(body: &str) -> String {
    body.lines()
        .filter(|line| {
            let t = line.trim_start();
            !t.starts_with('#') && !t.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn values(body: &str) -> Result<Vec<Value>, String> {
    let cleaned = strip_comments(&replace_triple_quotes(body));
    serde_json::Deserializer::from_str(&cleaned)
        .into_iter::<Value>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            tf(
                "JSON tidak valid (baris {0}, kolom {1}): {2}",
                &[&e.line(), &e.column(), &e],
            )
        })
}

/// Body siap kirim. NDJSON: satu objek per baris dengan newline penutup.
pub fn prepare_body(body: &str, ndjson: bool) -> Result<Option<String>, String> {
    let values = values(body)?;
    if values.is_empty() {
        return Ok(None);
    }
    if ndjson {
        let mut out = String::new();
        for v in &values {
            out.push_str(&v.to_string());
            out.push('\n');
        }
        return Ok(Some(out));
    }
    match values.as_slice() {
        [single] => Ok(Some(single.to_string())),
        _ => Err(
            t("Body berisi lebih dari satu objek JSON. Pisahkan menjadi request terpisah.").into(),
        ),
    }
}

/// Format ulang body (Ctrl/Cmd+I). NDJSON tetap satu objek per baris.
pub fn format_body(body: &str, ndjson: bool) -> Result<String, String> {
    let values = values(body)?;
    let mut out = String::new();
    for v in &values {
        if ndjson {
            out.push_str(&v.to_string());
        } else {
            out.push_str(&serde_json::to_string_pretty(v).unwrap_or_default());
        }
        out.push('\n');
    }
    Ok(out)
}

/// Pecah argumen gaya shell (kutip tunggal/ganda, backslash, sambungan baris).
fn shell_words(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err(t("cURL: kutip tunggal tidak ditutup").into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(ch) = chars.next() {
                                cur.push(ch);
                            }
                        }
                        Some(ch) => cur.push(ch),
                        None => return Err(t("cURL: kutip ganda tidak ditutup").into()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(ch) => {
                    in_word = true;
                    cur.push(ch);
                }
            },
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

/// CON-62: perintah cURL -> teks request Console.
pub fn from_curl(input: &str) -> Result<String, String> {
    let words = shell_words(input.trim())?;
    if words.first().map(String::as_str) != Some("curl") {
        return Err(t("Teks harus diawali `curl`.").into());
    }
    let mut method = None;
    let mut url = None;
    let mut body = None;
    let mut it = words.iter().skip(1);
    while let Some(w) = it.next() {
        match w.as_str() {
            "-X" | "--request" => method = it.next().cloned(),
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                body = it.next().cloned()
            }
            "-H" | "--header" | "-u" | "--user" | "-o" | "--output" | "--cacert" | "-E"
            | "--cert" => {
                it.next();
            }
            w if w.starts_with("-X") && w.len() > 2 => method = Some(w[2..].to_owned()),
            w if w.starts_with("http://") || w.starts_with("https://") => url = Some(w.to_owned()),
            w if !w.starts_with('-') && url.is_none() => url = Some(w.to_owned()),
            _ => {}
        }
    }
    let url = url.ok_or(t("URL tidak ditemukan di perintah cURL."))?;
    let without_scheme = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
    let path = without_scheme
        .find('/')
        .map_or("/", |i| &without_scheme[i..]);
    let method = method
        .unwrap_or_else(|| {
            if body.is_some() {
                "POST".into()
            } else {
                "GET".into()
            }
        })
        .to_ascii_uppercase();
    let mut out = format!("{method} {path}\n");
    if let Some(body) = body.filter(|b| !b.trim().is_empty()) {
        let ndjson = crate::es::client::is_ndjson(path);
        out.push_str(&format_body(&body, ndjson).unwrap_or_else(|_| format!("{body}\n")));
    }
    Ok(out)
}

/// CON-63: snippet bawaan (nama, teks).
pub const SNIPPETS: &[(&str, &str)] = &[
    ("Cluster health", "GET /_cluster/health\n"),
    (
        "Index terbesar",
        "GET /_cat/indices?v&s=store.size:desc&h=index,health,docs.count,store.size\n",
    ),
    (
        "Node & disk",
        "GET /_cat/allocation?v\n\nGET /_cat/nodes?v&h=name,heap.percent,ram.percent,cpu,load_1m,disk.used_percent,node.role\n",
    ),
    (
        "Shard unassigned",
        "GET /_cat/shards?v&h=index,shard,prirep,state,unassigned.reason&s=state\n\nGET /_cluster/allocation/explain\n",
    ),
    ("Hot threads", "GET /_nodes/hot_threads\n"),
    (
        "Task berjalan",
        "GET /_tasks?detailed=true&actions=*search*,*reindex*,*byquery*\n",
    ),
    (
        "Pencarian dasar",
        "GET /INDEX/_search\n{\n  \"size\": 10,\n  \"query\": {\n    \"match\": {\n      \"FIELD\": \"nilai\"\n    }\n  }\n}\n",
    ),
    (
        "Agregasi per waktu",
        "GET /INDEX/_search\n{\n  \"size\": 0,\n  \"aggs\": {\n    \"per_jam\": {\n      \"date_histogram\": {\n        \"field\": \"@timestamp\",\n        \"fixed_interval\": \"1h\"\n      }\n    }\n  }\n}\n",
    ),
    (
        "Reindex (async)",
        "POST /_reindex?wait_for_completion=false\n{\n  \"source\": { \"index\": \"SUMBER\" },\n  \"dest\": { \"index\": \"TUJUAN\" }\n}\n",
    ),
    (
        "ES|QL",
        "POST /_query?format=txt\n{\n  \"query\": \"FROM INDEX | LIMIT 10\"\n}\n",
    ),
];

/// CON-07: ganti `${NAMA}` dengan nilai variabel. Variabel tidak dikenal = error.
pub fn substitute(text: &str, vars: &[(String, String)]) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or(t("Variabel `${` tidak ditutup dengan `}`."))?;
        let name = after[..end].trim();
        let value = vars
            .iter()
            .find(|(n, _)| n.trim() == name)
            .map(|(_, v)| v)
            .ok_or_else(|| {
                tf(
                    "Variabel ${{0}} belum didefinisikan (panel Variabel).",
                    &[&name],
                )
            })?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("parse.rs"), super::super::EN);
    }

    const TEXT: &str = "# komentar\nGET /_cluster/health\n\nPOST logs/_search\n{\n  \"query\": {\"match_all\": {}}\n}\n\nPOST _bulk\n{\"index\":{\"_index\":\"a\"}}\n{\"f\":1}\n";

    #[test]
    fn splits_blocks() {
        let b = blocks(TEXT);
        assert_eq!(b.len(), 3);
        assert_eq!(
            (b[0].method.as_str(), b[0].path.as_str()),
            ("GET", "/_cluster/health")
        );
        assert_eq!(b[1].path, "logs/_search");
        assert!(b[1].body(TEXT).contains("match_all"));
        let cursor = TEXT.find("match_all").unwrap();
        assert_eq!(block_at(&b, cursor).unwrap().path, "logs/_search");
    }

    #[test]
    fn triple_quotes_hide_request_lines() {
        let text = "POST x/_search\n{\"q\": \"\"\"\nGET inside\n\"\"\"}\n";
        let b = blocks(text);
        assert_eq!(b.len(), 1);
        let body = prepare_body(b[0].body(text), false).unwrap().unwrap();
        assert_eq!(body, r#"{"q":"\nGET inside\n"}"#);
    }

    #[test]
    fn prepares_ndjson_and_errors() {
        let b = blocks(TEXT);
        let bulk = prepare_body(b[2].body(TEXT), true).unwrap().unwrap();
        assert_eq!(bulk, "{\"index\":{\"_index\":\"a\"}}\n{\"f\":1}\n");
        assert_eq!(prepare_body("  \n", false).unwrap(), None);
        assert!(
            prepare_body("{\"a\":", false)
                .unwrap_err()
                .contains("baris")
        );
        assert!(prepare_body("{} {}", false).is_err());
        assert_eq!(
            prepare_body("{\n # c\n \"a\": 1}", false).unwrap().unwrap(),
            "{\"a\":1}"
        );
    }

    #[test]
    fn imports_curl() {
        let text = from_curl(
            "curl -X POST 'https://es:9200/logs/_search?pretty' -u elastic:x -H 'Content-Type: application/json' -d '{\"size\":1}'",
        )
        .unwrap();
        assert_eq!(text, "POST /logs/_search?pretty\n{\n  \"size\": 1\n}\n");
        assert_eq!(
            from_curl("curl localhost:9200/_cat/indices").unwrap(),
            "GET /_cat/indices\n"
        );
        assert_eq!(
            from_curl("curl --data '{}' \\\n  http://h/x/_count").unwrap(),
            "POST /x/_count\n{}\n"
        );
        assert!(from_curl("wget x").is_err());
        assert!(from_curl("curl 'x").is_err());
    }

    #[test]
    fn substitutes_variables() {
        let vars = vec![
            ("idx".to_string(), "logs-*".to_string()),
            ("n".to_string(), "5".to_string()),
        ];
        assert_eq!(
            substitute("GET /${idx}/_search?size=${ n }", &vars).unwrap(),
            "GET /logs-*/_search?size=5"
        );
        assert_eq!(
            substitute("tanpa variabel", &vars).unwrap(),
            "tanpa variabel"
        );
        assert!(substitute("${x}", &vars).unwrap_err().contains("${x}"));
        assert!(substitute("${idx", &vars).is_err());
    }
}
