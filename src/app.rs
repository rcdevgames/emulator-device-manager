use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, TextureHandle};

use crate::sdk::{self, Avd, Device, DeviceDef, Sdk, SystemImage};
use crate::settings::Settings;
use crate::tasks::{Level, Msg, Tasks};

const CARD: Color32 = Color32::from_rgb(32, 35, 41);
const CARD_SEL: Color32 = Color32::from_rgb(38, 56, 78);
const ACCENT: Color32 = Color32::from_rgb(122, 186, 255);
const GOOD: Color32 = Color32::from_rgb(120, 214, 150);
const WARN: Color32 = Color32::from_rgb(240, 190, 110);
const BAD: Color32 = Color32::from_rgb(240, 120, 120);
const MUTED: Color32 = Color32::from_rgb(146, 154, 166);
const PANEL: Color32 = Color32::from_rgb(18, 20, 23);
const WELL: Color32 = Color32::from_rgb(14, 16, 18);

#[derive(Clone, PartialEq)]
enum Target {
    Avd(String),
    Device(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Overview,
    Screen,
    Logcat,
    Install,
    Ports,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Overview, Tab::Screen, Tab::Logcat, Tab::Install, Tab::Ports];

    fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Screen => "Screen",
            Tab::Logcat => "Logcat",
            Tab::Install => "Install",
            Tab::Ports => "Ports",
        }
    }
}

struct CreateForm {
    name: String,
    device_id: String,
    image: String,
    sdcard: String,
    error: String,
}

struct EditForm {
    avd: Avd,
    ram: String,
    cores: String,
    width: String,
    height: String,
    density: String,
    gpu: String,
    sdcard: String,
    data: String,
}

enum Act {
    Select(Target),
    Refresh,
    NewAvd,
    Start {
        name: String,
        cold: bool,
        wipe: bool,
    },
    Stop {
        name: String,
    },
    ConfirmDelete {
        name: String,
    },
    Delete {
        name: String,
    },
    Edit {
        name: String,
    },
    SaveEdit,
    Reveal(PathBuf),
    Screenshot,
    SaveShot,
    Logcat,
    ClearLogcat,
    PickApk,
    Install,
    AddForward,
    RemoveForward,
    ListForwards,
    ListSnapshots,
    Settings,
}

pub struct App {
    sdk: Sdk,
    settings: Settings,
    tasks: Tasks,
    busy: usize,

    avds: Vec<Avd>,
    devices: Vec<Device>,
    running: BTreeMap<String, String>,
    images: Vec<SystemImage>,
    defs: Vec<DeviceDef>,

    target: Option<Target>,
    tab: Tab,
    search: String,

    create: Option<CreateForm>,
    edit: Option<EditForm>,
    deleting: Option<String>,
    settings_open: bool,
    settings_draft: Settings,

    logcat: String,
    logcat_filter: String,
    logcat_lines: usize,
    logcat_live: bool,

    screen_tex: Option<TextureHandle>,
    screen_raw: Vec<u8>,
    screen_live: bool,

    forwards: String,
    snapshots: String,
    port_spec: String,
    port_reverse: bool,

    apk: String,
    reinstall: bool,
    headless: bool,

    log: Vec<(Level, String)>,
    /// Guard each self-scheduling worker so slow tasks never stack up.
    pending_refresh: bool,
    pending_poll: bool,
    pending_live: bool,
    last_poll: Instant,
    last_live: Instant,
    log_open: bool,
    frame_times: Vec<f32>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let ctx = cc.egui_ctx.clone();
        ctx.set_theme(egui::ThemePreference::Dark);
        ctx.all_styles_mut(|style| {
            style.visuals.panel_fill = PANEL;
            style.visuals.window_fill = Color32::from_rgb(28, 31, 36);
            style.visuals.extreme_bg_color = WELL;
            style.visuals.selection.bg_fill = Color32::from_rgb(52, 84, 120);
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
        });

        let settings = Settings::load();
        let sdk = if settings.sdk_override.trim().is_empty() {
            Sdk::discover()
        } else {
            Sdk::from_root(PathBuf::from(settings.sdk_override.trim()))
        };

