//! Parser KQL (Kibana Query Language) ke Query DSL.
//!
//! Ditulis sendiri (grammar Kibana berlisensi Elastic/SSPL). Yang didukung:
//! `field:value`, `field:"frasa"`, `field:val*`, `field:*` (exists), `field >= 10`,
//! `field:(a or b)`, `nested:{ sub:x }`, teks bebas, `and`/`or`/`not`, kurung.
//! Dua klausa berdampingan tanpa operator dianggap `and`.

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBrace,
    RBrace,
    Colon,
    Range(&'static str),
    And,
    Or,
    Not,
    Word(Word),
}

#[derive(Clone, Debug, PartialEq)]
struct Word {
    text: String,
    quoted: bool,
    /// Mengandung `*` yang tidak di-escape.
    wildcard: bool,
}

fn lex(input: &str) -> Result<Vec<(Tok, usize)>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let c = chars[i];
        let tok = match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            ':' => Tok::Colon,
            '<' | '>' => {
                let eq = chars.get(i + 1) == Some(&'=');
                if eq {
                    i += 1;
                }
                Tok::Range(match (c, eq) {
                    ('<', true) => "lte",
                    ('<', false) => "lt",
                    (_, true) => "gte",
                    _ => "gt",
                })
            }
            '"' => {
                let mut text = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(format!("KQL: kutip tidak ditutup (posisi {start})")),
                        Some('"') => break,
                        Some('\\') if i + 1 < chars.len() => {
                            i += 1;
                            text.push(chars[i]);
                        }
                        Some(ch) => text.push(*ch),
                    }
                    i += 1;
                }
                Tok::Word(Word {
                    text,
                    quoted: true,
                    wildcard: false,
                })
            }
            _ => {
                let mut text = String::new();
                let mut wildcard = false;
                while let Some(&ch) = chars.get(i) {
                    if ch.is_whitespace() || "(){}:<>\"".contains(ch) {
                        break;
                    }
                    if ch == '\\' && i + 1 < chars.len() {
                        // Karakter ter-escape selalu literal.
                        text.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    wildcard |= ch == '*';
                    text.push(ch);
                    i += 1;
                }
                let tok = match text.to_ascii_lowercase().as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    _ => Tok::Word(Word {
                        text,
                        quoted: false,
                        wildcard,
                    }),
                };
                out.push((tok, start));
                continue;
            }
        };
        out.push((tok, start));
        i += 1;
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    /// Kedalaman `field:( ... )`; `and`/`or`/`not` di dalamnya milik grup nilai.
    group_depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> Option<&Tok> {
        self.toks.get(self.pos + offset).map(|(t, _)| t)
    }

    fn take_word(&mut self) -> Option<Word> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.pos += 1;
                Some(w)
            }
            _ => None,
        }
    }

    fn error(&self, what: &str) -> String {
        match self.toks.get(self.pos) {
            Some((tok, at)) => format!("KQL: {what}, ditemukan {tok:?} di posisi {at}"),
            None => format!("KQL: {what}, tetapi query sudah berakhir"),
        }
    }

    fn expect(&mut self, tok: Tok, what: &str) -> Result<(), String> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(what))
        }
    }

    fn starts_clause(&self) -> bool {
        matches!(self.peek(), Some(Tok::Word(_) | Tok::LParen | Tok::Not))
    }

    /// `prefix` = path nested (mis. `items.`) di dalam `field:{ }`.
    fn or_expr(&mut self, prefix: &str) -> Result<Value, String> {
        let mut items = vec![self.and_expr(prefix)?];
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            items.push(self.and_expr(prefix)?);
        }
        Ok(combine(items, "should"))
    }

    fn and_expr(&mut self, prefix: &str) -> Result<Value, String> {
        let mut items = vec![self.not_expr(prefix)?];
        loop {
            if self.peek() == Some(&Tok::And) {
                self.pos += 1;
            } else if !self.starts_clause() {
                break;
            }
            items.push(self.not_expr(prefix)?);
        }
        Ok(combine(items, "filter"))
    }

    fn not_expr(&mut self, prefix: &str) -> Result<Value, String> {
        if self.peek() == Some(&Tok::Not) {
            self.pos += 1;
            let inner = self.not_expr(prefix)?;
            return Ok(json!({"bool": {"must_not": [inner]}}));
        }
        self.primary(prefix)
    }

    fn primary(&mut self, prefix: &str) -> Result<Value, String> {
        match (self.peek(), self.peek_at(1)) {
            (Some(Tok::LParen), _) => {
                self.pos += 1;
                let inner = self.or_expr(prefix)?;
                self.expect(Tok::RParen, "diharapkan ')'")?;
                Ok(inner)
            }
            (Some(Tok::Word(w)), Some(Tok::Colon)) if !w.quoted => {
                let field = format!("{prefix}{}", w.text);
                self.pos += 2;
                if self.peek() == Some(&Tok::LBrace) {
                    self.pos += 1;
                    let inner = self.or_expr(&format!("{field}."))?;
                    self.expect(Tok::RBrace, "diharapkan '}'")?;
                    return Ok(
                        json!({"nested": {"path": field, "query": inner, "score_mode": "none"}}),
                    );
                }
                self.value_or(&field)
            }
            (Some(Tok::Word(w)), Some(Tok::Range(op))) if !w.quoted => {
                let field = format!("{prefix}{}", w.text);
                let op = *op;
                self.pos += 2;
                let value = self
                    .take_word()
                    .ok_or_else(|| self.error("diharapkan nilai setelah operator range"))?;
                Ok(json!({"range": {field: {op: value.text}}}))
            }
            (Some(Tok::Word(_)), _) => Ok(free_text(&self.words())),
            _ => Err(self.error("diharapkan field, nilai, atau '('")),
        }
    }

    fn value_or(&mut self, field: &str) -> Result<Value, String> {
        let mut items = vec![self.value_and(field)?];
        while self.group_depth > 0 && self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            items.push(self.value_and(field)?);
        }
        Ok(combine(items, "should"))
    }

    fn value_and(&mut self, field: &str) -> Result<Value, String> {
        let mut items = vec![self.value_not(field)?];
        while self.group_depth > 0 && self.peek() == Some(&Tok::And) {
            self.pos += 1;
            items.push(self.value_not(field)?);
        }
        Ok(combine(items, "filter"))
    }

    fn value_not(&mut self, field: &str) -> Result<Value, String> {
        if self.group_depth > 0 && self.peek() == Some(&Tok::Not) {
            self.pos += 1;
            let inner = self.value_not(field)?;
            return Ok(json!({"bool": {"must_not": [inner]}}));
        }
        if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            self.group_depth += 1;
            let inner = self.value_or(field);
            self.group_depth -= 1;
            let inner = inner?;
            self.expect(Tok::RParen, "diharapkan ')'")?;
            return Ok(inner);
        }
        let words = self.words();
        if words.is_empty() {
            return Err(self.error(&format!("diharapkan nilai untuk field {field}")));
        }
        Ok(field_value(field, &words))
    }

    /// Kata berurutan sampai operator/kurung/akhir, atau sampai kata yang adalah field berikutnya.
    fn words(&mut self) -> Vec<Word> {
        let mut out = Vec::new();
        while let Some(Tok::Word(_)) = self.peek() {
            if !out.is_empty() && matches!(self.peek_at(1), Some(Tok::Colon | Tok::Range(_))) {
                break;
            }
            out.extend(self.take_word());
        }
        out
    }
}

