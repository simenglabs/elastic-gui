//! Query bar + filter + range waktu -> `bool` DSL, serta util waktu/interval.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum QueryMode {
    #[default]
    Kql,
    Lucene,
    Dsl,
}

impl QueryMode {
    pub const ALL: [QueryMode; 3] = [QueryMode::Kql, QueryMode::Lucene, QueryMode::Dsl];

    pub fn label(self) -> &'static str {
        match self {
            QueryMode::Kql => "KQL",
            QueryMode::Lucene => "Lucene",
            QueryMode::Dsl => "DSL",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    Is,
    OneOf,
    Exists,
    Between,
    /// DIS-18: query DSL mentah di `value`.
    Dsl,
}

impl FilterOp {
    pub const ALL: [FilterOp; 5] = [
        FilterOp::Is,
        FilterOp::OneOf,
        FilterOp::Exists,
        FilterOp::Between,
        FilterOp::Dsl,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Is => "is",
            FilterOp::OneOf => crate::i18n::t("salah satu dari"),
            FilterOp::Exists => "exists",
            FilterOp::Between => crate::i18n::t("di antara"),
            FilterOp::Dsl => "DSL",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
    /// `Is`: nilai; `OneOf`: dipisah koma; `Between`: batas bawah.
    pub value: String,
    /// `Between`: batas atas.
    pub value2: String,
    pub negate: bool,
    pub enabled: bool,
    /// DIS-18: tetap aktif saat ganti sumber atau memuat pencarian tersimpan.
    #[serde(default)]
    pub pinned: bool,
}

impl Filter {
    pub fn is(field: &str, value: &str, negate: bool) -> Self {
        Self {
            field: field.into(),
            op: FilterOp::Is,
            value: value.into(),
            value2: String::new(),
            negate,
            enabled: true,
            pinned: false,
        }
    }

    /// Ubah filter menjadi filter DSL dengan isi query saat ini (untuk diedit).
    pub fn make_dsl(&mut self) {
        if self.op != FilterOp::Dsl {
            self.value = serde_json::to_string_pretty(&self.to_dsl()).unwrap_or_default();
            self.value2.clear();
            self.op = FilterOp::Dsl;
        }
    }

    pub fn label(&self) -> String {
        let not = if self.negate { "NOT " } else { "" };
        match self.op {
            FilterOp::Is => format!("{not}{}: {}", self.field, self.value),
            FilterOp::OneOf => format!("{not}{} is one of {}", self.field, self.value),
            FilterOp::Exists => format!("{not}{} exists", self.field),
            FilterOp::Between => format!(
                "{not}{}: {} → {}",
                self.field,
                or_dash(&self.value),
                or_dash(&self.value2)
            ),
            FilterOp::Dsl => {
                let compact = serde_json::from_str::<Value>(&self.value)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| self.value.clone());
                let short: String = compact.chars().take(60).collect();
                format!("{not}DSL {short}")
            }
        }
    }

    pub fn to_dsl(&self) -> Value {
        let field = &self.field;
        match self.op {
            FilterOp::Is => json!({"match_phrase": {field: self.value}}),
            FilterOp::OneOf => {
                let should: Vec<Value> = self
                    .value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(|v| json!({"match_phrase": {field: v}}))
                    .collect();
                json!({"bool": {"should": should, "minimum_should_match": 1}})
            }
            FilterOp::Exists => json!({"exists": {"field": field}}),
            FilterOp::Between => {
                let mut range = serde_json::Map::new();
                if !self.value.trim().is_empty() {
                    range.insert("gte".into(), json!(self.value.trim()));
                }
                if !self.value2.trim().is_empty() {
                    range.insert("lte".into(), json!(self.value2.trim()));
                }
                json!({"range": {field: range}})
            }
            // DSL tidak valid sudah dicegah di editor; fallback aman = tidak mencocokkan apa pun.
            FilterOp::Dsl => serde_json::from_str::<Value>(&self.value)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({"match_none": {}})),
        }
    }
}

fn or_dash(s: &str) -> &str {
    if s.trim().is_empty() { "∞" } else { s }
}

/// Rentang waktu dalam epoch millis (sudah diselesaikan dari date math).
#[derive(Clone, Copy, Debug)]
pub struct TimeBounds {
    pub from: i64,
    pub to: i64,
}