        let mut app = App {
            sdk,
            settings_draft: settings.clone(),
            settings,
            tasks: Tasks::new(),
            busy: 0,
            avds: Vec::new(),
            devices: Vec::new(),
            running: BTreeMap::new(),
            images: Vec::new(),
            defs: Vec::new(),
            target: None,
            tab: Tab::Overview,
            search: String::new(),
            create: None,
            edit: None,
            deleting: None,
            settings_open: false,
            logcat: String::new(),
            logcat_filter: String::new(),
            logcat_lines: 400,
            logcat_live: true,
            screen_tex: None,
            screen_raw: Vec::new(),
            screen_live: false,
            forwards: String::new(),
            snapshots: String::new(),
            port_spec: String::new(),
            port_reverse: false,
            apk: String::new(),
            reinstall: true,
            headless: false,
            log: Vec::new(),
            pending_refresh: false,
            pending_poll: false,
            pending_live: false,
            last_poll: Instant::now() - Duration::from_secs(60),
            last_live: Instant::now(),
            log_open: true,
            frame_times: Vec::new(),
        };
        let missing = app.sdk.missing();
        for m in missing {
            app.log(Level::Err, format!("{m} tidak ditemukan — set path SDK di Settings"));
        }
        app.refresh();
        app
    }

    fn log(&mut self, level: Level, text: impl Into<String>) {
        self.log.push((level, text.into()));
        if self.log.len() > 300 {
            self.log.drain(..self.log.len() - 300);
        }
    }

    /// Reload everything (AVDs, images, devices, device definitions) off the UI thread.
    fn refresh(&mut self) {
        self.busy += 1;
        self.pending_refresh = true;
        let sdk = self.sdk.clone();
        let want_defs = self.defs.is_empty();
        self.tasks.spawn(move |tx| {
            let _ = tx.send(Msg::Avds(sdk::list_avds(&sdk)));
            let devices = sdk::list_devices(&sdk);
            let _ = tx.send(Msg::Running(sdk::running_map(&sdk, &devices)));
            let _ = tx.send(Msg::Devices(devices));
            let _ = tx.send(Msg::Images(sdk::list_images(&sdk)));
            // avdmanager is a JVM start (~4 s), so only run it once per session.
            if want_defs {
                let _ = tx.send(Msg::Defs(sdk::list_device_defs(&sdk)));
            }
            let _ = tx.send(Msg::Refreshed);
        });
    }

    /// Cheap device poll: runs on a worker and never blocks on long tasks.
    fn poll_devices(&mut self) {
        self.busy += 1;
        self.pending_poll = true;
        let sdk = self.sdk.clone();
        self.tasks.spawn(move |tx| {
            let devices = sdk::list_devices(&sdk);
            let _ = tx.send(Msg::Running(sdk::running_map(&sdk, &devices)));
            let _ = tx.send(Msg::Devices(devices));
            let _ = tx.send(Msg::Polled);
        });
    }

    fn serial_of(&self, t: &Target) -> Option<String> {
        match t {
            Target::Device(s) => Some(s.clone()),
            Target::Avd(n) => self.running.get(n).cloned(),
        }
    }

    fn target_serial(&self) -> Option<String> {
        self.target.as_ref().and_then(|t| self.serial_of(t))
    }

    fn avd(&self, name: &str) -> Option<&Avd> {
        self.avds.iter().find(|a| a.name == name)
    }

    fn pump(&mut self, ctx: &egui::Context) {
        for msg in self.tasks.drain() {
            match msg {
                Msg::Idle => self.busy = self.busy.saturating_sub(1),
                Msg::Log(level, text) => self.log(level, text),
                Msg::Avds(list) => {
                    self.avds = list;
                    if let Some(Target::Avd(n)) = &self.target {
                        if !self.avds.iter().any(|a| &a.name == n) {
                            self.target = None;
                        }
                    }
                    if self.target.is_none() {
                        self.target = self.avds.first().map(|a| Target::Avd(a.name.clone()));
                    }
                }
                Msg::Devices(list) => {
                    self.devices = list;
                    if let Some(Target::Device(s)) = &self.target {
                        if !self.devices.iter().any(|d| &d.serial == s) {
                            self.target = None;
                        }
                    }
                }
                Msg::Running(map) => self.running = map,
                Msg::Images(list) => {
                    self.images = list;
                    let available: Vec<String> =
                        self.images.iter().map(|i| i.package.clone()).collect();
                    if let Some(form) = &mut self.create {
                        if !available.contains(&form.image) {
                            form.image = available.first().cloned().unwrap_or_default();
                        }
                    }
                }
                Msg::Defs(list) => {
                    self.defs = list;
                    let fallback = self
                        .defs
                        .iter()
                        .find(|d| d.id == "pixel_7_pro")
                        .or_else(|| self.defs.first())
                        .map(|d| d.id.clone())
                        .unwrap_or_default();
                    if let Some(form) = &mut self.create {
                        if form.device_id.is_empty() {
                            form.device_id = fallback;
                        }
                    }
                }
                Msg::Screen(png) => match decode_png(&png) {
                    Ok(image) => {
                        match &mut self.screen_tex {
                            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                self.screen_tex = Some(ctx.load_texture(
                                    "screen",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                        }
                        self.screen_raw = png;
                    }
                    Err(e) => self.log(Level::Err, format!("screenshot tidak terbaca: {e}")),
                },
                Msg::Logcat(text) => self.logcat = text,
                Msg::Forwards(text) => self.forwards = text,
                Msg::Snapshots(text) => self.snapshots = text,
                // Long tasks ask for a reload; the UI only schedules the worker.
                Msg::Refresh => self.refresh(),
                Msg::Refreshed => self.pending_refresh = false,
                Msg::Polled => self.pending_poll = false,
                Msg::Lived => self.pending_live = false,
            }
        }
    }

    fn auto_refresh(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        // Gate on the cheap poll flag, not on `busy`: a running emulator start
        // holds `busy` for up to two minutes and used to freeze all refreshing.
        if !self.pending_poll && now.duration_since(self.last_poll) > Duration::from_secs(2) {
            self.last_poll = now;
            self.poll_devices();
        }
        let interval = Duration::from_millis(self.settings.live_ms.clamp(400, 5000));
        if !self.pending_live && now.duration_since(self.last_live) > interval {
            let want_logcat = self.tab == Tab::Logcat && self.logcat_live;
            let want_screen = self.tab == Tab::Screen && self.screen_live;
            if (want_logcat || want_screen) && self.target_serial().is_some() {
                self.last_live = now;
                if let Some(serial) = self.target_serial() {
                    let sdk = self.sdk.clone();
                    let filter = self.logcat_filter.clone();
                    let lines = self.logcat_lines;
                    self.busy += 1;
                    self.pending_live = true;
                    self.tasks.spawn(move |tx| {
                        if want_screen {
                            match sdk::screenshot(&sdk, &serial) {
                                Ok(png) => {
                                    let _ = tx.send(Msg::Screen(png));
                                }
                                Err(e) => {
                                    let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                                }
                            }
                        }
                        if want_logcat {
                            let text = sdk::logcat_tail(&sdk, &serial, lines, &filter);
                            let _ = tx.send(Msg::Logcat(text));
                        }
                        let _ = tx.send(Msg::Lived);
                    });
                }
            }
        }
        if self.busy > 0 {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    fn apply(&mut self, act: Act) {
        match act {
            Act::Refresh => {
                self.refresh();
                self.log(Level::Info, "refresh");
            }
            Act::Select(t) => {
                if self.target.as_ref() != Some(&t) {
                    self.target = Some(t);
                    self.logcat.clear();
                    self.forwards.clear();
                    self.snapshots.clear();
                }
            }
            Act::NewAvd => {
                let device_id = self
                    .defs
                    .iter()
                    .find(|d| d.id == "pixel_7_pro")
                    .or_else(|| self.defs.first())
                    .map(|d| d.id.clone())
                    .unwrap_or_default();
                let image = self
                    .images
                    .first()
                    .map(|i| i.package.clone())
                    .unwrap_or_default();
                let api = self
                    .images
                    .first()
                    .map(|i| i.api.clone())
                    .unwrap_or_else(|| "36".into());
                self.create = Some(CreateForm {
                    name: format!("Pixel_API_{api}"),
                    device_id,
                    image,
                    sdcard: "512M".into(),
                    error: String::new(),
                });
            }
            Act::Start { name, cold, wipe } => {
                let sdk = self.sdk.clone();
                let extra = self.settings.extra_args.clone();
                let headless = self.headless;
                self.log(
                    Level::Info,
                    format!(
                        "start {name}{}{}{}",
                        if cold { " · cold boot" } else { "" },
                        if wipe { " · wipe data" } else { "" },
                        if headless { " · headless" } else { "" }
                    ),
                );
                self.busy += 1;
                self.tasks.spawn(
                    move |tx| match sdk::launch(&sdk, &name, cold, wipe, headless, &extra) {
                        Ok(_) => {
                            let _ = tx.send(Msg::Log(Level::Ok, format!("emulator {name} dijalankan")));
                            for _ in 0..120 {
                                std::thread::sleep(Duration::from_secs(1));
                                let devices = sdk::list_devices(&sdk);
                                if devices.iter().any(|d| d.is_emulator) {
                                    let _ = tx.send(Msg::Running(sdk::running_map(&sdk, &devices)));
                                    let _ = tx.send(Msg::Devices(devices));
                                    break;
                                }
                            }
                            let _ = tx.send(Msg::Refresh);
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    },
                );
            }
            Act::Stop { name } => {
                let Some(serial) = self.running.get(&name).cloned() else {
                    self.log(Level::Err, format!("{name} tidak sedang berjalan"));
                    return;
                };
                let sdk = self.sdk.clone();
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    match sdk::kill_emulator(&sdk, &serial) {
                        Ok(_) => {
                            let _ = tx.send(Msg::Log(Level::Ok, format!("{name} dimatikan")));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1500));
                    let _ = tx.send(Msg::Refresh);
                });
            }
            Act::ConfirmDelete { name } => self.deleting = Some(name),
            Act::Delete { name } => {
                self.deleting = None;
                let sdk = self.sdk.clone();
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    match sdk::delete_avd(&sdk, &name) {
                        Ok(_) => {
                            let _ = tx.send(Msg::Log(Level::Ok, format!("AVD {name} dihapus")));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    }
                    let _ = tx.send(Msg::Refresh);
                });
            }
            Act::Edit { name } => {
                if let Some(avd) = self.avd(&name) {
                    self.edit = Some(EditForm {
                        ram: avd.ram.clone(),
                        cores: avd.cores.clone(),
                        width: avd.width.clone(),
                        height: avd.height.clone(),
                        density: avd.density.clone(),
                        gpu: if avd.gpu.is_empty() {
                            "auto".into()
                        } else {
                            avd.gpu.clone()
                        },
                        sdcard: avd.sdcard.clone(),
                        data: avd.data_partition.clone(),
                        avd: avd.clone(),
                    });
                }
            }
            Act::SaveEdit => {
                let Some(form) = self.edit.take() else { return };
                let mut avd = form.avd.clone();
                avd.set("hw.ramSize", form.ram.trim());
                avd.set("hw.cpu.ncore", form.cores.trim());
                avd.set("hw.lcd.width", form.width.trim());
                avd.set("hw.lcd.height", form.height.trim());
                avd.set("hw.lcd.density", form.density.trim());
                avd.set("hw.gpu.mode", form.gpu.trim());
                if !form.sdcard.trim().is_empty() {
                    avd.set("hw.sdCard", "yes");
                    avd.set("sdcard.size", form.sdcard.trim());
                }
                avd.set("disk.dataPartition.size", form.data.trim());
                match avd.save() {
                    Ok(_) => {
                        self.log(
                            Level::Ok,
                            format!(
                                "config {} disimpan · restart emulator untuk menerapkan",
                                avd.name
                            ),
                        );
                        self.refresh();
                    }
                    Err(e) => self.log(Level::Err, e.to_string()),
                }
            }
            Act::Reveal(path) => {
                if let Err(e) = crate::proc::reveal(&path) {
                    self.log(Level::Err, e.to_string());
                }
            }
            Act::Screenshot => {
                let Some(serial) = self.target_serial() else {
                    self.log(Level::Err, "target belum berjalan");
                    return;
                };
                let sdk = self.sdk.clone();
                self.busy += 1;
                self.tasks.spawn(move |tx| match sdk::screenshot(&sdk, &serial) {
                    Ok(png) => {
                        let _ = tx.send(Msg::Screen(png));
                        let _ = tx.send(Msg::Log(Level::Ok, "screenshot diambil".to_string()));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                    }
                });
            }
            Act::SaveShot => {
                if self.screen_raw.is_empty() {
                    self.log(Level::Err, "belum ada screenshot");
                    return;
                }
                match save_png(&self.screen_raw) {
                    Ok(path) => {
                        self.log(Level::Ok, format!("screenshot disimpan: {}", path.display()));
                        let _ = crate::proc::reveal(&path);
                    }
                    Err(e) => self.log(Level::Err, e.to_string()),
                }
            }
            Act::Logcat => {
                let Some(serial) = self.target_serial() else {
                    self.log(Level::Err, "target belum berjalan");
                    return;
                };
                let sdk = self.sdk.clone();
                let filter = self.logcat_filter.clone();
                let lines = self.logcat_lines;
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    let text = sdk::logcat_tail(&sdk, &serial, lines, &filter);
                    let _ = tx.send(Msg::Logcat(text));
                });
            }
            Act::ClearLogcat => {
                if let Some(serial) = self.target_serial() {
                    sdk::clear_logcat(&self.sdk, &serial);
                    self.logcat.clear();
                    self.log(Level::Info, "logcat dibersihkan");
                }
            }
            Act::PickApk => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Android package", &["apk", "apks", "xapk"])
                    .pick_file()
                {
                    self.apk = path.to_string_lossy().to_string();
                }
            }
            Act::Install => {
                let Some(serial) = self.target_serial() else {
                    self.log(Level::Err, "target belum berjalan");
                    return;
                };
                let apk = self.apk.trim().to_string();
                if apk.is_empty() {
                    self.log(Level::Err, "pilih file APK dulu");
                    return;
                }
                let sdk = self.sdk.clone();
                let reinstall = self.reinstall;
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    match sdk::install_apk(&sdk, &serial, Path::new(&apk), reinstall) {
                        Ok(out) => {
                            let _ = tx.send(Msg::Log(Level::Ok, format!("install: {}", out.trim())));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    }
                });
            }
            Act::AddForward => {
                let Some(serial) = self.target_serial() else {
                    self.log(Level::Err, "target belum berjalan");
                    return;
                };
                let spec = self.port_spec.trim().to_string();
                if spec.is_empty() {
                    self.log(Level::Err, "isi mapping, contoh: tcp:8080 tcp:8080");
                    return;
                }
                let sdk = self.sdk.clone();
                let reverse = self.port_reverse;
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    match sdk::forward(&sdk, &serial, &spec, reverse, false) {
                        Ok(_) => {
                            let _ = tx.send(Msg::Log(
                                Level::Ok,
                                format!("port mapping ditambah: {spec}"),
                            ));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    }
                    let list = sdk::list_forwards(&sdk, &serial);
                    let _ = tx.send(Msg::Forwards(list));
                });
            }
            Act::RemoveForward => {
                let Some(serial) = self.target_serial() else { return };
                let spec = self.port_spec.trim().to_string();
                if spec.is_empty() {
                    self.log(Level::Err, "isi mapping yang mau dihapus");
                    return;
                }
                let sdk = self.sdk.clone();
                let reverse = self.port_reverse;
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    match sdk::forward(&sdk, &serial, &spec, reverse, true) {
                        Ok(_) => {
                            let _ = tx.send(Msg::Log(Level::Ok, format!("mapping dihapus: {spec}")));
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                        }
                    }
                    let list = sdk::list_forwards(&sdk, &serial);
                    let _ = tx.send(Msg::Forwards(list));
                });
            }
            Act::ListForwards => {
                let Some(serial) = self.target_serial() else { return };
                let sdk = self.sdk.clone();
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    let list = sdk::list_forwards(&sdk, &serial);
                    let _ = tx.send(Msg::Forwards(list));
                });
            }
            Act::ListSnapshots => {
                let name = match &self.target {
                    Some(Target::Avd(n)) => n.clone(),
                    _ => return,
                };
                let Some(avd) = self.avd(&name).cloned() else { return };
                let sdk = self.sdk.clone();
                self.busy += 1;
                self.tasks.spawn(move |tx| {
                    let text = sdk::list_snapshots(&sdk, &avd);
                    let _ = tx.send(Msg::Snapshots(text));
                });
            }
            Act::Settings => {
                self.settings_draft = self.settings.clone();
                self.settings_open = true;
            }
        }
    }

    fn save_settings(&mut self) {
        self.settings = self.settings_draft.clone();
        self.settings.save();
        let root = self.settings.sdk_override.trim().to_string();
        self.sdk = if root.is_empty() {
            Sdk::discover()
        } else {
            Sdk::from_root(PathBuf::from(root))
        };
        let missing = self.sdk.missing();
        for m in missing {
            self.log(Level::Err, format!("{m} masih belum ditemukan"));
        }
        self.log(Level::Ok, "settings disimpan");
        self.refresh();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        self.auto_refresh(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            let is_apk = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("apk"))
                .unwrap_or(false);
            if is_apk {
                self.apk = path.to_string_lossy().to_string();
                self.tab = Tab::Install;
                self.log(Level::Info, format!("APK dipilih: {}", self.apk));
            }
        }

        let mut act: Option<Act> = None;
        self.top_bar(ui, &mut act);
        self.nav_panel(ui, &mut act);
        self.log_panel(ui);
        self.main_panel(ui, &mut act);
        self.modals(&ctx, &mut act);

        if let Some(a) = act {
            self.apply(a);
        }
    }
}

