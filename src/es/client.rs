//! Satu-satunya jalur HTTP ke Elasticsearch: auth, TLS, timeout, failover, guard read-only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::Value;

use crate::i18n::{t, tf};
use crate::profile::{AuthKind, Profile};

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub millis: u128,
    pub warnings: Vec<String>,
}

impl Response {
    pub fn is_ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Body sebagai JSON jika status sukses, selain itu pesan error terstruktur.
    pub fn json(self) -> Result<Value, String> {
        let value: Value = serde_json::from_str(&self.body).unwrap_or(Value::Null);
        if self.is_ok() {
            Ok(value)
        } else if value.is_null() {
            Err(format!("HTTP {}: {}", self.status, self.body.trim()))
        } else {
            Err(format!("HTTP {}: {}", self.status, describe_error(&value)))
        }
    }
}

/// Ringkas error ES (`type: reason`, root_cause, caused_by bertingkat).
pub fn describe_error(value: &Value) -> String {
    let Some(error) = value.get("error") else {
        return value.to_string();
    };
    if let Some(text) = error.as_str() {
        return text.to_owned();
    }
    let mut lines = vec![one_error(error)];
    for cause in error["root_cause"].as_array().into_iter().flatten() {
        let line = one_error(cause);
        if !lines.contains(&line) {
            lines.push(format!("root cause: {line}"));
        }
    }
    let mut cause = &error["caused_by"];
    while cause.is_object() {
        lines.push(format!("caused by: {}", one_error(cause)));
        cause = &cause["caused_by"];
    }
    lines.join("\n")
}

fn one_error(error: &Value) -> String {
    let kind = error["type"].as_str().unwrap_or("error");
    let reason = error["reason"].as_str().unwrap_or("");
    format!("{kind}: {reason}")
}

/// Endpoint POST/DELETE yang tidak mengubah data, diizinkan pada profile read-only.
const READ_POST: &[&str] = &[
    "_search",
    "_count",
    "_msearch",
    "_field_caps",
    "_query",
    "_async_search",
    "_mget",
    "_validate",
    "_explain",
    "_terms_enum",
    "_resolve",
    "_analyze",
    "_pit",
    "_has_privileges",
    "_render",
    "_simulate",
    "_simulate_index",
    "_simulate_template",
    "_eql",
    "_sql",
    "_execute",
];

pub fn is_write(method: &str, path: &str) -> bool {
    let method = method.to_ascii_uppercase();
    if method == "GET" || method == "HEAD" {
        return false;
    }
    let path = path.split('?').next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match method.as_str() {
        "POST" => !segments.iter().any(|s| READ_POST.contains(s)),
        // Menutup PIT/async search bukan perubahan data.
        "DELETE" => !segments
            .first()
            .is_some_and(|s| *s == "_pit" || *s == "_async_search"),
        _ => true,
    }
}

pub fn is_ndjson(path: &str) -> bool {
    path.split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .any(|s| s == "_bulk" || s == "_msearch")
}

/// PRF-02: urutan percobaan node, melingkar mulai dari `start % len`.
fn node_order(start: usize, len: usize) -> Vec<usize> {
    (0..len).map(|i| (start % len + i) % len).collect()
}

pub struct Conn {
    http: reqwest::blocking::Client,
    urls: Vec<String>,
    auth: Option<String>,
    headers: Vec<(String, String)>,
    pub read_only: bool,
    /// Untuk menyusun cURL tanpa membocorkan secret.
    pub username: String,
    pub auth_kind: AuthKind,
    /// Jumlah request yang sedang berjalan (status bar).
    inflight: std::sync::atomic::AtomicUsize,
    /// PRF-02: penghitung round-robin; tiap request mulai dari node berbeda.
    next: std::sync::atomic::AtomicUsize,
}