pub fn build(
    mode: QueryMode,
    text: &str,
    filters: &[Filter],
    time: Option<(&str, TimeBounds)>,
) -> Result<Value, String> {
    let mut must = Vec::new();
    let text = text.trim();
    if !text.is_empty() {
        must.push(match mode {
            QueryMode::Kql => super::kql::to_dsl(text)?,
            QueryMode::Lucene => {
                json!({"query_string": {"query": text, "analyze_wildcard": true}})
            }
            QueryMode::Dsl => {
                let value: Value =
                    serde_json::from_str(text).map_err(|e| format!("DSL tidak valid: {e}"))?;
                if !value.is_object() {
                    return Err("DSL harus berupa objek query, mis. {\"match_all\": {}}".into());
                }
                value
            }
        });
    }
    let mut filter = Vec::new();
    let mut must_not = Vec::new();
    if let Some((field, bounds)) = time {
        filter.push(json!({"range": {field: {
            "gte": bounds.from, "lte": bounds.to, "format": "epoch_millis"}}}));
    }
    for f in filters.iter().filter(|f| f.enabled && !f.field.is_empty()) {
        if f.negate {
            must_not.push(f.to_dsl());
        } else {
            filter.push(f.to_dsl());
        }
    }
    if must.is_empty() && filter.is_empty() && must_not.is_empty() {
        return Ok(json!({"match_all": {}}));
    }
    let mut bool_query = serde_json::Map::new();
    for (key, list) in [("must", must), ("filter", filter), ("must_not", must_not)] {
        if !list.is_empty() {
            bool_query.insert(key.into(), Value::Array(list));
        }
    }
    Ok(json!({"bool": bool_query}))
}

/// Selesaikan date math sederhana (`now`, `now-15m`, `now/d`, `now-7d/d`) atau tanggal ISO
/// menjadi epoch millis zona lokal.
pub fn resolve_time(expr: &str, now_ms: i64) -> Option<i64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let expr = expr.trim();
    if let Some(rest) = expr.strip_prefix("now") {
        let (ops, round) = match rest.split_once('/') {
            Some((ops, round)) => (ops, Some(round)),
            None => (rest, None),
        };
        let mut ms = now_ms;
        let mut chars = ops.chars().peekable();
        while let Some(sign) = chars.next() {
            let sign = match sign {
                '+' => 1,
                '-' => -1,
                _ => return None,
            };
            let mut digits = String::new();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                digits.push(chars.next()?);
            }
            let amount: i64 = if digits.is_empty() {
                1
            } else {
                digits.parse().ok()?
            };
            let unit = unit_ms(chars.next()?)?;
            ms += sign * amount * unit;
        }
        if let Some(round) = round {
            let unit = unit_ms(round.chars().next()?)?;
            let offset = Local
                .timestamp_millis_opt(ms)
                .single()?
                .offset()
                .local_minus_utc() as i64
                * 1000;
            ms = (ms + offset).div_euclid(unit) * unit - offset;
        }
        return Some(ms);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(expr) {
        return Some(dt.timestamp_millis());
    }
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(expr, format) {
            return Local
                .from_local_datetime(&naive)
                .single()
                .map(|d| d.timestamp_millis());
        }
    }
    let date = NaiveDate::parse_from_str(expr, "%Y-%m-%d").ok()?;
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()
        .map(|d| d.timestamp_millis())
}

fn unit_ms(unit: char) -> Option<i64> {
    Some(match unit {
        's' => 1_000,
        'm' => 60_000,
        'h' | 'H' => 3_600_000,
        'd' => 86_400_000,
        'w' => 7 * 86_400_000,
        'M' => 30 * 86_400_000,
        'y' => 365 * 86_400_000,
        _ => return None,
    })
}

const INTERVALS: [(i64, &str); 16] = [
    (1_000, "1s"),
    (5_000, "5s"),
    (10_000, "10s"),
    (30_000, "30s"),
    (60_000, "1m"),
    (300_000, "5m"),
    (600_000, "10m"),
    (1_800_000, "30m"),
    (3_600_000, "1h"),
    (10_800_000, "3h"),
    (43_200_000, "12h"),
    (86_400_000, "1d"),
    (604_800_000, "7d"),
    (2_592_000_000, "30d"),
    (7_776_000_000, "90d"),
    (31_536_000_000, "365d"),
];

/// Interval histogram otomatis: bucket terkecil yang menghasilkan ≤ `target` bucket.
pub fn auto_interval(span_ms: i64, target: i64) -> (i64, &'static str) {
    let raw = span_ms.max(1) / target.max(1);
    INTERVALS
        .iter()
        .copied()
        .find(|(ms, _)| *ms >= raw)
        .unwrap_or(INTERVALS[INTERVALS.len() - 1])
}