// --------------------------------------------------------------- top layout

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Device Manager").strong().size(17.0));
                ui.label(
                    RichText::new(short_path(&self.sdk.root))
                        .color(MUTED)
                        .size(11.0),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Settings").clicked() {
                        *act = Some(Act::Settings);
                    }
                    if ui.button("Refresh").clicked() {
                        *act = Some(Act::Refresh);
                    }
                    if ui.button("+ New AVD").clicked() {
                        *act = Some(Act::NewAvd);
                    }
                    if self.busy > 0 {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(5.0);
        });
    }

    fn nav_panel(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        egui::Panel::left("nav")
            .resizable(true)
            .default_size(340.0)
            .size_range(280.0..=520.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Virtual devices · {}", self.avds.len()))
                        .strong()
                        .size(12.5),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("cari AVD…")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                let needle = self.search.trim().to_lowercase();
                self.frame_times.push(ui.ctx().input(|i| i.stable_dt).min(1.0));
                if self.frame_times.len() > 240 {
                    self.frame_times.remove(0);
                }
                let mut picks: Vec<Target> = Vec::new();
                egui::ScrollArea::vertical()
                    .id_salt("avd-list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut shown = 0usize;
                        for avd in &self.avds {
                            if !needle.is_empty() && !avd.name.to_lowercase().contains(&needle) {
                                continue;
                            }
                            shown += 1;
                            let serial = self.running.get(&avd.name).cloned();
                            let selected = self.target == Some(Target::Avd(avd.name.clone()));
                            if avd_card(ui, avd, serial.as_deref(), selected) {
                                picks.push(Target::Avd(avd.name.clone()));
                            }
                        }
                        if shown == 0 {
                            ui.add_space(6.0);
                            ui.label(RichText::new("Belum ada AVD.").color(MUTED).size(11.5));
                        }

                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("Connected · {}", self.devices.len()))
                                .strong()
                                .size(12.5),
                        );
                        if self.devices.is_empty() {
                            ui.label(
                                RichText::new("tidak ada device terhubung")
                                    .color(MUTED)
                                    .size(11.0),
                            );
                        }
                        for dev in &self.devices {
                            let selected = self.target == Some(Target::Device(dev.serial.clone()));
                            if device_card(ui, dev, selected) {
                                picks.push(Target::Device(dev.serial.clone()));
                            }
                        }
                        ui.add_space(10.0);
                    });
                if let Some(p) = picks.into_iter().next() {
                    *act = Some(Act::Select(p));
                }
            });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("activity")
            .resizable(true)
            .default_size(if self.log_open { 130.0 } else { 28.0 })
            .min_size(28.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let arrow = if self.log_open { "v" } else { ">" };
                    if ui.button(format!("{arrow} Activity")).clicked() {
                        self.log_open = !self.log_open;
                    }
                    ui.label(
                        RichText::new(format!("{} task", self.busy))
                            .color(if self.busy > 0 { ACCENT } else { MUTED })
                            .size(11.0),
                    );
                    if !self.frame_times.is_empty() {
                        let mut sorted = self.frame_times.clone();
                        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let p50 = sorted[sorted.len() / 2] * 1000.0;
                        let p99 = sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)] * 1000.0;
                        ui.label(
                            RichText::new(format!(
                                "frame p50 {p50:.0} ms · p99 {p99:.0} ms · {} sampel",
                                sorted.len()
                            ))
                            .color(if p99 > 60.0 { WARN } else { MUTED })
                            .size(11.0),
                        );
                    }
                });
                if self.log_open {
                    egui::ScrollArea::vertical()
                        .id_salt("activity-scroll")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (level, text) in &self.log {
                                let color = match level {
                                    Level::Ok => GOOD,
                                    Level::Err => BAD,
                                    Level::Info => MUTED,
                                };
                                ui.label(RichText::new(text).color(color).size(11.5).monospace());
                            }
                        });
                }
            });
    }

    fn main_panel(&mut self, ui: &mut egui::Ui, act: &mut Option<Act>) {
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(target) = self.target.clone() else {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Pilih device di kiri, atau buat AVD baru.").color(MUTED));
                });
                return;
            };
            let serial = self.serial_of(&target);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let title = match &target {
                    Target::Avd(n) => n.clone(),
                    Target::Device(s) => s.clone(),
                };
                ui.label(RichText::new(title).strong().size(18.0));
                match &serial {
                    Some(s) => ui.label(RichText::new(format!("running · {s}")).color(GOOD).size(11.5)),
                    None => ui.label(RichText::new("stopped").color(MUTED).size(11.5)),
                };
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.checkbox(&mut self.headless, "headless");
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    if ui
                        .selectable_label(self.tab == tab, RichText::new(tab.label()).size(13.0))
                        .clicked()
                    {
                        self.tab = tab;
                    }
                }
            });
            ui.separator();

            egui::ScrollArea::vertical()
                .id_salt("tab-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Overview => self.tab_overview(ui, &target, serial.as_deref(), act),
                    Tab::Screen => self.tab_screen(ui, serial.as_deref(), act),
                    Tab::Logcat => self.tab_logcat(ui, serial.as_deref(), act),
                    Tab::Install => self.tab_install(ui, serial.as_deref(), act),
                    Tab::Ports => self.tab_ports(ui, serial.as_deref(), act),
                });
        });
    }
}

