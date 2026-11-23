//! Spesifikasi API Elasticsearch untuk autocomplete.
//!
//! File JSON dihasilkan oleh `tools/gen_spec.py` dari elastic/elasticsearch-specification
//! (Apache-2.0). Format `ref` dijelaskan di skrip tersebut.

use std::sync::OnceLock;

use serde_json::{Map, Value};

pub struct Endpoint {
    pub name: String,
    pub doc: String,
    /// (methods, segmen path). Segmen `{x}` adalah parameter.
    pub urls: Vec<(Vec<String>, Vec<String>)>,
    pub query: Map<String, Value>,
    pub body: Option<Value>,
}

pub struct Spec {
    pub endpoints: Vec<Endpoint>,
    types: Map<String, Value>,
}

/// Saran nilai/kunci yang butuh data live dari cluster.
#[derive(Debug, Default, PartialEq)]
pub struct Needs {
    pub fields: bool,
    pub indices: bool,
}

pub fn for_major(major: u32) -> &'static Spec {
    static ES7: OnceLock<Spec> = OnceLock::new();
    static ES8: OnceLock<Spec> = OnceLock::new();
    static ES9: OnceLock<Spec> = OnceLock::new();
    match major {
        0..=7 => ES7.get_or_init(|| Spec::parse(include_str!("spec/es7.json"))),
        8 => ES8.get_or_init(|| Spec::parse(include_str!("spec/es8.json"))),
        _ => ES9.get_or_init(|| Spec::parse(include_str!("spec/es9.json"))),
    }
}

pub fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

fn is_param(segment: &str) -> bool {
    segment.starts_with('{')
}

/// Nama index/id tidak diawali `_` (kecuali `_all`), jadi `_cluster` tidak dianggap parameter.
fn param_matches(segment: &str) -> bool {
    !segment.is_empty() && (!segment.starts_with('_') || segment == "_all")
}

/// Parameter path yang diisi nama index/alias/data stream.
pub fn is_index_param(segment: &str) -> bool {
    matches!(
        segment,
        "{index}" | "{target}" | "{alias}" | "{data_stream}" | "{new_index}"
    )
}

