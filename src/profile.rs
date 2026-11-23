//! Profile cluster, penyimpanan lokal, dan secret di keychain OS.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::i18n::{Lang, t, tf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Env {
    Dev,
    Staging,
    Prod,
    Other,
}

impl Env {
    pub const ALL: [Env; 4] = [Env::Dev, Env::Staging, Env::Prod, Env::Other];

    pub fn label(self) -> &'static str {
        match self {
            Env::Dev => "dev",
            Env::Staging => "staging",
            Env::Prod => "prod",
            Env::Other => t("lainnya"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthKind {
    None,
    Basic,
    ApiKey,
    Bearer,
}

impl AuthKind {
    pub const ALL: [AuthKind; 4] = [
        AuthKind::None,
        AuthKind::Basic,
        AuthKind::ApiKey,
        AuthKind::Bearer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AuthKind::None => t("Tanpa auth"),
            AuthKind::Basic => "Basic (user/password)",
            AuthKind::ApiKey => "API key",
            AuthKind::Bearer => "Bearer token",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub env: Env,
    /// Satu atau lebih URL node; dicoba berurutan saat koneksi gagal.
    pub urls: Vec<String>,
    /// PRF-02: opsional, bagi beban antar node dengan titik mulai bergilir alih-alih
    /// selalu mulai dari node pertama.
    pub round_robin: bool,
    pub auth: AuthKind,
    pub username: String,
    /// File PEM CA tambahan (kosong = pakai CA sistem).
    pub ca_cert_path: String,
    /// Lewati verifikasi TLS. Berbahaya; UI menampilkan peringatan permanen.
    pub insecure: bool,
    /// PRF-06: fingerprint SHA-256 CA/sertifikat server yang dipercaya.
    pub ca_fingerprint: String,
    /// PRF-04: client certificate (PEM) dan private key (PEM) untuk mTLS.
    pub client_cert_path: String,
    pub client_key_path: String,
    pub timeout_secs: u64,
    /// PRF-08: HTTP(S) proxy, mis. `http://proxy:3128` (kosong = proxy sistem).
    pub proxy: String,
    pub headers: Vec<(String, String)>,
    /// Blokir semua request yang mengubah data.
    pub read_only: bool,
    /// Profile dari environment variable; tidak disimpan ke disk.
    #[serde(skip)]
    pub ephemeral: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: t("Cluster baru").into(),
            env: Env::Dev,
            urls: vec!["http://localhost:9200".into()],
            round_robin: false,
            auth: AuthKind::None,
            username: String::new(),
            ca_cert_path: String::new(),
            insecure: false,
            ca_fingerprint: String::new(),
            client_cert_path: String::new(),
            client_key_path: String::new(),
            timeout_secs: 30,
            proxy: String::new(),
            headers: Vec::new(),
            read_only: false,
            ephemeral: false,
        }
    }
}

impl Profile {
    pub fn is_prod(&self) -> bool {
        self.env == Env::Prod
    }

    /// Profile dari `ELASTIC_URL`, `ELASTIC_USER`, `ELASTIC_PASSWORD` / `ELASTIC_API_KEY`.
    /// Secret dikembalikan terpisah dan hanya disimpan di memori.
    pub fn from_env() -> Option<(Profile, Option<String>)> {
        let url = std::env::var("ELASTIC_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())?;
        let mut profile = Profile {
            id: "env".into(),
            name: "ENV".into(),
            urls: url.split(',').map(|u| u.trim().to_owned()).collect(),
            ephemeral: true,
            ..Profile::default()
        };
        let secret = if let Ok(key) = std::env::var("ELASTIC_API_KEY") {
            profile.auth = AuthKind::ApiKey;
            Some(key)
        } else if let Ok(user) = std::env::var("ELASTIC_USER") {
            profile.auth = AuthKind::Basic;
            profile.username = user;
            std::env::var("ELASTIC_PASSWORD").ok()
        } else {
            None
        };
        if url.contains("prod") {
            profile.env = Env::Prod;
        }
        Some((profile, secret))
    }
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("p{nanos:x}")
}

/// X-05: preferensi aplikasi di `settings.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub lang: Lang,
}

/// Direktori data aplikasi dan file JSON per profile.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open() -> Self {
        let dir = directories::ProjectDirs::from("id", "MengLabs", "elastic-gui")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".elastic-gui"));
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    /// File tidak ada atau rusak = pengaturan bawaan.
    pub fn load_settings(&self) -> AppSettings {
        read_json(&self.dir.join("settings.json")).unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<(), String> {
        write_json(&self.dir.join("settings.json"), settings)
    }

    pub fn load_profiles(&self) -> Vec<Profile> {
        read_json(&self.dir.join("profiles.json")).unwrap_or_default()
    }

    pub fn save_profiles(&self, profiles: &[Profile]) -> Result<(), String> {
        let persisted: Vec<&Profile> = profiles.iter().filter(|p| !p.ephemeral).collect();
        write_json(&self.dir.join("profiles.json"), &persisted)
    }

    pub fn load<T: DeserializeOwned>(&self, profile_id: &str, name: &str) -> Option<T> {
        read_json(&self.dir.join("profiles").join(profile_id).join(name))
    }

    pub fn save<T: Serialize>(&self, profile_id: &str, name: &str, value: &T) {
        let dir = self.dir.join("profiles").join(profile_id);
        let _ = std::fs::create_dir_all(&dir);
        if let Err(err) = write_json(&dir.join(name), value) {
            eprintln!("gagal menyimpan {name}: {err}");
        }
    }
}

fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize + ?Sized>(path: &std::path::Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Secret disimpan di keychain OS. Jika keychain tidak tersedia (mis. Linux tanpa
/// Secret Service), secret hanya disimpan di memori selama sesi.
pub mod secret {
    use super::*;

    const SERVICE: &str = "id.menglabs.elastic-gui";

    /// `None` = sudah dicek dan tidak ada di keychain (cache negatif).
    type Cache = HashMap<String, Option<String>>;

    fn session() -> &'static Mutex<Cache> {
        static SESSION: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
        SESSION.get_or_init(Default::default)
    }

    /// Ambil dari cache; bila belum pernah dicek, tanya `backend` lalu simpan hasilnya,
    /// termasuk "tidak ada", supaya keychain tidak disentuh berulang kali.
    fn lookup(
        cache: &Mutex<Cache>,
        profile_id: &str,
        backend: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        if let Some(hit) = cache.lock().ok().and_then(|m| m.get(profile_id).cloned()) {
            return hit;
        }
        let value = backend();
        // Pembacaan keychain bisa lama (dialog izin macOS); bila `set`/`delete` menulis
        // selama itu, nilai mereka lebih baru dan tidak boleh tertimpa hasil baca ini.
        match cache.lock() {
            Ok(mut map) => map.entry(profile_id.to_owned()).or_insert(value).clone(),
            Err(_) => value,
        }
    }

    fn remember(cache: &Mutex<Cache>, profile_id: &str, value: Option<String>) {
        if let Ok(mut map) = cache.lock() {
            map.insert(profile_id.to_owned(), value);
        }
    }

    /// Membaca keychain OS bila belum ada di cache sesi. Bisa memblokir (dialog izin
    /// macOS), jadi hanya dipanggil dari thread latar.
    pub fn get(profile_id: &str) -> Option<String> {
        lookup(session(), profile_id, || {
            keyring::Entry::new(SERVICE, profile_id)
                .ok()?
                .get_password()
                .ok()
        })
    }

    /// Isi cache sesi tanpa menyentuh keychain; `None` bila belum pernah dicek.
    pub fn cached(profile_id: &str) -> Option<Option<String>> {
        session().lock().ok()?.get(profile_id).cloned()
    }

    /// Simpan secret. `Err` berisi peringatan jika hanya tersimpan di memori.
    pub fn set(profile_id: &str, value: &str, persist: bool) -> Result<(), String> {
        remember(session(), profile_id, Some(value.to_owned()));
        if !persist {
            return Ok(());
        }
        keyring::Entry::new(SERVICE, profile_id)
            .and_then(|entry| entry.set_password(value))
            .map_err(|e| {
                tf(
                    "Keychain tidak tersedia ({0}). Secret hanya disimpan selama sesi ini.",
                    &[&e],
                )
            })
    }

    pub fn delete(profile_id: &str) {
        remember(session(), profile_id, None);
        if let Ok(entry) = keyring::Entry::new(SERVICE, profile_id) {
            let _ = entry.delete_credential();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::Cell;

        fn counting<'a>(
            calls: &'a Cell<u32>,
            value: Option<&'a str>,
        ) -> impl FnOnce() -> Option<String> + 'a {
            move || {
                calls.set(calls.get() + 1);
                value.map(str::to_owned)
            }
        }

        #[test]
        fn hasil_positif_dicache() {
            let cache = Mutex::default();
            let calls = Cell::new(0);
            assert_eq!(
                lookup(&cache, "a", counting(&calls, Some("pw"))).as_deref(),
                Some("pw")
            );
            assert_eq!(
                lookup(&cache, "a", counting(&calls, None)).as_deref(),
                Some("pw")
            );
            assert_eq!(calls.get(), 1);
        }

        #[test]
        fn hasil_negatif_dicache() {
            let cache = Mutex::default();
            let calls = Cell::new(0);
            assert_eq!(lookup(&cache, "a", counting(&calls, None)), None);
            assert_eq!(lookup(&cache, "a", counting(&calls, Some("pw"))), None);
            assert_eq!(calls.get(), 1);
        }

        #[test]
        fn set_dan_delete_memperbarui_cache() {
            let cache = Mutex::default();
            let calls = Cell::new(0);
            assert_eq!(lookup(&cache, "a", counting(&calls, None)), None);
            remember(&cache, "a", Some("baru".into()));
            assert_eq!(
                lookup(&cache, "a", counting(&calls, None)).as_deref(),
                Some("baru")
            );
            remember(&cache, "a", None);
            assert_eq!(lookup(&cache, "a", counting(&calls, Some("lama"))), None);
            assert_eq!(calls.get(), 1);
        }

        #[test]
        fn set_selama_baca_keychain_tidak_tertimpa() {
            let cache = Mutex::default();
            // Backend lambat: `set` (lewat `remember`) terjadi sebelum bacaan lama selesai.
            let got = lookup(&cache, "a", || {
                remember(&cache, "a", Some("baru".into()));
                Some("lama".into())
            });
            assert_eq!(got.as_deref(), Some("baru"));
            assert_eq!(
                lookup(&cache, "a", || panic!("tidak boleh ke keychain")).as_deref(),
                Some("baru")
            );
        }

        #[test]
        fn delete_selama_baca_keychain_tidak_dibatalkan() {
            let cache = Mutex::default();
            let got = lookup(&cache, "a", || {
                remember(&cache, "a", None);
                Some("lama".into())
            });
            assert_eq!(got, None);
            assert_eq!(
                lookup(&cache, "a", || panic!("tidak boleh ke keychain")),
                None
            );
        }

        #[test]
        fn profile_lain_tidak_tercampur() {
            let cache = Mutex::default();
            let calls = Cell::new(0);
            remember(&cache, "a", Some("pw".into()));
            assert_eq!(lookup(&cache, "b", counting(&calls, None)), None);
            assert_eq!(calls.get(), 1);
        }
    }
}

