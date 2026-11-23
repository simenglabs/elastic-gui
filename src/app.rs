//! Shell aplikasi: switcher cluster, navigasi modul, workspace per profile.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::es::Conn;
use crate::es::catalog::Catalog;
use crate::profile::{Profile, Store, secret};
use crate::task::{Pending, poll};
use crate::ui::{
    self, Ctx, Nav, console::Console, dashboard::Dashboard, discover::Discover, indices::Indices,
    profiles::Profiles,
};

const HEALTH_EVERY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Module {
    Discover,
    Console,
    Dashboard,
    Indices,
    Profiles,
}

impl Module {
    const ALL: [Module; 5] = [
        Module::Discover,
        Module::Console,
        Module::Dashboard,
        Module::Indices,
        Module::Profiles,
    ];

    fn label(self) -> &'static str {
        match self {
            Module::Discover => "Discover",
            Module::Console => "Dev Tools",
            Module::Dashboard => "Dashboard",
            Module::Indices => "Index",
            Module::Profiles => "Cluster",
        }
    }
}

struct ClusterInfo {
    name: String,
    version: String,
    caps: ui::Caps,
    user: Option<String>,
}

/// State semua modul untuk satu profile. Pindah cluster tidak membuang state.
struct Workspace {
    conn: Result<Arc<Conn>, String>,
    info: Option<Result<ClusterInfo, String>>,
    info_pending: Option<Pending<Result<ClusterInfo, String>>>,
    health: Option<Result<String, String>>,
    health_pending: Option<Pending<Result<String, String>>>,
    health_at: Option<Instant>,
    catalog: Catalog,
    console: Console,
    discover: Discover,
    indices: Indices,
    dashboard: Dashboard,
}

impl Workspace {
    fn new(profile: &Profile) -> Self {
        Self {
            conn: Conn::new(profile, secret::get(&profile.id).as_deref()).map(Arc::new),
            info: None,
            info_pending: None,
            health: None,
            health_pending: None,
            health_at: None,
            catalog: Catalog::default(),
            console: Console::default(),
            discover: Discover::default(),
            indices: Indices::default(),
            dashboard: Dashboard::default(),
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let Ok(conn) = &self.conn else { return };
        if self.info.is_none() && self.info_pending.is_none() {
            let conn = conn.clone();
            self.info_pending = Some(Pending::spawn(ctx, move || load_info(&conn)));
        }
        if let Some(info) = poll(&mut self.info_pending) {
            self.info = Some(info);
        }
        let serverless = matches!(&self.info, Some(Ok(info)) if info.caps.serverless);
        if !serverless
            && self.health_at.is_none_or(|t| t.elapsed() > HEALTH_EVERY)
            && self.health_pending.is_none()
        {
            self.health_at = Some(Instant::now());
            let conn = conn.clone();
            self.health_pending = Some(Pending::spawn(ctx, move || {
                conn.get("/_cluster/health")
                    .map(|h| h["status"].as_str().unwrap_or("unknown").to_owned())
            }));
        }
        if let Some(health) = poll(&mut self.health_pending) {
            self.health = Some(health);
        }
        self.catalog.poll();
        ctx.request_repaint_after(HEALTH_EVERY);
    }

    fn health_color(&self) -> Color32 {
        match &self.health {
            Some(Ok(s)) if s == "green" => Color32::from_rgb(0x2e, 0xa0, 0x43),
            Some(Ok(s)) if s == "yellow" => Color32::from_rgb(0xd4, 0xa7, 0x2c),
            Some(Ok(s)) if s == "red" => Color32::from_rgb(0xd0, 0x3b, 0x3b),
            Some(Err(_)) => Color32::from_rgb(0xd0, 0x3b, 0x3b),
            _ => Color32::GRAY,
        }
    }
}

fn load_info(conn: &Conn) -> Result<ClusterInfo, String> {
    let root = conn.get("/")?;
    let version = root["version"]["number"].as_str().unwrap_or("0").to_owned();
    let caps = ui::Caps::from_version(
        &version,
        root["version"]["build_flavor"].as_str().unwrap_or_default(),
    );
    let user = conn
        .get("/_security/_authenticate")
        .ok()
        .and_then(|me| me["username"].as_str().map(str::to_owned));
    Ok(ClusterInfo {
        name: root["cluster_name"].as_str().unwrap_or("?").to_owned(),
        version,
        caps,
        user,
    })
}

/// NFR: pengukuran startup opsional lewat `ELASTIC_GUI_TIMING=1`.
pub mod timing {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    static DONE: AtomicBool = AtomicBool::new(false);

