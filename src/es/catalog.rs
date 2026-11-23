//! Cache nama index/alias/data stream dan daftar field per pola index.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::Conn;
use crate::task::{Pending, poll};

const TTL: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Index,
    Alias,
    DataStream,
    /// DIS-07: cluster remote untuk cross-cluster search (`alias:*`).
    Remote,
}

impl TargetKind {
    pub fn label(self) -> &'static str {
        match self {
            TargetKind::Index => "index",
            TargetKind::Alias => "alias",
            TargetKind::DataStream => "data stream",
            TargetKind::Remote => crate::i18n::t("cluster remote"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub kind: TargetKind,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    /// Tipe ES. Lebih dari satu berarti konflik antar index.
    pub types: Vec<String>,
    pub aggregatable: bool,
}

impl Field {
    pub fn es_type(&self) -> &str {
        self.types.first().map(String::as_str).unwrap_or("?")
    }

    pub fn is_date(&self) -> bool {
        self.types.iter().all(|t| t == "date" || t == "date_nanos")
    }

    pub fn is_numeric(&self) -> bool {
        self.types.iter().all(|t| {
            matches!(
                t.as_str(),
                "long"
                    | "integer"
                    | "short"
                    | "byte"
                    | "double"
                    | "float"
                    | "half_float"
                    | "scaled_float"
                    | "unsigned_long"
            )
        })
    }

    pub fn sortable(&self) -> bool {
        self.aggregatable
    }
}

type FieldsResult = Result<Vec<Field>, String>;

/// CON-23: (kunci path, endpoint daftar, cara membaca nama).
const NAMED: &[(&str, &str, &str)] = &[
    ("_ilm/policy", "/_ilm/policy", "keys"),
    ("_ingest/pipeline", "/_ingest/pipeline", "keys"),
    (
        "_index_template",
        "/_index_template",
        "index_templates.name",
    ),
    (
        "_component_template",
        "/_component_template",
        "component_templates.name",
    ),
    ("_snapshot", "/_snapshot", "keys"),
    ("_slm/policy", "/_slm/policy", "keys"),
    ("_security/user", "/_security/user", "keys"),
    ("_security/role", "/_security/role", "keys"),
    ("_nodes", "/_nodes?filter_path=nodes.*.name", "nodes.*name"),
    ("_transform", "/_transform?size=1000", "transforms.id"),
];

/// Ambil daftar nama dari response sesuai aturan di `NAMED`.
fn extract_names(value: &Value, rule: &str) -> Vec<String> {
    let mut out: Vec<String> = match rule.split_once('.') {
        None => value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        Some((field, "*name")) => value[field]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, n)| n["name"].as_str().map(str::to_owned))
            .collect(),
        Some((field, key)) => value[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item[key].as_str().map(str::to_owned))
            .collect(),
    };
    out.sort();
    out
}

#[derive(Default)]
pub struct Catalog {
    targets: Vec<Target>,
    targets_at: Option<Instant>,
    targets_pending: Option<Pending<Result<Vec<Target>, String>>>,
    fields: HashMap<String, (Instant, FieldsResult)>,
    fields_pending: HashMap<String, Pending<FieldsResult>>,
    names: HashMap<String, (Instant, Vec<String>)>,
    names_pending: HashMap<String, Pending<Vec<String>>>,
}

impl Catalog {
    pub fn poll(&mut self) {
        if let Some(result) = poll(&mut self.targets_pending) {
            self.targets_at = Some(Instant::now());
            match result {
                Ok(targets) => self.targets = targets,
                Err(err) => eprintln!("gagal memuat daftar index: {err}"),
            }
        }
        let keys: Vec<String> = self.names_pending.keys().cloned().collect();
        for key in keys {
            let mut slot = self.names_pending.remove(&key);
            match poll(&mut slot) {
                Some(names) => {
                    self.names.insert(key, (Instant::now(), names));
                }
                None => {
                    if let Some(pending) = slot {
                        self.names_pending.insert(key, pending);
                    }
                }
            }
        }
        let done: Vec<String> = self.fields_pending.keys().cloned().collect();
        for pattern in done {
            let mut slot = self.fields_pending.remove(&pattern);
            match poll(&mut slot) {
                Some(result) => {
                    self.fields.insert(pattern, (Instant::now(), result));
                }
                None => {
                    if let Some(pending) = slot {
                        self.fields_pending.insert(pattern, pending);
                    }
                }
            }
        }
    }

    pub fn invalidate(&mut self) {
        self.targets_at = None;
        self.fields.clear();
        self.names.clear();
    }

    /// Nama untuk parameter path (policy, pipeline, template, repository, node, ...).
    pub fn names(
        &mut self,
        ctx: &eframe::egui::Context,
        conn: &Arc<Conn>,
        key: &str,
    ) -> Option<&[String]> {
        let &(_, path, rule) = NAMED.iter().find(|(k, ..)| *k == key)?;
        let stale = self.names.get(key).is_none_or(|(at, _)| at.elapsed() > TTL);
        if stale && !self.names_pending.contains_key(key) {
            let conn = conn.clone();
            self.names_pending.insert(
                key.to_owned(),
                Pending::spawn(ctx, move || {
                    conn.get(path)
                        .map(|v| extract_names(&v, rule))
                        .unwrap_or_default()
                }),
            );
        }
        self.names.get(key).map(|(_, n)| n.as_slice())
    }