/// Offset zona lokal untuk `time_zone` ES, mis. `+07:00`.
pub fn local_tz() -> String {
    chrono::Local::now().format("%:z").to_string()
}

/// Nilai bersarang dari `_source` dengan path bertitik; mendukung kunci literal bertitik.
pub fn get_path<'a>(source: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(v) = source.get(path) {
        return Some(v);
    }
    let (head, rest) = path.split_once('.')?;
    let mut prefix = head.to_owned();
    let mut remaining = rest;
    loop {
        if let Some(inner) = source.get(&prefix)
            && let Some(found) = get_path(inner, remaining)
        {
            return Some(found);
        }
        let (next, rest) = remaining.split_once('.')?;
        prefix = format!("{prefix}.{next}");
        remaining = rest;
    }
}

/// Teks satu baris untuk sel tabel.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        Value::Array(items) if items.iter().all(|v| !v.is_object()) => {
            items.iter().map(display).collect::<Vec<_>>().join(", ")
        }
        other => other.to_string(),
    }
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[("salah satu dari", "is one of"), ("di antara", "between")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("query.rs"), EN);
    }

    #[test]
    fn builds_bool_query() {
        let filters = vec![
            Filter::is("status", "500", false),
            Filter::is("host", "web-1", true),
            Filter {
                enabled: false,
                ..Filter::is("x", "y", false)
            },
        ];
        let q = build(
            QueryMode::Lucene,
            "error",
            &filters,
            Some(("@timestamp", TimeBounds { from: 1, to: 2 })),
        )
        .unwrap();
        assert_eq!(q["bool"]["must"][0]["query_string"]["query"], "error");
        assert_eq!(q["bool"]["filter"].as_array().unwrap().len(), 2);
        assert_eq!(q["bool"]["filter"][0]["range"]["@timestamp"]["gte"], 1);
        assert_eq!(q["bool"]["must_not"][0]["match_phrase"]["host"], "web-1");
        assert_eq!(
            build(QueryMode::Lucene, " ", &[], None).unwrap(),
            json!({"match_all": {}})
        );
        assert!(build(QueryMode::Dsl, "{bad", &[], None).is_err());
    }

    #[test]
    fn dsl_filters() {
        let mut f = Filter::is("status", "500", false);
        f.make_dsl();
        assert_eq!(f.op, FilterOp::Dsl);
        assert_eq!(f.to_dsl(), json!({"match_phrase": {"status": "500"}}));
        f.value = "bukan json".into();
        assert_eq!(f.to_dsl(), json!({"match_none": {}}));
        f.value = "{\"term\": {\"a\": 1}}".into();
        let q = build(QueryMode::Kql, "", &[f], None).unwrap();
        assert_eq!(q["bool"]["filter"][0]["term"]["a"], 1);
    }

    #[test]
    fn resolves_date_math() {
        let now = 1_700_000_000_000;
        assert_eq!(resolve_time("now", now), Some(now));
        assert_eq!(resolve_time("now-15m", now), Some(now - 900_000));
        assert_eq!(
            resolve_time("now-1d+2h", now),
            Some(now - 86_400_000 + 7_200_000)
        );
        let day = resolve_time("now/d", now).unwrap();
        assert!(day <= now && now - day < 86_400_000);
        assert!(resolve_time("2026-11-01", now).is_some());
        assert!(resolve_time("2026-11-01T10:00:00Z", now).is_some());
        assert_eq!(resolve_time("kemarin", now), None);
    }

    #[test]
    fn picks_interval() {
        assert_eq!(auto_interval(15 * 60_000, 60).1, "30s");
        assert_eq!(auto_interval(24 * 3_600_000, 60).1, "30m");
        assert_eq!(auto_interval(7 * 86_400_000, 60).1, "3h");
    }

    #[test]
    fn reads_nested_paths() {
        let doc = json!({"a": {"b": {"c": 1}}, "x.y": 2, "k": {"l.m": 3}});
        assert_eq!(get_path(&doc, "a.b.c"), Some(&json!(1)));
        assert_eq!(get_path(&doc, "x.y"), Some(&json!(2)));
        assert_eq!(get_path(&doc, "k.l.m"), Some(&json!(3)));
        assert_eq!(get_path(&doc, "a.z"), None);
    }
}