// ------------------------------------------------------------------- tabs

impl App {
    fn tab_overview(
        &mut self,
        ui: &mut egui::Ui,
        target: &Target,
        serial: Option<&str>,
        act: &mut Option<Act>,
    ) {
        match target {
            Target::Avd(name) => {
                let Some(avd) = self.avd(name).cloned() else { return };
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if serial.is_some() {
                        if ui.button("Stop").clicked() {
                            *act = Some(Act::Stop { name: avd.name.clone() });
                        }
                    } else {
                        if ui.button("Start").clicked() {
                            *act = Some(Act::Start {
                                name: avd.name.clone(),
                                cold: false,
                                wipe: false,
                            });
                        }
                        if ui.button("Cold boot").clicked() {
                            *act = Some(Act::Start {
                                name: avd.name.clone(),
                                cold: true,
                                wipe: false,
                            });
                        }
                        if ui.button("Wipe data").clicked() {
                            *act = Some(Act::Start {
                                name: avd.name.clone(),
                                cold: true,
                                wipe: true,
                            });
                        }
                    }
                    if ui.button("Edit config").clicked() {
                        *act = Some(Act::Edit { name: avd.name.clone() });
                    }
                    if ui.button("Open folder").clicked() {
                        *act = Some(Act::Reveal(avd.dir.clone()));
                    }
                    if ui.button(RichText::new("Delete").color(BAD)).clicked() {
                        *act = Some(Act::ConfirmDelete { name: avd.name.clone() });
                    }
                });
                ui.add_space(12.0);

                egui::Grid::new("avd-info")
                    .num_columns(2)
                    .spacing([18.0, 7.0])
                    .show(ui, |ui| {
                        kv(ui, "Status", if serial.is_some() { "running" } else { "stopped" });
                        kv(ui, "Target", &avd.target);
                        kv(ui, "Tag", &avd.tag);
                        kv(ui, "ABI", &avd.abi);
                        kv(ui, "Device profile", &avd.device);
                        kv(ui, "RAM", &format!("{} MB", dash(&avd.ram)));
                        kv(ui, "CPU cores", &avd.cores);
                        kv(ui, "Screen", &avd.screen());
                        kv(ui, "SD card", &avd.sdcard);
                        kv(ui, "Data partition", &avd.data_partition);
                        kv(ui, "GPU mode", &avd.gpu);
                        kv(ui, "Play Store", if avd.playstore { "yes" } else { "no" });
                        kv(ui, "Path", &avd.dir.display().to_string());
                    });

                ui.add_space(14.0);
                ui.collapsing("Snapshots", |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("List snapshots").clicked() {
                            *act = Some(Act::ListSnapshots);
                        }
                        ui.label(
                            RichText::new("butuh emulator dalam keadaan mati")
                                .color(MUTED)
                                .size(11.0),
                        );
                    });
                    if !self.snapshots.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&self.snapshots).monospace().size(11.0));
                    }
                });
            }
            Target::Device(serial) => {
                let Some(dev) = self.devices.iter().find(|d| &d.serial == serial).cloned() else {
                    return;
                };
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Screenshot").clicked() {
                        self.tab = Tab::Screen;
                        *act = Some(Act::Screenshot);
                    }
                    if ui.button("Logcat").clicked() {
                        self.tab = Tab::Logcat;
                        *act = Some(Act::Logcat);
                    }
                    if ui.button("Install APK").clicked() {
                        self.tab = Tab::Install;
                    }
                    if ui.button("Ports").clicked() {
                        self.tab = Tab::Ports;
                        *act = Some(Act::ListForwards);
                    }
                });
                ui.add_space(12.0);
                let avd = self
                    .running
                    .iter()
                    .find(|(_, s)| *s == &dev.serial)
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default();
                egui::Grid::new("dev-info")
                    .num_columns(2)
                    .spacing([18.0, 7.0])
                    .show(ui, |ui| {
                        kv(ui, "Serial", &dev.serial);
                        kv(ui, "State", &dev.state);
                        kv(ui, "Model", &dev.model);
                        kv(ui, "Product", &dev.product);
                        kv(ui, "Transport id", &dev.transport);
                        kv(ui, "Kind", if dev.is_emulator { "emulator" } else { "physical" });
                        kv(ui, "AVD", &avd);
                    });
            }
        }
    }

    fn tab_screen(&mut self, ui: &mut egui::Ui, serial: Option<&str>, act: &mut Option<Act>) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let live = serial.is_some();
            if ui.add_enabled(live, egui::Button::new("Capture")).clicked() {
                *act = Some(Act::Screenshot);
            }
            ui.checkbox(&mut self.screen_live, "live");
            if ui
                .add_enabled(!self.screen_raw.is_empty(), egui::Button::new("Save PNG"))
                .clicked()
            {
                *act = Some(Act::SaveShot);
            }
            if let Some(tex) = &self.screen_tex {
                let s = tex.size();
                ui.label(
                    RichText::new(format!("{} x {}", s[0], s[1]))
                        .color(MUTED)
                        .size(11.0),
                );
            }
        });
        if serial.is_none() {
            ui.add_space(10.0);
            ui.label(RichText::new("Target belum berjalan — start dulu.").color(MUTED));
            return;
        }
        ui.add_space(8.0);
        if let Some(tex) = &self.screen_tex {
            let s = tex.size();
            let size = egui::vec2(s[0] as f32, s[1] as f32);
            let avail = ui.available_size();
            let scale = (avail.x / size.x).min(avail.y / size.y).min(1.0);
            let shown = (size * scale.max(0.05)).max(egui::vec2(64.0, 64.0));
            ui.image(egui::load::SizedTexture::new(tex.id(), shown));
        } else {
            ui.label(RichText::new("Belum ada frame. Klik Capture.").color(MUTED));
        }
    }

    fn tab_logcat(&mut self, ui: &mut egui::Ui, serial: Option<&str>, act: &mut Option<Act>) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let live = serial.is_some();
            if ui.add_enabled(live, egui::Button::new("Refresh")).clicked() {
                *act = Some(Act::Logcat);
            }
            ui.checkbox(&mut self.logcat_live, "live");
            if ui.add_enabled(live, egui::Button::new("Clear")).clicked() {
                *act = Some(Act::ClearLogcat);
            }
            ui.label("filter");
            ui.add(egui::TextEdit::singleline(&mut self.logcat_filter).desired_width(170.0));
            ui.label("baris");
            ui.add(egui::Slider::new(&mut self.logcat_lines, 50..=3000));
        });
        if serial.is_none() {
            ui.add_space(10.0);
            ui.label(RichText::new("Target belum berjalan — start dulu.").color(MUTED));
            return;
        }
        ui.add_space(8.0);
        egui::Frame::new()
            .fill(WELL)
            .inner_margin(egui::Margin::same(8))
            .corner_radius(CornerRadius::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(320.0);
                egui::ScrollArea::vertical()
                    .id_salt("logcat-scroll")
                    .stick_to_bottom(true)
                    .max_height(ui.available_height().max(320.0))
                    .show(ui, |ui| {
                        if self.logcat.is_empty() {
                            ui.label(RichText::new("(kosong)").color(MUTED).size(11.0));
                        } else {
                            ui.label(RichText::new(&self.logcat).monospace().size(11.0));
                        }
                    });
            });
    }

    fn tab_install(&mut self, ui: &mut egui::Ui, serial: Option<&str>, act: &mut Option<Act>) {
        ui.add_space(8.0);
        ui.label(RichText::new("Install APK").strong().size(13.5));
        ui.label(
            RichText::new("bisa juga drag & drop file .apk ke jendela ini")
                .color(MUTED)
                .size(11.0),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 110.0).max(160.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.apk)
                    .hint_text("C:\\path\\app.apk")
                    .desired_width(w),
            );
            if ui.button("Browse").clicked() {
                *act = Some(Act::PickApk);
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.reinstall, "reinstall (-r)");
            if ui
                .add_enabled(serial.is_some(), egui::Button::new("Install"))
                .clicked()
            {
                *act = Some(Act::Install);
            }
            if serial.is_none() {
                ui.label(RichText::new("target belum berjalan").color(MUTED).size(11.0));
            }
        });
    }

    fn tab_ports(&mut self, ui: &mut egui::Ui, serial: Option<&str>, act: &mut Option<Act>) {
        ui.add_space(8.0);
        ui.label(RichText::new("Port mapping").strong().size(13.5));
        ui.label(
            RichText::new(
                "forward = host ke device · reverse = device ke host · contoh: tcp:8080 tcp:8080",
            )
            .color(MUTED)
            .size(11.0),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.port_spec)
                    .hint_text("tcp:8080 tcp:8080")
                    .desired_width(250.0),
            );
            ui.radio_value(&mut self.port_reverse, false, "forward");
            ui.radio_value(&mut self.port_reverse, true, "reverse");
            let live = serial.is_some();
            if ui.add_enabled(live, egui::Button::new("Add")).clicked() {
                *act = Some(Act::AddForward);
            }
            if ui.add_enabled(live, egui::Button::new("Remove")).clicked() {
                *act = Some(Act::RemoveForward);
            }
            if ui.add_enabled(live, egui::Button::new("List")).clicked() {
                *act = Some(Act::ListForwards);
            }
        });
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(WELL)
            .inner_margin(egui::Margin::same(8))
            .corner_radius(CornerRadius::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(90.0);
                if self.forwards.is_empty() {
                    ui.label(
                        RichText::new("klik List untuk melihat mapping")
                            .color(MUTED)
                            .size(11.0),
                    );
                } else {
                    ui.label(RichText::new(&self.forwards).monospace().size(11.0));
                }
            });
    }
}

