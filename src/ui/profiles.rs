//! Modul Config: CRUD profile cluster + test connection.

use std::sync::Arc;

use eframe::egui::{self, RichText};

use crate::es::Conn;
use crate::profile::{AuthKind, Env, Profile, Store, secret};
use crate::task::{Pending, poll};
use crate::widgets::{self, Confirm};

pub enum Event {
    /// Profile disimpan/dihapus: workspace profile ini harus dibuat ulang.
    Changed(String),
    Activate(String),
}

#[derive(Default)]
pub struct Profiles {
    selected: Option<String>,
    draft: Option<Profile>,
    secret_input: String,
    test: Option<Pending<Result<String, String>>>,
    test_result: Option<Result<String, String>>,
    message: Option<Result<String, String>>,
    confirm_delete: bool,
    confirm_typed: String,
    cloud_id: String,
}

impl Profiles {
    pub fn has_selection(&self) -> bool {
        self.draft.is_some()
    }

    pub fn select(&mut self, id: &str, profiles: &[Profile]) {
        self.selected = Some(id.to_owned());
        self.draft = profiles.iter().find(|p| p.id == id).cloned();
        self.secret_input.clear();
        self.test_result = None;
        self.message = None;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        profiles: &mut Vec<Profile>,
        store: &Store,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(result) = poll(&mut self.test) {
            self.test_result = Some(result);
        }
        egui::Panel::left("profile_list")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                ui.heading("Cluster");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Ekspor…")
                        .on_hover_text("Tanpa password/API key")
                        .clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_file_name("elastic-gui-profiles.json")
                            .save_file()
                    {
                        let list: Vec<&Profile> =
                            profiles.iter().filter(|p| !p.ephemeral).collect();
                        let text = serde_json::to_string_pretty(&list).unwrap_or_default();
                        self.message = Some(
                            std::fs::write(&path, text)
                                .map(|_| format!("{} profile diekspor (tanpa secret).", list.len()))
                                .map_err(|e| e.to_string()),
                        );
                    }
                    if ui.button("Impor…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("JSON", &["json"])
                            .pick_file()
                    {
                        self.message = Some(import_profiles(&path, profiles, store));
                    }
                });
                if ui.button("+ Profile baru").clicked() {
                    let profile = Profile::default();
                    self.selected = Some(profile.id.clone());
                    self.draft = Some(profile);
                    self.secret_input.clear();
                    self.test_result = None;
                    self.message = None;
                }
                ui.separator();
                let mut pick = None;
                for p in profiles.iter() {
                    let selected = self.selected.as_deref() == Some(p.id.as_str());
                    let label = format!("{}  ·  {}", p.name, p.env.label());
                    let mut text = RichText::new(label);
                    if p.is_prod() {
                        text = text.color(ui.visuals().error_fg_color);
                    }
                    if ui.selectable_label(selected, text).clicked() {
                        pick = Some(p.id.clone());
                    }
                }
                if let Some(id) = pick {
                    self.select(&id, profiles);
                }
            });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .show(ui, |ui| self.editor(ui, profiles, store, &mut events));
        });
        events
    }

    fn editor(
        &mut self,
        ui: &mut egui::Ui,
        profiles: &mut Vec<Profile>,
        store: &Store,
        events: &mut Vec<Event>,
    ) {
        let Some(draft) = &mut self.draft else {
            ui.weak("Pilih profile di kiri atau buat profile baru.");
            return;
        };
        let is_new = !profiles.iter().any(|p| p.id == draft.id);
        ui.heading(if is_new {
            "Profile baru"
        } else {
            "Edit profile"
        });
        if draft.ephemeral {
            widgets::warn_box(
                ui,
                "Profile ini berasal dari environment variable (ELASTIC_URL) dan tidak disimpan. \
                 Klik \"Simpan\" untuk menjadikannya profile permanen.",
            );
        }
        ui.add_space(6.0);
        egui::Grid::new("profile_form")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Nama");
                ui.text_edit_singleline(&mut draft.name);
                ui.end_row();

                ui.label("Environment");
                ui.horizontal(|ui| {
                    for env in Env::ALL {
                        ui.selectable_value(&mut draft.env, env, env.label());
                    }
                });
                ui.end_row();

                ui.label("URL node");
                ui.vertical(|ui| {
                    let mut remove = None;
                    for (i, url) in draft.urls.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(url)
                                    .desired_width(360.0)
                                    .hint_text("https://host:9200"),
                            );
                            if ui.small_button("×").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        draft.urls.remove(i);
                    }
                    if ui
                        .small_button("+ node")
                        .on_hover_text("Node cadangan untuk failover")
                        .clicked()
                    {
                        draft.urls.push(String::new());
                    }
                });
                ui.end_row();

                ui.label("Autentikasi");
                egui::ComboBox::from_id_salt("auth_kind")
                    .selected_text(draft.auth.label())
                    .show_ui(ui, |ui| {
                        for kind in AuthKind::ALL {
                            ui.selectable_value(&mut draft.auth, kind, kind.label());
                        }
                    });
                ui.end_row();

                if draft.auth == AuthKind::Basic {
                    ui.label("Username");
                    ui.text_edit_singleline(&mut draft.username);
                    ui.end_row();
                }
                if draft.auth != AuthKind::None {
                    let label = match draft.auth {
                        AuthKind::Basic => "Password",
                        AuthKind::ApiKey => "API key",
                        _ => "Token",
                    };
                    ui.label(label);
                    let has_secret = secret::get(&draft.id).is_some();
                    let hint = if has_secret {
                        "•••••• (tersimpan, isi untuk mengganti)"
                    } else {
                        ""
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut self.secret_input)
                            .password(true)
                            .desired_width(360.0)
                            .hint_text(hint),
                    );
                    ui.end_row();
                }

                ui.label("CA cert (PEM)");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.ca_cert_path)
                            .desired_width(300.0)
                            .hint_text("kosong = CA sistem"),
                    );
                    if ui.button("Pilih…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("PEM", &["pem", "crt", "cer"])
                            .pick_file()
                    {
                        draft.ca_cert_path = path.display().to_string();
                    }
                });
                ui.end_row();

                ui.label("Fingerprint CA");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.ca_fingerprint)
                        .desired_width(360.0)
                        .hint_text("SHA-256, mis. dari output setup ES (opsional)"),
                )
                .on_hover_text("Jika diisi, sertifikat server diterima hanya bila fingerprint-nya cocok (menggantikan CA cert).");
                ui.end_row();

                ui.label("Client cert (mTLS)");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut draft.client_cert_path).desired_width(240.0).hint_text("cert PEM"));
                    if ui.button("Pilih…").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        draft.client_cert_path = path.display().to_string();
                    }
                });
                ui.end_row();

                ui.label("Client key (mTLS)");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut draft.client_key_path).desired_width(240.0).hint_text("private key PEM"));
                    if ui.button("Pilih…").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        draft.client_key_path = path.display().to_string();
                    }
                });
                ui.end_row();

                ui.label("Cloud ID");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.cloud_id).desired_width(300.0).hint_text("deployment:base64…"));
                    if ui.button("Isi URL").clicked() {
                        match crate::es::tls::decode_cloud_id(&self.cloud_id) {
                            Ok(url) => {
                                draft.urls = vec![url];
                                self.message = None;
                            }
                            Err(err) => self.message = Some(Err(err)),
                        }
                    }
                });
                ui.end_row();

                ui.label("TLS");
                ui.checkbox(
                    &mut draft.insecure,
                    "Lewati verifikasi sertifikat (tidak aman)",
                );
                ui.end_row();

                ui.label("Timeout (detik)");
                ui.add(egui::DragValue::new(&mut draft.timeout_secs).range(1..=600));
                ui.end_row();

                ui.label("HTTP proxy");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.proxy)
                        .desired_width(360.0)
                        .hint_text("http://user:pass@proxy:3128 (kosong = proxy sistem)"),
                );
                ui.end_row();

                ui.label("Header tambahan");
                ui.vertical(|ui| {
                    let mut remove = None;
                    for (i, (name, value)) in draft.headers.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(name)
                                    .desired_width(140.0)
                                    .hint_text("Nama"),
                            );
                            ui.add(
                                egui::TextEdit::singleline(value)
                                    .desired_width(200.0)
                                    .hint_text("Nilai"),
                            );
                            if ui.small_button("×").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                    if let Some(i) = remove {
                        draft.headers.remove(i);
                    }
                    if ui.small_button("+ header").clicked() {
                        draft.headers.push((String::new(), String::new()));
                    }
                });
                ui.end_row();

                ui.label("Mode");
                ui.checkbox(
                    &mut draft.read_only,
                    "Read-only (blokir semua request yang mengubah data)",
                );
                ui.end_row();
            });

        if draft.insecure {
            widgets::warn_box(
                ui,
                "Verifikasi TLS dimatikan. Koneksi rentan disadap (MITM).",
            );
        }
        if !draft.ca_fingerprint.trim().is_empty()
            && let Err(err) = crate::es::tls::parse_fingerprint(&draft.ca_fingerprint)
        {
            widgets::warn_box(ui, &err);
        }
        ui.add_space(10.0);
        let secret_value = if self.secret_input.is_empty() {
            secret::get(&draft.id)
        } else {
            Some(self.secret_input.clone())
        };
        let mut duplicate = None;
        ui.horizontal(|ui| {
            if self.test.is_some() {
                ui.spinner();
            } else if ui.button("Test koneksi").clicked() {
                let profile = draft.clone();
                let secret_value = secret_value.clone();
                self.test = Some(Pending::spawn(ui.ctx(), move || {
                    test_connection(&profile, secret_value.as_deref())
                }));
            }
            if ui.button("Simpan").clicked() {
                if draft.ephemeral {
                    draft.ephemeral = false;
                    draft.id = Profile::default().id;
                }
                self.message = Some(save(
                    draft,
                    &self.secret_input,
                    secret_value.as_deref(),
                    profiles,
                    store,
                ));
                if self.message.as_ref().is_some_and(Result::is_ok) || draft.auth == AuthKind::None
                {
                    self.secret_input.clear();
                }
                self.selected = Some(draft.id.clone());
                events.push(Event::Changed(draft.id.clone()));
            }
            if !is_new && ui.button("Gunakan").clicked() {
                events.push(Event::Activate(draft.id.clone()));
            }
            if !is_new && ui.button("Duplikat").clicked() {
                let mut copy = draft.clone();
                copy.id = Profile::default().id;
                copy.name = format!("{} (salinan)", copy.name);
                copy.ephemeral = false;
                duplicate = Some(copy);
            }
            if !is_new && !draft.ephemeral && ui.button("Hapus").clicked() {
                self.confirm_delete = true;
                self.confirm_typed.clear();
            }
        });
        if let Some(copy) = duplicate {
            self.secret_input = secret_value.unwrap_or_default();
            self.selected = Some(copy.id.clone());
            self.draft = Some(copy);
            return;
        }
        match &self.test_result {
            Some(Ok(text)) => {
                ui.colored_label(egui::Color32::from_rgb(0x2e, 0xa0, 0x43), text);
            }
            Some(Err(err)) => widgets::error_box(ui, err),
            None => {}
        }
        match &self.message {
            Some(Ok(text)) => {
                ui.label(text);
            }
            Some(Err(err)) => widgets::warn_box(ui, err),
            None => {}
        }

        if self.confirm_delete {
            let Some(draft) = &self.draft else { return };
            let message = format!(
                "Profile \"{}\" dan secret-nya di keychain akan dihapus.",
                draft.name
            );
            match widgets::confirm_dialog(
                ui.ctx(),
                "Hapus profile?",
                &message,
                None,
                &mut self.confirm_typed,
                true,
            ) {
                Confirm::Yes => {
                    let id = draft.id.clone();
                    profiles.retain(|p| p.id != id);
                    secret::delete(&id);
                    if let Err(err) = store.save_profiles(profiles) {
                        self.message = Some(Err(err));
                    }
                    self.draft = None;
                    self.selected = None;
                    self.confirm_delete = false;
                    events.push(Event::Changed(id));
                }
                Confirm::No => self.confirm_delete = false,
                Confirm::Pending => {}
            }
        }
    }
}