    fn enabled() -> bool {
        std::env::var("ELASTIC_GUI_TIMING").is_ok_and(|v| v == "1")
    }

    pub fn start() {
        if enabled() {
            let _ = START.set(Instant::now());
        }
    }

    /// Dicetak sekali, saat guard dilepas di akhir frame pertama.
    pub struct FirstFrame;

    impl Drop for FirstFrame {
        fn drop(&mut self) {
            let Some(start) = START.get() else { return };
            if DONE.swap(true, Ordering::Relaxed) {
                return;
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let rss = rss_kb().map_or("n/a".to_string(), |kb| format!("{} MB", kb / 1024));
            eprintln!("[timing] frame pertama selesai: {ms:.0} ms dari main(); RSS: {rss}");
        }
    }

    // `ps` hanya ada di macOS/Linux; di Windows dilewati.
    fn rss_kb() -> Option<u64> {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
}

pub struct DesktopApp {
    store: Store,
    profiles: Vec<Profile>,
    active: Option<String>,
    module: Module,
    workspaces: HashMap<String, Workspace>,
    profile_ui: Profiles,
    /// PRF-15: tab terbuka (profile, modul) untuk berpindah cepat antar cluster.
    tabs: Vec<(String, Module)>,
    /// X-04: command palette (teks cari, pilihan).
    palette: Option<(String, usize)>,
}

/// Perintah command palette.
#[derive(Clone)]
enum Command {
    Module(Module),
    Profile(String),
    Discover(String),
    Theme,
}

impl DesktopApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let store = Store::open();
        let mut profiles = store.load_profiles();
        if let Some((env_profile, env_secret)) = Profile::from_env() {
            if let Some(value) = env_secret {
                let _ = secret::set(&env_profile.id, &value, false);
            }
            profiles.insert(0, env_profile);
        }
        let active = profiles.first().map(|p| p.id.clone());
        let module = if active.is_some() {
            Module::Discover
        } else {
            Module::Profiles
        };
        Self {
            store,
            profiles,
            active,
            module,
            workspaces: HashMap::new(),
            tabs: Vec::new(),
            palette: None,
            profile_ui: Profiles::default(),
        }
    }

    /// PRF-15: tab (profile + modul). State tiap profile tetap hidup di workspace-nya.
    fn tabs_ui(&mut self, ui: &mut egui::Ui) {
        self.tabs
            .retain(|(id, _)| self.profiles.iter().any(|p| &p.id == id));
        let mut pick = None;
        let mut close = None;
        for (i, (id, module)) in self.tabs.iter().enumerate() {
            let Some(p) = self.profiles.iter().find(|p| &p.id == id) else {
                continue;
            };
            let active = self.active.as_deref() == Some(id.as_str()) && self.module == *module;
            let mut text = RichText::new(format!("{} · {}", p.name, module.label()));
            if p.is_prod() {
                text = text.color(Color32::from_rgb(0xff, 0x8a, 0x80));
            }
            if ui.selectable_label(active, text).clicked() {
                pick = Some(i);
            }
            if ui.small_button("×").clicked() {
                close = Some(i);
            }
        }
        if let Some(id) = self.active.clone()
            && self.module != Module::Profiles
            && !self.tabs.contains(&(id.clone(), self.module))
            && ui
                .small_button("+ tab")
                .on_hover_text("Simpan cluster + modul ini sebagai tab")
                .clicked()
        {
            self.tabs.push((id, self.module));
        }
        if let Some(i) = pick {
            let (id, module) = self.tabs[i].clone();
            self.active = Some(id);
            self.module = module;
        }
        if let Some(i) = close {
            self.tabs.remove(i);
        }
    }