// ------------------------------------------------------------------ modals

impl App {
    fn modals(&mut self, ctx: &egui::Context, act: &mut Option<Act>) {
        self.create_modal(ctx);
        self.edit_modal(ctx, act);
        self.delete_modal(ctx, act);
        self.settings_modal(ctx);
    }

    fn create_modal(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.create.take() else { return };
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;

        egui::Window::new("Buat AVD baru")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                egui::Grid::new("create-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Nama");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.name)
                                .hint_text("Pixel_API_36")
                                .desired_width(320.0),
                        );
                        ui.end_row();

                        ui.label("Device profile");
                        let current = self
                            .defs
                            .iter()
                            .find(|d| d.id == form.device_id)
                            .map(|d| d.label())
                            .unwrap_or_else(|| "(default)".into());
                        egui::ComboBox::from_id_salt("device-def")
                            .selected_text(current)
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for def in &self.defs {
                                    ui.selectable_value(
                                        &mut form.device_id,
                                        def.id.clone(),
                                        def.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("System image");
                        let current = self
                            .images
                            .iter()
                            .find(|i| i.package == form.image)
                            .map(|i| i.label())
                            .unwrap_or_else(|| "(tidak ada image lokal)".into());
                        egui::ComboBox::from_id_salt("sys-image")
                            .selected_text(current)
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for img in &self.images {
                                    ui.selectable_value(
                                        &mut form.image,
                                        img.package.clone(),
                                        img.label(),
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("SD card");
                        ui.add(egui::TextEdit::singleline(&mut form.sdcard).desired_width(120.0));
                        ui.end_row();
                    });

                if self.images.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Tidak ada system image ter-download. Install dulu lewat SDK Manager di Android Studio.",
                        )
                        .color(WARN)
                        .size(11.0),
                    );
                }
                if !form.error.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&form.error).color(BAD).size(11.5));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ready = !form.name.trim().is_empty() && !form.image.is_empty();
                    if ui.add_enabled(ready, egui::Button::new("Buat")).clicked() {
                        submit = true;
                    }
                    if ui.button("Batal").clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel || !open {
            self.create = None;
            return;
        }
        if !submit {
            self.create = Some(form);
            return;
        }

        let name = form.name.trim().to_string();
        if name.contains(char::is_whitespace) {
            form.error = "nama tidak boleh mengandung spasi".into();
            self.create = Some(form);
            return;
        }
        if self.avds.iter().any(|a| a.name == name) {
            form.error = "nama AVD sudah dipakai".into();
            self.create = Some(form);
            return;
        }
        let draft = sdk::AvdDraft {
            name: name.clone(),
            device_id: form.device_id.clone(),
            image_package: form.image.clone(),
            sdcard: form.sdcard.clone(),
        };
        let sdk = self.sdk.clone();
        self.busy += 1;
        self.tasks.spawn(move |tx| {
            match sdk::create_avd(&sdk, &draft) {
                Ok(_) => {
                    let _ = tx.send(Msg::Log(Level::Ok, format!("AVD {name} dibuat")));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Log(Level::Err, e.to_string()));
                }
            }
            let _ = tx.send(Msg::Refresh);
        });
        self.create = None;
    }

    fn edit_modal(&mut self, ctx: &egui::Context, act: &mut Option<Act>) {
        let Some(mut form) = self.edit.take() else { return };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;

        egui::Window::new(format!("Edit {}", form.avd.name))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(500.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Ditulis ke config.ini AVD. Restart emulator untuk menerapkan.")
                        .color(MUTED)
                        .size(11.0),
                );
                ui.add_space(8.0);
                egui::Grid::new("edit-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("RAM (MB)");
                        ui.add(egui::TextEdit::singleline(&mut form.ram).desired_width(140.0));
                        ui.end_row();
                        ui.label("CPU cores");
                        ui.add(egui::TextEdit::singleline(&mut form.cores).desired_width(140.0));
                        ui.end_row();
                        ui.label("Lebar (px)");
                        ui.add(egui::TextEdit::singleline(&mut form.width).desired_width(140.0));
                        ui.end_row();
                        ui.label("Tinggi (px)");
                        ui.add(egui::TextEdit::singleline(&mut form.height).desired_width(140.0));
                        ui.end_row();
                        ui.label("Density (dpi)");
                        ui.add(egui::TextEdit::singleline(&mut form.density).desired_width(140.0));
                        ui.end_row();
                        ui.label("SD card");
                        ui.add(egui::TextEdit::singleline(&mut form.sdcard).desired_width(140.0));
                        ui.end_row();
                        ui.label("Data partition");
                        ui.add(egui::TextEdit::singleline(&mut form.data).desired_width(140.0));
                        ui.end_row();
                        ui.label("GPU mode");
                        egui::ComboBox::from_id_salt("gpu-mode")
                            .selected_text(form.gpu.clone())
                            .width(160.0)
                            .show_ui(ui, |ui| {
                                for mode in [
                                    "auto",
                                    "host",
                                    "swiftshader_indirect",
                                    "angle_indirect",
                                    "guest",
                                ] {
                                    ui.selectable_value(&mut form.gpu, mode.to_string(), mode);
                                }
                            });
                        ui.end_row();
                    });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Simpan").clicked() {
                        save = true;
                    }
                    if ui.button("Batal").clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel || !open {
            self.edit = None;
            return;
        }
        self.edit = Some(form);
        if save {
            *act = Some(Act::SaveEdit);
        }
    }

    fn delete_modal(&mut self, ctx: &egui::Context, act: &mut Option<Act>) {
        let Some(name) = self.deleting.clone() else { return };
        let mut open = true;
        let mut confirmed = false;
        let mut cancel = false;
        let running = self.running.contains_key(&name);
        egui::Window::new("Hapus AVD")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Hapus AVD \"{name}\" beserta data disk-nya?"));
                if running {
                    ui.label(
                        RichText::new("AVD sedang berjalan — matikan dulu.")
                            .color(WARN)
                            .size(11.5),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Hapus").color(BAD)).clicked() {
                        confirmed = true;
                    }
                    if ui.button("Batal").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel || !open {
            self.deleting = None;
        } else if confirmed {
            *act = Some(Act::Delete { name });
        }
    }

    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut save = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(580.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                egui::Grid::new("set-grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("SDK root");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_draft.sdk_override)
                                .hint_text(self.sdk.root.display().to_string())
                                .desired_width(380.0),
                        );
                        ui.end_row();
                        ui.label("Emulator args");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings_draft.extra_args)
                                .hint_text("-gpu host -memory 4096")
                                .desired_width(380.0),
                        );
                        ui.end_row();
                        ui.label("Live interval");
                        ui.add(egui::Slider::new(&mut self.settings_draft.live_ms, 400..=5000));
                        ui.end_row();
                    });
                ui.add_space(10.0);
                let dump = format!(
                    "emulator  : {}\nadb       : {}\navdmanager: {}\nsdkmanager: {}\njava      : {}\nAVD home  : {}",
                    opt_path(&self.sdk.emulator),
                    opt_path(&self.sdk.adb),
                    opt_path(&self.sdk.avdmanager),
                    opt_path(&self.sdk.sdkmanager),
                    opt_path(&self.sdk.java),
                    self.sdk.avd_home.display()
                );
                ui.label(RichText::new(dump).color(MUTED).size(11.0).monospace());
                if self.sdk.avdmanager.is_none() {
                    ui.label(
                        RichText::new(
                            "avdmanager butuh JDK: set JAVA_HOME ke folder JDK 17+.",
                        )
                        .color(WARN)
                        .size(11.0),
                    );
                }
                if ui.button("Simpan").clicked() {
                    save = true;
                }
            });
        if !open {
            self.settings_open = false;
            return;
        }
        if save {
            self.save_settings();
            self.settings_open = false;
        }
    }
}