fn save(
    draft: &Profile,
    typed_secret: &str,
    secret_value: Option<&str>,
    profiles: &mut Vec<Profile>,
    store: &Store,
) -> Result<String, String> {
    let mut profile = draft.clone();
    profile.urls.retain(|u| !u.trim().is_empty());
    profile.headers.retain(|(n, _)| !n.trim().is_empty());
    if profile.urls.is_empty() {
        return Err("Isi minimal satu URL node.".into());
    }
    match profiles.iter_mut().find(|p| p.id == profile.id) {
        Some(existing) => *existing = profile.clone(),
        None => profiles.push(profile.clone()),
    }
    store.save_profiles(profiles)?;
    // Secret baru diketik, atau secret dari env yang dijadikan permanen.
    let value = if typed_secret.is_empty() {
        secret_value
    } else {
        Some(typed_secret)
    };
    if profile.auth != AuthKind::None
        && let Some(value) = value
    {
        secret::set(&profile.id, value, true)?;
    }
    Ok("Profile disimpan. Secret disimpan di keychain OS.".into())
}

/// PRF-16: impor profile dari file ekspor. Selalu diberi id baru; secret diisi ulang manual.
fn import_profiles(
    path: &std::path::Path,
    profiles: &mut Vec<Profile>,
    store: &Store,
) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let imported: Vec<Profile> =
        serde_json::from_str(&text).map_err(|e| format!("File tidak valid: {e}"))?;
    let n = imported.len();
    for mut p in imported {
        p.id = Profile::default().id;
        p.ephemeral = false;
        // Hindari id kembar saat impor beruntun dalam nanodetik yang sama.
        while profiles.iter().any(|x| x.id == p.id) {
            p.id.push('x');
        }
        profiles.push(p);
    }
    store.save_profiles(profiles)?;
    Ok(format!(
        "{n} profile diimpor. Isi password/API key di masing-masing profile."
    ))
}

/// `GET /` + `_security/_authenticate`. Security nonaktif bukan error.
pub fn test_connection(profile: &Profile, secret: Option<&str>) -> Result<String, String> {
    let conn = Arc::new(Conn::new(profile, secret)?);
    let info = conn.get("/")?;
    let mut text = format!(
        "Terhubung: cluster {}, versi {}",
        info["cluster_name"].as_str().unwrap_or("?"),
        info["version"]["number"].as_str().unwrap_or("?")
    );
    if info["version"]["build_flavor"] == "serverless" {
        text.push_str(" (serverless)");
    }
    if let Ok(me) = conn.get("/_security/_authenticate") {
        let roles: Vec<&str> = me["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str())
            .collect();
        text.push_str(&format!(
            ", sebagai {} (roles: {})",
            me["username"].as_str().unwrap_or("?"),
            roles.join(", ")
        ));
    }
    Ok(text)
}