fn combine(mut items: Vec<Value>, kind: &str) -> Value {
    if items.len() == 1 {
        return items.remove(0);
    }
    if kind == "should" {
        json!({"bool": {"should": items, "minimum_should_match": 1}})
    } else {
        json!({"bool": {"filter": items}})
    }
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_wildcard(words: &[Word]) -> bool {
    words.iter().any(|w| w.wildcard)
}

fn field_value(field: &str, words: &[Word]) -> Value {
    if let [w] = words
        && !w.quoted
        && w.text == "*"
    {
        return json!({"exists": {"field": field}});
    }
    if field.contains('*') || has_wildcard(words) {
        return json!({"query_string": {
            "query": lucene_text(words), "fields": [field], "analyze_wildcard": true}});
    }
    let text = join(words);
    if words.len() == 1 && words[0].quoted {
        json!({"match_phrase": {field: text}})
    } else {
        json!({"match": {field: text}})
    }
}

fn free_text(words: &[Word]) -> Value {
    if has_wildcard(words) {
        return json!({"query_string": {"query": lucene_text(words), "analyze_wildcard": true}});
    }
    let kind = if words.len() == 1 && words[0].quoted {
        "phrase"
    } else {
        "best_fields"
    };
    json!({"multi_match": {"query": join(words), "type": kind, "lenient": true}})
}

/// Teks `query_string`: kata dikutip jadi frasa; karakter khusus di-escape kecuali `*`.
fn lucene_text(words: &[Word]) -> String {
    const SPECIAL: &str = "+-=&|!(){}[]^\"~?:\\/<>";
    words
        .iter()
        .map(|w| {
            if w.quoted {
                return format!("\"{}\"", w.text.replace('\\', "\\\\").replace('"', "\\\""));
            }
            let mut out = String::new();
            for c in w.text.chars() {
                if SPECIAL.contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// KQL -> Query DSL. Query kosong menghasilkan `match_all`.
pub fn to_dsl(input: &str) -> Result<Value, String> {
    let toks = lex(input)?;
    if toks.is_empty() {
        return Ok(json!({"match_all": {}}));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        group_depth: 0,
    };
    let value = p.or_expr("")?;
    if p.pos < p.toks.len() {
        return Err(p.error("token tidak terduga"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> Value {
        to_dsl(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn field_values() {
        assert_eq!(q("status:500"), json!({"match": {"status": "500"}}));
        assert_eq!(
            q("msg:\"null pointer\""),
            json!({"match_phrase": {"msg": "null pointer"}})
        );
        assert_eq!(
            q("msg:null pointer"),
            json!({"match": {"msg": "null pointer"}})
        );
        assert_eq!(q("host:*"), json!({"exists": {"field": "host"}}));
        assert_eq!(
            q("host:web-*"),
            json!({"query_string": {"query": "web\\-*", "fields": ["host"], "analyze_wildcard": true}})
        );
        assert_eq!(
            q("bytes >= 100"),
            json!({"range": {"bytes": {"gte": "100"}}})
        );
        assert_eq!(q("bytes<5"), json!({"range": {"bytes": {"lt": "5"}}}));
        assert_eq!(q(""), json!({"match_all": {}}));
    }

    #[test]
    fn boolean_logic() {
        assert_eq!(
            q("a:1 and not b:2"),
            json!({"bool": {"filter": [{"match": {"a": "1"}}, {"bool": {"must_not": [{"match": {"b": "2"}}]}}]}})
        );
        assert_eq!(
            q("a:1 or b:2 and c:3"),
            json!({"bool": {"should": [{"match": {"a": "1"}},
                {"bool": {"filter": [{"match": {"b": "2"}}, {"match": {"c": "3"}}]}}],
                "minimum_should_match": 1}})
        );
        assert_eq!(
            q("code:(400 or 404)"),
            json!({"bool": {"should": [{"match": {"code": "400"}}, {"match": {"code": "404"}}],
                "minimum_should_match": 1}})
        );
        // Berdampingan tanpa operator = and.
        assert_eq!(
            q("error host:x"),
            json!({"bool": {"filter": [
                {"multi_match": {"query": "error", "type": "best_fields", "lenient": true}},
                {"match": {"host": "x"}}]}})
        );
        assert_eq!(q("(a:1)"), json!({"match": {"a": "1"}}));
        assert_eq!(
            q("A:1 AND B:2")["bool"]["filter"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn nested_escape_and_errors() {
        assert_eq!(
            q("items:{ name:x and qty > 2 }"),
            json!({"nested": {"path": "items", "score_mode": "none", "query": {"bool": {"filter": [
                {"match": {"items.name": "x"}}, {"range": {"items.qty": {"gt": "2"}}}]}}}})
        );
        assert_eq!(q("path:C\\:\\\\x"), json!({"match": {"path": "C:\\x"}}));
        assert_eq!(
            q("\"exact phrase\""),
            json!({"multi_match": {"query": "exact phrase", "type": "phrase", "lenient": true}})
        );
        assert!(to_dsl("a:(1 or").is_err());
        assert!(to_dsl("a:\"open").is_err());
        assert!(to_dsl("a:1 )").is_err());
        assert!(to_dsl("bytes >").is_err());
    }
}