// ---------------------------------------------------------------- helpers

fn kv(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).color(MUTED).size(12.0));
    if value.trim().is_empty() {
        ui.label(RichText::new("-").color(MUTED).size(12.5));
    } else {
        ui.label(RichText::new(value).size(12.5));
    }
    ui.end_row();
}

fn dash(s: &str) -> &str {
    if s.trim().is_empty() {
        "-"
    } else {
        s
    }
}

fn card(ui: &mut egui::Ui, selected: bool, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let fill = if selected { CARD_SEL } else { CARD };
    let stroke = if selected {
        egui::Stroke::new(1.0, ACCENT)
    } else {
        egui::Stroke::NONE
    };
    let resp = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .corner_radius(CornerRadius::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        })
        .response;
    let resp = ui.interact(resp.rect, resp.id.with("click"), egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if !selected {
            ui.painter().rect_filled(
                resp.rect,
                CornerRadius::same(8),
                Color32::from_rgba_unmultiplied(255, 255, 255, 12),
            );
        }
    }
    resp.clicked()
}

fn avd_card(ui: &mut egui::Ui, avd: &Avd, serial: Option<&str>, selected: bool) -> bool {
    let running = serial.is_some();
    card(ui, selected, |ui| {
        ui.horizontal(|ui| {
            let color = if running { GOOD } else { MUTED };
            ui.label(RichText::new("*").color(color).size(14.0));
            ui.label(RichText::new(&avd.name).strong().size(13.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(if running { "running" } else { "stopped" })
                        .color(color)
                        .size(10.5),
                );
            });
        });
        ui.label(
            RichText::new(format!(
                "API {} · {} · {} · {} MB",
                avd.api(),
                avd.tag.replace('_', " "),
                avd.abi,
                dash(&avd.ram)
            ))
            .color(MUTED)
            .size(10.5),
        );
    })
}