/// Menurunkan penghitung request saat `send` selesai, apa pun hasilnya.
struct InflightGuard<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Conn {
    pub fn new(profile: &Profile, secret: Option<&str>) -> Result<Self, String> {
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(profile.timeout_secs.max(1)))
            .connect_timeout(Duration::from_secs(10))
            .danger_accept_invalid_certs(profile.insecure);
        let proxy = profile.proxy.trim();
        if !proxy.is_empty() {
            let proxy =
                reqwest::Proxy::all(proxy).map_err(|e| tf("Proxy tidak valid: {0}", &[&e]))?;
            builder = builder.proxy(proxy);
        }
        // PRF-04: client cert + key digabung jadi satu PEM.
        let identity = match (
            profile.client_cert_path.trim(),
            profile.client_key_path.trim(),
        ) {
            ("", "") => None,
            (cert, key) if !cert.is_empty() && !key.is_empty() => {
                let mut pem = std::fs::read(cert)
                    .map_err(|e| tf("Gagal membaca client cert {0}: {1}", &[&cert, &e]))?;
                pem.push(b'\n');
                pem.extend(
                    std::fs::read(key)
                        .map_err(|e| tf("Gagal membaca private key {0}: {1}", &[&key, &e]))?,
                );
                Some(pem)
            }
            _ => return Err(t("Client cert dan private key harus diisi keduanya.").into()),
        };
        let fingerprint = profile.ca_fingerprint.trim();
        if !fingerprint.is_empty() && !profile.insecure {
            builder = builder.use_preconfigured_tls(super::tls::pinned_config(
                fingerprint,
                identity.as_deref(),
            )?);
        } else {
            let ca = profile.ca_cert_path.trim();
            if !ca.is_empty() {
                let pem = std::fs::read(ca)
                    .map_err(|e| tf("Gagal membaca CA cert {0}: {1}", &[&ca, &e]))?;
                let cert = reqwest::Certificate::from_pem(&pem)
                    .map_err(|e| tf("CA cert tidak valid: {0}", &[&e]))?;
                builder = builder.add_root_certificate(cert);
            }
            if let Some(pem) = &identity {
                let id = reqwest::Identity::from_pem(pem)
                    .map_err(|e| tf("Client cert/key tidak valid: {0}", &[&e]))?;
                builder = builder.identity(id);
            }
        }
        let http = builder.build().map_err(|e| e.to_string())?;
        let secret = secret.unwrap_or_default();
        let b64 = base64::engine::general_purpose::STANDARD;
        let auth = match profile.auth {
            AuthKind::None => None,
            AuthKind::Basic => Some(format!(
                "Basic {}",
                b64.encode(format!("{}:{}", profile.username, secret))
            )),
            AuthKind::ApiKey if secret.contains(':') => {
                Some(format!("ApiKey {}", b64.encode(secret)))
            }
            AuthKind::ApiKey => Some(format!("ApiKey {secret}")),
            AuthKind::Bearer => Some(format!("Bearer {secret}")),
        };
        let urls: Vec<String> = profile
            .urls
            .iter()
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .filter(|u| !u.is_empty())
            .collect();
        if urls.is_empty() {
            return Err(t("Profile belum punya URL node.").into());
        }
        Ok(Self {
            http,
            urls,
            auth,
            headers: profile.headers.clone(),
            read_only: profile.read_only,
            username: profile.username.clone(),
            auth_kind: profile.auth,
            inflight: Default::default(),
            next: Default::default(),
        })
    }

    pub fn inflight(&self) -> usize {
        self.inflight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn base_url(&self) -> &str {
        &self.urls[0]
    }

    /// Kirim request mentah. Mencoba node berikutnya bila koneksi gagal.
    pub fn send(&self, method: &str, path: &str, body: Option<&str>) -> Result<Response, String> {
        self.send_tagged(method, path, body, None)
    }

    /// CON-42: kirim dengan header `X-Opaque-Id` supaya task-nya bisa dibatalkan di server.
    pub fn send_tagged(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        opaque_id: Option<&str>,
    ) -> Result<Response, String> {
        let method = method.to_ascii_uppercase();
        if self.read_only && is_write(&method, path) {
            return Err(tf(
                "Profile read-only: {0} {1} diblokir karena dapat mengubah data.",
                &[&method, &path],
            ));
        }
        self.inflight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _guard = InflightGuard(&self.inflight);
        let http_method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| tf("Method tidak dikenal: {0}", &[&method]))?;
        let path = format!("/{}", path.trim().trim_start_matches('/'));
        let content_type = if is_ndjson(&path) {
            "application/x-ndjson"
        } else {
            "application/json"
        };
        let mut last_error = String::new();
        // PRF-02: mulai dari node bergilir, lalu failover melingkar tiap node sekali.
        let start = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for idx in node_order(start, self.urls.len()) {
            let base = &self.urls[idx];
            let mut request = self
                .http
                .request(http_method.clone(), format!("{base}{path}"))
                .header("Content-Type", content_type)
                .header("Accept", "application/json, text/plain;q=0.9");
            if let Some(auth) = &self.auth {
                request = request.header("Authorization", auth);
            }
            for (name, value) in &self.headers {
                request = request.header(name, value);
            }
            if let Some(id) = opaque_id {
                request = request.header("X-Opaque-Id", id);
            }
            if let Some(body) = body {
                request = request.body(body.to_owned());
            }
            let started = Instant::now();
            match request.send() {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let warnings = response
                        .headers()
                        .get_all("warning")
                        .iter()
                        .filter_map(|v| v.to_str().ok().map(str::to_owned))
                        .collect();
                    let body = response.text().map_err(|e| e.to_string())?;
                    return Ok(Response {
                        status,
                        body,
                        millis: started.elapsed().as_millis(),
                        warnings,
                    });
                }
                Err(err) if err.is_connect() || err.is_timeout() => {
                    last_error = describe_reqwest(&err);
                }
                Err(err) => return Err(describe_reqwest(&err)),
            }
        }
        Err(last_error)
    }

    pub fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let body = body.map(Value::to_string);
        self.send(method, path, body.as_deref())?.json()
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        self.json("GET", path, None)
    }
}