    /// X-06: profile aktif, cluster, health, dan request yang sedang berjalan.
    fn status_bar(&self, ui: &mut egui::Ui) {
        let Some(profile) = self
            .active
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|p| &p.id == id))
        else {
            return;
        };
        let ws = self.workspaces.get(&profile.id);
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let dot = ws.map_or(Color32::GRAY, Workspace::health_color);
                ui.label(RichText::new("●").color(dot));
                ui.small(format!("{} ({})", profile.name, profile.env.label()));
                if let Some(Ok(info)) = ws.and_then(|w| w.info.as_ref()) {
                    let flavor = if info.caps.serverless {
                        " · serverless"
                    } else {
                        ""
                    };
                    ui.small(format!("· {} · v{}{flavor}", info.name, info.version));
                }
                if let Some(Ok(health)) = ws.and_then(|w| w.health.as_ref()) {
                    ui.small(format!("· health {health}"));
                }
                if profile.read_only {
                    ui.small(RichText::new("· read-only").color(ui.visuals().warn_fg_color));
                }
                let running = ws
                    .and_then(|w| w.conn.as_ref().ok())
                    .map_or(0, |c| c.inflight());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if running > 0 {
                        ui.small(format!("{running} request berjalan"));
                        ui.spinner();
                        ui.ctx().request_repaint_after(Duration::from_millis(250));
                    } else {
                        ui.small(profile.urls.first().map(String::as_str).unwrap_or_default());
                    }
                });
            });
        });
    }

    /// X-04: Ctrl/Cmd+K untuk pindah modul, cluster, atau membuka index di Discover.
    fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some((query, selected)) = &mut self.palette else {
            return;
        };
        let mut commands: Vec<(String, Command)> = Module::ALL
            .iter()
            .map(|m| (format!("Buka {}", m.label()), Command::Module(*m)))
            .collect();
        commands.extend(self.profiles.iter().map(|p| {
            (
                format!("Cluster: {} ({})", p.name, p.env.label()),
                Command::Profile(p.id.clone()),
            )
        }));
        commands.push(("Ganti tema terang/gelap".into(), Command::Theme));
        if let Some(ws) = self
            .active
            .as_ref()
            .and_then(|id| self.workspaces.get_mut(id))
            && let Ok(conn) = ws.conn.clone()
        {
            for t in ws
                .catalog
                .targets(ctx, &conn)
                .iter()
                .filter(|t| !t.name.starts_with('.'))
                .take(2000)
            {
                commands.push((
                    format!("Discover: {} ({})", t.name, t.kind.label()),
                    Command::Discover(t.name.clone()),
                ));
            }
        }
        let needle = query.to_lowercase();
        let words: Vec<&str> = needle.split_whitespace().collect();
        let matches: Vec<&(String, Command)> = commands
            .iter()
            .filter(|(label, _)| {
                let l = label.to_lowercase();
                words.iter().all(|w| l.contains(w))
            })
            .take(12)
            .collect();
        *selected = (*selected).min(matches.len().saturating_sub(1));
        let mut chosen = None;
        let mut close = false;
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                *selected = (*selected + 1).min(matches.len().saturating_sub(1));
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                *selected = selected.saturating_sub(1);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                chosen = matches.get(*selected).map(|(_, c)| c.clone());
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                close = true;
            }
        });
        let response = egui::Modal::new(egui::Id::unique("command_palette")).show(ctx, |ui| {
            ui.set_width(520.0);
            let r = ui.add(
                egui::TextEdit::singleline(query)
                    .hint_text("Ketik perintah, cluster, atau nama index…")
                    .desired_width(f32::INFINITY),
            );
            r.request_focus();
            for (i, (label, command)) in matches.iter().enumerate() {
                if ui.selectable_label(i == *selected, label).clicked() {
                    chosen = Some(command.clone());
                }
            }
            if matches.is_empty() {
                ui.weak("Tidak ada yang cocok.");
            }
            ui.weak("↑↓ pilih · Enter jalankan · Esc tutup");
        });
        if response.should_close() {
            close = true;
        }
        if let Some(command) = chosen {
            close = true;
            match command {
                Command::Module(m) => self.module = m,
                Command::Profile(id) => self.active = Some(id),
                Command::Theme => {
                    let dark = ctx.global_style().visuals.dark_mode;
                    ctx.set_visuals(if dark {
                        egui::Visuals::light()
                    } else {
                        egui::Visuals::dark()
                    });
                }
                Command::Discover(pattern) => {
                    if let Some(ws) = self
                        .active
                        .as_ref()
                        .and_then(|id| self.workspaces.get_mut(id))
                    {
                        ws.discover.open_pattern(&pattern);
                    }
                    self.module = Module::Discover;
                }
            }
        }
        if close {
            self.palette = None;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let active_profile = self
            .active
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|p| &p.id == id))
            .cloned();
        let prod = active_profile.as_ref().is_some_and(Profile::is_prod);
        let mut frame = egui::Frame::side_top_panel(ui.style());
        if prod {
            frame = frame.fill(Color32::from_rgb(0x5a, 0x1d, 0x1d));
        }
        egui::Panel::top("top_bar").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Elastic GUI");
                ui.separator();
                let current = active_profile
                    .as_ref()
                    .map_or("Pilih cluster".to_owned(), |p| p.name.clone());
                let mut switch_to = None;
                egui::ComboBox::from_id_salt("cluster_switcher")
                    .selected_text(current)
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for p in &self.profiles {
                            let dot = self
                                .workspaces
                                .get(&p.id)
                                .map_or(Color32::GRAY, Workspace::health_color);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").color(dot));
                                if ui
                                    .selectable_label(
                                        self.active.as_deref() == Some(p.id.as_str()),
                                        format!("{}  ·  {}", p.name, p.env.label()),
                                    )
                                    .clicked()
                                {
                                    switch_to = Some(p.id.clone());
                                }
                            });
                        }
                    });
                if let Some(id) = switch_to {
                    self.active = Some(id);
                }
                if let Some(profile) = &active_profile {
                    let ws = self.workspaces.get(&profile.id);
                    let dot = ws.map_or(Color32::GRAY, Workspace::health_color);
                    let health = ws
                        .and_then(|w| w.health.as_ref())
                        .map_or("…".to_owned(), |h| match h {
                            Ok(s) => s.clone(),
                            Err(e) => e.clone(),
                        });
                    ui.label(RichText::new("●").color(dot))
                        .on_hover_text(format!("Health: {health}"));
                    let env = RichText::new(profile.env.label().to_uppercase()).strong();
                    ui.label(if prod {
                        env.color(Color32::from_rgb(0xff, 0x8a, 0x80))
                    } else {
                        env
                    });
                    match ws.and_then(|w| w.info.as_ref()) {
                        Some(Ok(info)) => {
                            let user = info
                                .user
                                .as_deref()
                                .map(|u| format!(" · {u}"))
                                .unwrap_or_default();
                            ui.weak(format!("{} · v{}{user}", info.name, info.version));
                        }
                        Some(Err(err)) => {
                            ui.colored_label(ui.visuals().error_fg_color, "Tidak terhubung")
                                .on_hover_text(err);
                        }
                        None => {
                            ui.spinner();
                        }
                    }
                    if profile.read_only {
                        ui.label(
                            RichText::new("READ-ONLY")
                                .color(ui.visuals().warn_fg_color)
                                .strong(),
                        );
                    }
                    if profile.insecure {
                        ui.label(
                            RichText::new("TLS TIDAK DIVERIFIKASI")
                                .color(ui.visuals().warn_fg_color),
                        )
                        .on_hover_text("Verifikasi sertifikat dimatikan di profile ini.");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dark = ui.visuals().dark_mode;
                    if ui
                        .small_button(if dark { "☀" } else { "🌙" })
                        .on_hover_text("Ganti tema")
                        .clicked()
                    {
                        ui.ctx().set_visuals(if dark {
                            egui::Visuals::light()
                        } else {
                            egui::Visuals::dark()
                        });
                    }
                });
            });
            ui.horizontal(|ui| {
                for module in Module::ALL {
                    ui.selectable_value(&mut self.module, module, module.label());
                }
                ui.separator();
                self.tabs_ui(ui);
            });
        });
    }
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let _first_frame = timing::FirstFrame;
        let ctx = ui.ctx().clone();
        for ws in self.workspaces.values_mut() {
            ws.tick(&ctx);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette = Some((String::new(), 0));
        }
        self.palette_ui(&ctx);
        self.top_bar(ui);
        self.status_bar(ui);

        if self.module == Module::Profiles {
            egui::CentralPanel::default().show(ui, |ui| {
                if !self.profile_ui.has_selection()
                    && let Some(id) = self.active.clone()
                {
                    self.profile_ui.select(&id, &self.profiles);
                }
                for event in self.profile_ui.ui(ui, &mut self.profiles, &self.store) {
                    match event {
                        ui::profiles::Event::Changed(id) => {
                            self.workspaces.remove(&id);
                            if !self.profiles.iter().any(|p| p.id == id) {
                                if self.active.as_deref() == Some(id.as_str()) {
                                    self.active = self.profiles.first().map(|p| p.id.clone());
                                }
                            } else if self.active.is_none() {
                                self.active = Some(id);
                            }
                        }
                        ui::profiles::Event::Activate(id) => {
                            self.active = Some(id);
                            self.module = Module::Discover;
                        }
                    }
                }
            });
            return;
        }

        let Some(profile) = self
            .active
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|p| &p.id == id))
            .cloned()
        else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Belum ada cluster");
                ui.label("Buat profile cluster di menu Cluster terlebih dahulu.");
                if ui.button("Buat profile").clicked() {
                    self.module = Module::Profiles;
                }
            });
            return;
        };
        let ws = self
            .workspaces
            .entry(profile.id.clone())
            .or_insert_with(|| {
                let mut ws = Workspace::new(&profile);
                ws.tick(&ctx);
                ws
            });
        let conn = match &ws.conn {
            Ok(conn) => conn.clone(),
            Err(err) => {
                let err = err.clone();
                egui::CentralPanel::default().show(ui, |ui| {
                    crate::widgets::error_box(ui, &format!("Profile tidak bisa dipakai: {err}"));
                    if ui.button("Edit profile").clicked() {
                        self.module = Module::Profiles;
                    }
                });
                return;
            }
        };
        if let Some(Err(err)) = &ws.info {
            let err = err.clone();
            egui::Panel::top("conn_error").show(ui, |ui| {
                ui.horizontal(|ui| {
                    crate::widgets::error_box(ui, &format!("Gagal terhubung: {err}"));
                });
                ui.horizontal(|ui| {
                    if ui.button("Coba lagi").clicked() {
                        ws.info = None;
                        ws.health_at = None;
                    }
                    if err.contains("401") && ui.button("Edit profile").clicked() {
                        self.module = Module::Profiles;
                    }
                });
            });
        }
        let caps = match &ws.info {
            Some(Ok(info)) => info.caps,
            _ => ui::Caps::default(),
        };
        let mut nav = None;
        let mut cx = Ctx {
            conn: &conn,
            profile: &profile,
            catalog: &mut ws.catalog,
            caps,
            store: &self.store,
            profiles: &self.profiles,
            nav: &mut nav,
        };
        egui::CentralPanel::default().show(ui, |ui| match self.module {
            Module::Console => ws.console.ui(ui, &mut cx),
            Module::Discover => ws.discover.ui(ui, &mut cx),
            Module::Indices => ws.indices.ui(ui, &mut cx),
            Module::Dashboard => ws.dashboard.ui(ui, &mut cx),
            Module::Profiles => {}
        });
        match nav {
            Some(Nav::Console { method, path, body }) => {
                ws.console.append_request(&method, &path, body.as_ref());
                self.module = Module::Console;
            }
            Some(Nav::Discover { pattern }) => {
                ws.discover.open_pattern(&pattern);
                self.module = Module::Discover;
            }
            None => {}
        }
    }
}