    /// Daftar index/alias/data stream; memicu refresh latar jika kedaluwarsa.
    pub fn targets(&mut self, ctx: &eframe::egui::Context, conn: &Arc<Conn>) -> &[Target] {
        let stale = self.targets_at.is_none_or(|t| t.elapsed() > TTL);
        if stale && self.targets_pending.is_none() {
            let conn = conn.clone();
            self.targets_pending = Some(Pending::spawn(ctx, move || load_targets(&conn)));
        }
        &self.targets
    }

    pub fn loading_targets(&self) -> bool {
        self.targets_pending.is_some()
    }

    /// Field untuk pola index. `None` selama belum termuat.
    pub fn fields(
        &mut self,
        ctx: &eframe::egui::Context,
        conn: &Arc<Conn>,
        pattern: &str,
    ) -> Option<&FieldsResult> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        let stale = self
            .fields
            .get(pattern)
            .is_none_or(|(at, _)| at.elapsed() > TTL);
        if stale && !self.fields_pending.contains_key(pattern) {
            let conn = conn.clone();
            let p = pattern.to_owned();
            self.fields_pending.insert(
                pattern.to_owned(),
                Pending::spawn(ctx, move || load_fields(&conn, &p)),
            );
        }
        self.fields.get(pattern).map(|(_, f)| f)
    }

    pub fn refresh_fields(&mut self, pattern: &str) {
        self.fields.remove(pattern.trim());
    }
}

fn load_targets(conn: &Conn) -> Result<Vec<Target>, String> {
    let value = conn.get("/_resolve/index/*?expand_wildcards=open")?;
    let mut out = Vec::new();
    for (key, kind) in [
        ("indices", TargetKind::Index),
        ("aliases", TargetKind::Alias),
        ("data_streams", TargetKind::DataStream),
    ] {
        for item in value[key].as_array().into_iter().flatten() {
            if let Some(name) = item["name"].as_str() {
                out.push(Target {
                    name: name.to_owned(),
                    kind,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    if let Ok(remotes) = conn.get("/_remote/info") {
        for alias in remotes.as_object().into_iter().flatten().map(|(k, _)| k) {
            out.push(Target {
                name: format!("{alias}:*"),
                kind: TargetKind::Remote,
            });
        }
    }
    Ok(out)
}

fn load_fields(conn: &Conn, pattern: &str) -> FieldsResult {
    let path = format!(
        "/{}/_field_caps?fields=*&ignore_unavailable=true&allow_no_indices=true",
        pattern.trim_matches('/')
    );
    Ok(parse_field_caps(&conn.get(&path)?))
}

pub fn parse_field_caps(value: &Value) -> Vec<Field> {
    let mut out: Vec<Field> = value["fields"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| !name.starts_with('_') || *name == "_id" || *name == "_index")
        .filter_map(|(name, caps)| {
            let caps = caps.as_object()?;
            let types: Vec<String> = caps.keys().filter(|t| *t != "unmapped").cloned().collect();
            if types.is_empty() || types.iter().all(|t| t == "object" || t == "nested") {
                return None;
            }
            let aggregatable = caps
                .values()
                .all(|c| c["aggregatable"].as_bool().unwrap_or(false));
            Some(Field {
                name: name.clone(),
                types,
                aggregatable,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[("cluster remote", "remote cluster")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("catalog.rs"), EN);
    }

    #[test]
    fn parses_field_caps() {
        let value = serde_json::json!({"fields": {
            "@timestamp": {"date": {"type": "date", "aggregatable": true}},
            "message": {"text": {"type": "text", "aggregatable": false}},
            "host": {"object": {"type": "object"}},
            "_source": {"_source": {"type": "_source"}},
            "code": {"long": {"aggregatable": true}, "keyword": {"aggregatable": true}}
        }});
        let fields = parse_field_caps(&value);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["@timestamp", "code", "message"]);
        assert!(fields[0].is_date());
        assert_eq!(fields[1].types.len(), 2, "konflik tipe");
        assert!(!fields[2].aggregatable);
    }

    #[test]
    #[ignore = "butuh cluster"]
    fn live_targets_and_fields() {
        let conn = crate::es::client::live_conn().expect("env");
        let targets = load_targets(&conn).unwrap();
        assert!(!targets.is_empty());
        let index = &targets
            .iter()
            .find(|t| t.kind == TargetKind::Index)
            .unwrap()
            .name;
        let fields = load_fields(&conn, index).unwrap();
        println!("{} target, {index}: {} field", targets.len(), fields.len());
        assert!(!fields.is_empty());
    }

    #[test]
    fn extracts_names() {
        let keys = serde_json::json!({"b": {}, "a": {}});
        assert_eq!(extract_names(&keys, "keys"), ["a", "b"]);
        let arr = serde_json::json!({"index_templates": [{"name": "t1"}]});
        assert_eq!(extract_names(&arr, "index_templates.name"), ["t1"]);
        let nodes = serde_json::json!({"nodes": {"x1": {"name": "es01"}}});
        assert_eq!(extract_names(&nodes, "nodes.*name"), ["es01"]);
    }
}