/// X-05: terjemahan English untuk teks di file ini.
pub const EN: &[(&str, &str)] = &[
    ("lainnya", "other"),
    ("Tanpa auth", "No auth"),
    ("Cluster baru", "New cluster"),
    (
        "Keychain tidak tersedia ({0}). Secret hanya disimpan selama sesi ini.",
        "Keychain unavailable ({0}). The secret is only kept for this session.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("elastic-gui-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store { dir }
    }

    #[test]
    fn semua_teks_diterjemahkan() {
        crate::i18n::assert_translated(include_str!("profile.rs"), EN);
    }

    #[test]
    fn settings_tanpa_file_memakai_bawaan() {
        let store = temp_store("settings-default");
        assert_eq!(store.load_settings(), AppSettings::default());
        assert_eq!(store.load_settings().lang, Lang::Id);
    }

    #[test]
    fn settings_tersimpan_dan_dimuat_ulang() {
        let store = temp_store("settings-roundtrip");
        let settings = AppSettings { lang: Lang::En };
        store.save_settings(&settings).unwrap();
        let text = std::fs::read_to_string(store.dir.join("settings.json")).unwrap();
        assert!(text.contains("\"en\""), "{text}");
        assert_eq!(store.load_settings(), settings);
        let _ = std::fs::remove_dir_all(&store.dir);
    }
}