/// Batalkan semua task server yang membawa `X-Opaque-Id` ini. Mengembalikan jumlahnya.
pub fn cancel_opaque(conn: &Conn, opaque_id: &str) -> Result<usize, String> {
    let tasks = conn.get("/_tasks?detailed=true&group_by=none")?;
    let ids: Vec<String> = tasks["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["headers"]["X-Opaque-Id"] == opaque_id && t["cancellable"] == true)
        .filter_map(|t| Some(format!("{}:{}", t["node"].as_str()?, t["id"])))
        .collect();
    for id in &ids {
        conn.json("POST", &format!("/_tasks/{id}/_cancel"), None)?;
    }
    Ok(ids.len())
}

fn describe_reqwest(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        text.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    if text.contains("certificate") || text.contains("UnknownIssuer") {
        text.push('\n');
        text.push_str(&tf(
            "Sertifikat tidak bisa diverifikasi. Tambahkan CA cert di profile, atau aktifkan \"{0}\".",
            &[&t("Lewati verifikasi sertifikat (tidak aman)")],
        ));
    }
    text
}

impl Conn {
    /// `Conn` untuk profile dengan secret dari keychain. Membaca keychain bisa memblokir,
    /// jadi hanya dipanggil di thread latar (lihat `Source`).
    pub fn connect(profile: &Profile) -> Result<Self, String> {
        Self::new(profile, crate::profile::secret::get(&profile.id).as_deref())
    }
}

/// Koneksi yang siap dipakai, atau profile yang `Conn`-nya baru dibangun di thread latar
/// supaya thread UI tidak menunggu keychain OS.
#[derive(Clone)]
pub enum Source {
    Ready(Arc<Conn>),
    Profile(Box<Profile>),
}

impl Source {
    pub fn open(&self) -> Result<Arc<Conn>, String> {
        match self {
            Source::Ready(conn) => Ok(conn.clone()),
            Source::Profile(profile) => Conn::connect(profile).map(Arc::new),
        }
    }
}

/// Koneksi uji ke cluster nyata dari `ELASTIC_URL` dkk. Selalu read-only.
#[cfg(test)]
pub fn live_conn() -> Option<Conn> {
    let (mut profile, secret) = Profile::from_env()?;
    profile.read_only = true;
    Some(Conn::new(&profile, secret.as_deref()).expect("profile env valid"))
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("Proxy tidak valid: {0}", "Invalid proxy: {0}"),
    (
        "Gagal membaca client cert {0}: {1}",
        "Failed to read client cert {0}: {1}",
    ),
    (
        "Gagal membaca private key {0}: {1}",
        "Failed to read private key {0}: {1}",
    ),
    (
        "Client cert dan private key harus diisi keduanya.",
        "Client cert and private key must both be set.",
    ),
    (
        "Gagal membaca CA cert {0}: {1}",
        "Failed to read CA cert {0}: {1}",
    ),
    ("CA cert tidak valid: {0}", "Invalid CA cert: {0}"),
    (
        "Client cert/key tidak valid: {0}",
        "Invalid client cert/key: {0}",
    ),
    (
        "Profile belum punya URL node.",
        "The profile has no node URL yet.",
    ),
    (
        "Profile read-only: {0} {1} diblokir karena dapat mengubah data.",
        "Read-only profile: {0} {1} blocked because it could modify data.",
    ),
    ("Method tidak dikenal: {0}", "Unknown method: {0}"),
    (
        "Sertifikat tidak bisa diverifikasi. Tambahkan CA cert di profile, atau aktifkan \"{0}\".",
        "The certificate could not be verified. Add a CA cert to the profile, or enable \"{0}\".",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("client.rs"), EN);
    }

    #[test]
    fn source_ready_memakai_conn_yang_sama() {
        let conn = Arc::new(Conn::new(&Profile::default(), None).unwrap());
        let opened = Source::Ready(conn.clone()).open().unwrap();
        assert!(Arc::ptr_eq(&conn, &opened));
    }

    #[test]
    fn node_order_melingkar() {
        assert_eq!(node_order(0, 3), vec![0, 1, 2]);
        assert_eq!(node_order(4, 3), vec![1, 2, 0]);
        assert_eq!(node_order(7, 1), vec![0]);
    }

    #[test]
    fn write_detection() {
        assert!(!is_write("GET", "/_cat/indices"));
        assert!(!is_write("POST", "/logs-*/_search?size=1"));
        assert!(!is_write("POST", "/_query"));
        assert!(!is_write("DELETE", "/_pit"));
        assert!(is_write("POST", "/_bulk"));
        assert!(is_write("PUT", "/idx"));
        assert!(is_write("DELETE", "/idx"));
        assert!(is_write("POST", "/idx/_delete_by_query"));
        assert!(is_write("POST", "/idx/_close"));
    }

    #[test]
    fn error_description() {
        let value = serde_json::json!({"error": {"type": "index_not_found_exception",
            "reason": "no such index [x]",
            "root_cause": [{"type": "index_not_found_exception", "reason": "no such index [x]"}],
            "caused_by": {"type": "a", "reason": "b"}}});
        let text = describe_error(&value);
        assert_eq!(
            text,
            "index_not_found_exception: no such index [x]\ncaused by: a: b"
        );
    }

    #[test]
    #[ignore = "butuh cluster: set ELASTIC_URL/ELASTIC_USER/ELASTIC_PASSWORD"]
    fn live_read_only_guard() {
        let conn = live_conn().expect("env");
        assert!(conn.get("/").unwrap()["version"]["number"].is_string());
        let err = conn
            .send("PUT", "/elastic-gui-should-not-exist", None)
            .unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        // CON-42: pencarian task per X-Opaque-Id jalan walau tidak ada yang cocok.
        assert_eq!(cancel_opaque(&conn, "elastic-gui-tidak-ada").unwrap(), 0);
        let tagged = conn
            .send_tagged("GET", "/", None, Some("elastic-gui-test"))
            .unwrap();
        assert!(tagged.is_ok());
    }

    #[test]
    #[ignore = "butuh cluster + LIVE_FINGERPRINT"]
    fn live_fingerprint_pinning() {
        let (mut profile, secret) = Profile::from_env().expect("env");
        profile.read_only = true;
        profile.ca_fingerprint = std::env::var("LIVE_FINGERPRINT").expect("LIVE_FINGERPRINT");
        let ok = Conn::new(&profile, secret.as_deref()).unwrap().get("/");
        assert!(ok.is_ok(), "pin benar harus tersambung: {ok:?}");
        profile.ca_fingerprint = "00".repeat(32);
        let err = Conn::new(&profile, secret.as_deref())
            .unwrap()
            .get("/")
            .unwrap_err();
        println!("pin salah: {err}");
        assert!(err.contains("fingerprint"), "{err}");
    }
}