impl Spec {
    fn parse(text: &str) -> Spec {
        let root: Value = serde_json::from_str(text).expect("spec embedded valid");
        let endpoints = root["endpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| Endpoint {
                name: e["n"].as_str().unwrap_or_default().to_owned(),
                doc: e["doc"].as_str().unwrap_or_default().to_owned(),
                urls: e["u"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|u| {
                        let methods = u[0]
                            .as_str()
                            .unwrap_or_default()
                            .split(' ')
                            .map(str::to_owned)
                            .collect();
                        let segs = segments(u[1].as_str().unwrap_or_default())
                            .into_iter()
                            .map(str::to_owned)
                            .collect();
                        (methods, segs)
                    })
                    .collect(),
                query: e["q"].as_object().cloned().unwrap_or_default(),
                body: Some(e["b"].clone()).filter(|b| !b.is_null()),
            })
            .collect();
        let types = root["types"].as_object().cloned().unwrap_or_default();
        Spec { endpoints, types }
    }

    /// Endpoint yang cocok dengan method + path lengkap; segmen literal lebih banyak menang.
    /// Method yang valid untuk path ini (apa pun method yang diketik).
    pub fn methods_for(&self, path: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in ["GET", "POST", "PUT", "DELETE", "HEAD"] {
            if self.find(m, path).is_some() {
                out.push(m.to_owned());
            }
        }
        out
    }

    pub fn find(&self, method: &str, path: &str) -> Option<&Endpoint> {
        let segs = segments(path.split('?').next().unwrap_or_default());
        let method = method.to_ascii_uppercase();
        let mut best: Option<(&Endpoint, usize)> = None;
        for endpoint in &self.endpoints {
            for (methods, template) in &endpoint.urls {
                if template.len() != segs.len() || !methods.contains(&method) {
                    continue;
                }
                let mut literals = 0;
                let matched = template.iter().zip(&segs).all(|(t, s)| {
                    if is_param(t) {
                        param_matches(s)
                    } else {
                        literals += 1;
                        t == s
                    }
                });
                if matched && best.is_none_or(|(_, l)| literals > l) {
                    best = Some((endpoint, literals));
                }
            }
        }
        best.map(|(e, _)| e)
    }

    /// Kandidat segmen berikutnya untuk path yang sudah diketik (`done` = segmen lengkap).
    /// Mengembalikan (literal, butuh nama index?).
    pub fn next_segments(&self, method: &str, done: &[&str]) -> (Vec<(String, String)>, bool) {
        let method = method.to_ascii_uppercase();
        let mut out: Vec<(String, String)> = Vec::new();
        let mut wants_index = false;
        for endpoint in &self.endpoints {
            for (methods, template) in &endpoint.urls {
                if template.len() <= done.len() || !methods.contains(&method) {
                    continue;
                }
                let prefix_ok = template.iter().zip(done).all(|(t, s)| {
                    if is_param(t) {
                        param_matches(s)
                    } else {
                        t == s
                    }
                });
                if !prefix_ok {
                    continue;
                }
                let next = &template[done.len()];
                if is_index_param(next) {
                    wants_index = true;
                } else if !is_param(next) && !out.iter().any(|(s, _)| s == next) {
                    out.push((next.clone(), endpoint.name.clone()));
                }
            }
        }
        (out, wants_index)
    }

    /// CON-23: sumber nama untuk parameter path berikutnya, berupa segmen literal
    /// sebelum parameter (mis. `_ilm/policy` untuk `_ilm/policy/{policy}`).
    pub fn param_sources(&self, method: &str, done: &[&str]) -> Vec<String> {
        let method = method.to_ascii_uppercase();
        let mut out: Vec<String> = Vec::new();
        for endpoint in &self.endpoints {
            for (methods, template) in &endpoint.urls {
                if template.len() <= done.len() || !methods.contains(&method) {
                    continue;
                }
                let next = &template[done.len()];
                if !is_param(next) || is_index_param(next) {
                    continue;
                }
                let prefix_ok = template.iter().zip(done).all(|(t, s)| {
                    if is_param(t) {
                        param_matches(s)
                    } else {
                        t == s
                    }
                });
                let literals: Vec<&str> = template[..done.len()]
                    .iter()
                    .rev()
                    .take_while(|t| !is_param(t))
                    .map(String::as_str)
                    .collect();
                let key = literals.into_iter().rev().collect::<Vec<_>>().join("/");
                if prefix_ok && !key.is_empty() && !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        out
    }

    fn resolve<'a>(&'a self, r: &'a Value) -> &'a Value {
        match r.get("t").and_then(Value::as_str) {
            Some(name) => self.types.get(name).unwrap_or(&Value::Null),
            None => r,
        }
    }

    /// Turun satu langkah: kunci objek atau indeks array. Union dicoba per varian.
    pub fn child(&self, r: &Value, key: Option<&str>) -> Option<Value> {
        let r = self.resolve(r);
        if let Some(variants) = r.get("u").and_then(Value::as_array) {
            return variants.iter().find_map(|v| self.child(v, key));
        }
        match key {
            Some(key) => {
                if let Some(props) = r.get("p").and_then(Value::as_object) {
                    return props.get(key).cloned();
                }
                r.get("d").cloned()
            }
            None => r.get("a").cloned(),
        }
    }

    /// Kunci yang valid di posisi objek ini, plus kebutuhan data live.
    pub fn keys(&self, r: &Value) -> (Vec<(String, Value)>, Needs) {
        let mut out = Vec::new();
        let mut needs = Needs::default();
        self.collect_keys(r, &mut out, &mut needs, 0);
        (out, needs)
    }

    fn collect_keys(
        &self,
        r: &Value,
        out: &mut Vec<(String, Value)>,
        needs: &mut Needs,
        depth: u8,
    ) {
        if depth > 8 {
            return;
        }
        let r = self.resolve(r);
        if let Some(variants) = r.get("u").and_then(Value::as_array) {
            for v in variants {
                self.collect_keys(v, out, needs, depth + 1);
            }
        } else if let Some(props) = r.get("p").and_then(Value::as_object) {
            for (name, t) in props {
                if !out.iter().any(|(n, _)| n == name) {
                    out.push((name.clone(), t.clone()));
                }
            }
        } else if r.get("d").is_some() && r.get("k").and_then(Value::as_str) == Some("f") {
            needs.fields = true;
        }
    }

    /// Nilai yang valid untuk posisi nilai ini.
    pub fn values(&self, r: &Value) -> (Vec<String>, Needs) {
        let mut out = Vec::new();
        let mut needs = Needs::default();
        self.collect_values(r, &mut out, &mut needs, 0);
        (out, needs)
    }

    fn collect_values(&self, r: &Value, out: &mut Vec<String>, needs: &mut Needs, depth: u8) {
        if depth > 8 {
            return;
        }
        match r {
            Value::String(kind) => match kind.as_str() {
                "b" => out.extend(["true".into(), "false".into()]),
                "f" => needs.fields = true,
                "i" => needs.indices = true,
                _ => {}
            },
            Value::Object(o) => {
                if let Some(values) = o.get("e").and_then(Value::as_array) {
                    for v in values {
                        let text = match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        if !out.contains(&text) {
                            out.push(text);
                        }
                    }
                } else if let Some(variants) = o.get("u").and_then(Value::as_array) {
                    for v in variants {
                        self.collect_values(v, out, needs, depth + 1);
                    }
                } else if let Some(inner) = o.get("a") {
                    self.collect_values(inner, out, needs, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Template nilai setelah kunci diterima: `{}`, `[]`, `""`, atau kosong.
    pub fn snippet(&self, r: &Value) -> &'static str {
        let r = self.resolve(r);
        if let Some(variants) = r.get("u").and_then(Value::as_array) {
            // Kibana-style: properti seperti `must` diisi array.
            let kinds: Vec<&str> = variants.iter().map(|v| self.snippet(v)).collect();
            for preferred in ["[]", "{}", "\"\""] {
                if kinds.contains(&preferred) {
                    return preferred;
                }
            }
            return "";
        }
        if r.get("a").is_some() {
            "[]"
        } else if r.get("p").is_some() || r.get("d").is_some() {
            "{}"
        } else if matches!(r.as_str(), Some("s" | "f" | "i")) || r.get("e").is_some() {
            "\"\""
        } else {
            ""
        }
    }

    /// Ringkasan tipe untuk ditampilkan di popup autocomplete.
    pub fn type_label(&self, r: &Value) -> String {
        match r {
            Value::String(s) => match s.as_str() {
                "s" => "string",
                "n" => "number",
                "b" => "boolean",
                "f" => "field",
                "i" => "index",
                _ => "any",
            }
            .into(),
            Value::Object(o) => {
                if let Some(name) = o.get("t").and_then(Value::as_str) {
                    name.split('@').next().unwrap_or(name).to_owned()
                } else if let Some(inner) = o.get("a") {
                    format!("{}[]", self.type_label(inner))
                } else if o.contains_key("d") {
                    "map".into()
                } else if o.contains_key("e") {
                    "enum".into()
                } else if let Some(variants) = o.get("u").and_then(Value::as_array) {
                    variants
                        .iter()
                        .map(|v| self.type_label(v))
                        .collect::<Vec<_>>()
                        .join(" | ")
                } else {
                    "object".into()
                }
            }
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_search_endpoint() {
        let spec = for_major(9);
        let e = spec.find("POST", "/logs-*/_search?size=1").expect("search");
        assert_eq!(e.name, "search");
        assert!(spec.find("GET", "/_cluster/health").is_some());
    }

    #[test]
    fn navigates_query_dsl() {
        let spec = for_major(9);
        let body = spec
            .find("GET", "/x/_search")
            .unwrap()
            .body
            .clone()
            .unwrap();
        let query = spec.child(&body, Some("query")).unwrap();
        let (keys, _) = spec.keys(&query);
        assert!(keys.iter().any(|(k, _)| k == "bool"));
        let must = spec
            .child(&spec.child(&query, Some("bool")).unwrap(), Some("must"))
            .unwrap();
        assert_eq!(spec.snippet(&must), "[]");
        let clause = spec.child(&must, None).unwrap();
        let matcher = spec.child(&clause, Some("match")).unwrap();
        let (_, needs) = spec.keys(&matcher);
        assert!(needs.fields, "match dikunci nama field");
    }

    #[test]
    fn suggests_next_segment() {
        let spec = for_major(9);
        let (segs, wants_index) = spec.next_segments("GET", &["_cluster"]);
        assert!(segs.iter().any(|(s, _)| s == "health"));
        assert!(!wants_index);
        let (_, wants_index) = spec.next_segments("GET", &[]);
        assert!(wants_index);
    }

    #[test]
    fn finds_param_sources() {
        let spec = for_major(9);
        assert!(
            spec.param_sources("GET", &["_ilm", "policy"])
                .contains(&"_ilm/policy".to_string())
        );
        assert!(
            spec.param_sources("GET", &["_snapshot"])
                .contains(&"_snapshot".to_string())
        );
    }
}