fn device_card(ui: &mut egui::Ui, dev: &Device, selected: bool) -> bool {
    let ok = dev.state == "device";
    card(ui, selected, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("*")
                    .color(if ok { GOOD } else { WARN })
                    .size(14.0),
            );
            ui.label(RichText::new(&dev.serial).strong().size(13.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&dev.state).color(MUTED).size(10.5));
            });
        });
        let kind = if dev.is_emulator { "emulator" } else { "physical" };
        let sub = if dev.model.is_empty() {
            kind.to_string()
        } else {
            format!("{} · {}", dev.model, kind)
        };
        ui.label(RichText::new(sub).color(MUTED).size(10.5));
    })
}

fn opt_path(p: &Option<PathBuf>) -> String {
    p.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(not found)".into())
}

fn short_path(p: &Path) -> String {
    let s = p.display().to_string();
    if s.chars().count() > 52 {
        let tail: String = s.chars().rev().take(49).collect();
        format!("…{}", tail.chars().rev().collect::<String>())
    } else {
        s
    }
}

fn out_dir() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("devmgr-out")
}

fn save_png(bytes: &[u8]) -> anyhow::Result<PathBuf> {
    let dir = out_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("screenshot-{stamp}.png"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn decode_png(bytes: &[u8]) -> anyhow::Result<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(w * h * 4);
            for px in data.chunks_exact(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        other => anyhow::bail!("format warna tidak didukung: {other:?}"),
    };
    if rgba.len() < w * h * 4 {
        anyhow::bail!("data piksel tidak lengkap");
    }
    Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}
