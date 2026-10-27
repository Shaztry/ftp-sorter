mod remote;
mod sorter;

use eframe::egui::{
    self, pos2, vec2, Button, Frame, Rect, RichText, ScrollArea, Stroke, TextEdit,
};

use sorter::{Msg, Plan};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
    Arc,
};

const DEFAULT_UI_FONT: &str = "KG Perfect Penmanship";
const DEFAULT_MONO_FONT: &str = "Pointfree";

fn find_font(db: &fontdb::Database, family: &str) -> Option<Vec<u8>> {
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        ..Default::default()
    };
    let id = db.query(&query)?;
    db.with_face_data(id, |data, _index| data.to_vec())
}

fn apply_fonts(ctx: &egui::Context, ui_font: &str, mono_font: &str) -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();

    let mut fonts = egui::FontDefinitions::default();
    let mut msgs = Vec::new();

    for (key, name, family) in [
        ("custom_ui", ui_font, egui::FontFamily::Proportional),
        ("custom_mono", mono_font, egui::FontFamily::Monospace),
    ] {
        match find_font(&db, name) {
            Some(data) => {
                fonts
                .font_data
                .insert(key.to_string(), egui::FontData::from_owned(data));
                fonts
                .families
                .get_mut(&family)
                .unwrap()
                .insert(0, key.to_string());
                msgs.push(format!("Font loaded: {name}"));
            }
            None => msgs.push(format!("Font NOT found: '{name}' (using default)")),
        }
    }

    ctx.set_fonts(fonts);
    msgs
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/ftp-sorter/last.txt")
}

fn load_last() -> Option<[String; 4]> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let mut l = text.lines();
    Some([l.next()?.into(), l.next()?.into(), l.next()?.into(), l.next()?.into()])
}

fn save_last(ip: &str, port: &str, user: &str, folder: &str) {
    let p = config_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, format!("{ip}\n{port}\n{user}\n{folder}\n"));
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
}

fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

fn mmss(secs: f64) -> String {
    let s = secs.ceil() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn move_lane(ui: &mut egui::Ui, active: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let col = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let accent = ui.visuals().hyperlink_color;
    let y = rect.center().y;
    let (left, right) = (rect.left() + 14.0, rect.right() - 14.0);

    p.line_segment([pos2(left, y), pos2(right, y)], Stroke::new(1.0_f32, weak));
    p.rect_stroke(Rect::from_center_size(pos2(left, y), vec2(20.0, 16.0)), 3.0, Stroke::new(1.5_f32, col));
    p.rect_stroke(Rect::from_center_size(pos2(right, y), vec2(20.0, 16.0)), 3.0, Stroke::new(1.5_f32, col));

    if active {
        let time = ui.input(|i| i.time);
        for k in 0..4 {
            let t = ((time * 0.7) + k as f64 / 4.0) % 1.0;
            let x = left + 18.0 + (right - left - 36.0) * t as f32;
            p.rect_filled(Rect::from_center_size(pos2(x, y), vec2(10.0, 13.0)), 2.0, accent);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Scanning,
    Ready,
    Running,
    Finished,
}

struct App {
    use_local: bool,
    home_dir: PathBuf,
    ip: String,
    port: String,
    user: String,
    pass: String,
    folder: String,
    folders: Vec<String>,
    log: Vec<String>,
    phase: Phase,
    plan: Option<Arc<Plan>>,
    progress: (usize, usize),
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    dark: bool,
    ui_font: String,
    mono_font: String,
    show_settings: bool,
    show_extras: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = vec2(10.0, 10.0);
            s.spacing.button_padding = vec2(14.0, 6.0);
            s.spacing.interact_size.y = 30.0;
        });
        apply_theme(&cc.egui_ctx, true);

        let mut log = vec!["Ready.".to_string()];
        log.extend(apply_fonts(&cc.egui_ctx, DEFAULT_UI_FONT, DEFAULT_MONO_FONT));

        let [ip, port, user, folder] = load_last().unwrap_or_else(|| {
            [
                "192.168.1.13".into(),
                                                                  "2221".into(),
                                                                  "android".into(),
                                                                  "Download".into(),
            ]
        });

        let home_path = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home"));

        Self {
            use_local: false,
            home_dir: home_path,
            ip,
            port,
            user,
            pass: String::new(),
            folder,
            folders: Vec::new(),
            log,
            phase: Phase::Idle,
            plan: None,
            progress: (0, 0),
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            dark: true,
            ui_font: DEFAULT_UI_FONT.into(),
            mono_font: DEFAULT_MONO_FONT.into(),
            show_settings: false,
            show_extras: false,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Scanning | Phase::Running)
    }

    fn reset(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
        self.ip = "192.168.1.13".into();
        self.port = "2221".into();
        self.user = "android".into();
        self.pass.clear();
        self.folder = "Download".into();
        self.log = vec!["Ready.".into()];
        self.phase = Phase::Idle;
        self.plan = None;
        self.progress = (0, 0);
    }

    fn start_load_folders(&mut self, ctx: &egui::Context) {
        if self.use_local {
            self.log.push(format!("Loading local folders from {}...", self.home_dir.display()));
            match sorter::list_folders(&self.home_dir) {
                Ok(folders) => {
                    self.log.push(format!("Found {} local folders", folders.len()));
                    self.folders = folders;
                }
                Err(e) => {
                    self.log.push(format!("ERROR reading local folders: {e}"));
                }
            }
            return;
        }

        save_last(&self.ip, &self.port, &self.user, &self.folder);
        let conn = remote::Conn {
            ip: self.ip.clone(),
            port: self.port.clone(),
            user: self.user.clone(),
            pass: self.pass.clone(),
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.log.push("Connecting to fetch FTP folders...".into());
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            match remote::connect(&conn, &tx, &cancel) {
                Ok(mnt) => {
                    if let Ok(folders) = sorter::list_folders(&mnt) {
                        let _ = tx.send(Msg::Log(format!("Found {} folders", folders.len())));
                        let _ = tx.send(Msg::FoldersLoaded(folders));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::ScanFailed(e));
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        let folder = self.folder.trim().trim_matches('/').to_string();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.phase = Phase::Scanning;
        self.plan = None;
        self.progress = (0, 0);
        let ctx = ctx.clone();

        if self.use_local {
            let target = if folder.is_empty() {
                self.home_dir.clone()
            } else {
                self.home_dir.join(&folder)
            };

            // Enforce /home boundary guard
            if !target.starts_with(&self.home_dir) && !target.starts_with("/home") {
                self.log.push("ERROR: Access outside /home is strictly forbidden.".into());
                self.phase = Phase::Idle;
                return;
            }

            std::thread::spawn(move || {
                let res = sorter::scan(&target, &tx, &cancel);
                let _ = match res {
                    Ok(plan) => tx.send(Msg::Scanned(plan)),
                               Err(e) => tx.send(Msg::ScanFailed(e)),
                };
                ctx.request_repaint();
            });
            return;
        }

        save_last(&self.ip, &self.port, &self.user, &self.folder);
        let conn = remote::Conn {
            ip: self.ip.clone(),
            port: self.port.clone(),
            user: self.user.clone(),
            pass: self.pass.clone(),
        };

        std::thread::spawn(move || {
            let res = remote::connect(&conn, &tx, &cancel).and_then(|mnt| {
                let target = if folder.is_empty() { mnt } else { mnt.join(&folder) };
                sorter::scan(&target, &tx, &cancel)
            });
            let _ = match res {
                Ok(plan) => tx.send(Msg::Scanned(plan)),
                           Err(e) => tx.send(Msg::ScanFailed(e)),
            };
            ctx.request_repaint();
        });
    }

    fn start_run(&mut self, ctx: &egui::Context) {
        let Some(plan) = self.plan.clone() else { return };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.phase = Phase::Running;
        self.progress = (0, plan.moves.len());
        self.log.push("Starting moves...".into());
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            sorter::run(&plan, &tx, &cancel);
            ctx.request_repaint();
        });
    }

    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        let msgs: Vec<Msg> = rx.try_iter().collect();
        for m in msgs {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Progress(d, t) => self.progress = (d, t),
                Msg::FoldersLoaded(f) => self.folders = f,
                Msg::Scanned(p) => {
                    self.phase = if p.moves.is_empty() { Phase::Idle } else { Phase::Ready };
                    if p.moves.is_empty() {
                        self.log.push("Nothing to move.".into());
                    } else {
                        self.log.push("Scan complete. Press GO to move the files.".into());
                    }
                    self.plan = Some(Arc::new(p));
                }
                Msg::ScanFailed(e) => {
                    self.log.push(format!("ERROR: {e}"));
                    self.phase = Phase::Idle;
                }
                Msg::Done => self.phase = Phase::Finished,
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let busy = self.busy();
        let can_scan = !busy;
        let can_go = self.phase == Phase::Ready;

        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(Button::new("New").frame(false)).clicked() {
                    self.reset();
                }
                ui.label("|");
                if ui.add(Button::new("Settings").frame(false)).clicked() {
                    self.show_settings = true;
                }
                ui.label("|");
                if ui.add(Button::new("Extras").frame(false)).clicked() {
                    self.show_extras = true;
                }
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("actions").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.columns(4, |cols| {
                let size = vec2(cols[0].available_width(), 38.0);
                let conn_btn = if self.use_local { "Fetch Folders" } else { "Connect" };

                if cols[0]
                    .add_enabled(can_scan, Button::new(conn_btn).min_size(size))
                    .clicked()
                    {
                        self.start_load_folders(ctx);
                    }
                    if cols[1]
                        .add_enabled(can_scan, Button::new("Scan").min_size(size))
                        .clicked()
                        {
                            self.start_scan(ctx);
                        }
                        if cols[2]
                            .add_enabled(can_go, Button::new("Go").min_size(size))
                            .clicked()
                            {
                                self.start_run(ctx);
                            }
                            if cols[3]
                                .add_enabled(busy, Button::new("Cancel").min_size(size))
                                .clicked()
                                {
                                    self.cancel.store(true, Ordering::Relaxed);
                                    self.log.push("Cancelling...".into());
                                }
            });
            ui.add_space(10.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                Frame::group(cols[0].style()).show(&mut cols[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(230.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Source Mode").strong());
                        ui.selectable_value(&mut self.use_local, false, "FTP Remote");
                        ui.selectable_value(&mut self.use_local, true, "Local Machine");
                    });
                    ui.separator();

                    let w = ui.available_width() - 100.0;
                    ui.add_enabled_ui(!busy, |ui| {
                        if self.use_local {
                            egui::Grid::new("local_conn").num_columns(2).spacing([12.0, 12.0]).show(ui, |ui| {
                                ui.label("Base Directory");
                                ui.label(RichText::new(self.home_dir.to_string_lossy()).monospace());
                                ui.end_row();

                                ui.label("Target Folder");
                                let shown = if self.folder.is_empty() {
                                    "(choose)".to_string()
                                } else {
                                    self.folder.clone()
                                };
                                egui::ComboBox::from_id_salt("local_folder")
                                .selected_text(shown)
                                .width(w)
                                .show_ui(ui, |ui| {
                                    for f in self.folders.clone() {
                                        ui.selectable_value(&mut self.folder, f.clone(), f);
                                    }
                                });
                                ui.end_row();
                            });
                        } else {
                            egui::Grid::new("conn").num_columns(2).spacing([12.0, 12.0]).show(ui, |ui| {
                                ui.label("IP");
                                ui.add(TextEdit::singleline(&mut self.ip).desired_width(w));
                                ui.end_row();
                                ui.label("Port");
                                ui.add(TextEdit::singleline(&mut self.port).desired_width(w));
                                ui.end_row();
                                ui.label("Username");
                                ui.add(TextEdit::singleline(&mut self.user).desired_width(w));
                                ui.end_row();
                                ui.label("Password");
                                ui.add(TextEdit::singleline(&mut self.pass).password(true).desired_width(w));
                                ui.end_row();
                                ui.label("Folder");
                                let shown = if self.folder.is_empty() {
                                    "(choose)".to_string()
                                } else {
                                    self.folder.clone()
                                };
                                egui::ComboBox::from_id_salt("folder")
                                .selected_text(shown)
                                .width(w)
                                .show_ui(ui, |ui| {
                                    for f in self.folders.clone() {
                                        ui.selectable_value(&mut self.folder, f.clone(), f);
                                    }
                                });
                                ui.end_row();
                            });
                        }
                    });
                });

                Frame::group(cols[1].style()).show(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(230.0);
                    ui.label(RichText::new("Results").strong());
                    match (&self.plan, self.phase) {
                        (_, Phase::Scanning) => {
                            ui.label("Scanning...");
                        }
                        (Some(p), _) if !p.moves.is_empty() => {
                            ScrollArea::vertical().show(ui, |ui| {
                                for (name, (count, size)) in &p.by_cat {
                                    ui.label(
                                        RichText::new(format!("{:<16}{:>5}  ({})", name, count, human(*size)))
                                        .monospace(),
                                    );
                                }
                                ui.separator();
                                ui.label(
                                    RichText::new(format!(
                                        "Total: {} files ({})",
                                                          p.moves.len(),
                                                          human(p.total_bytes())
                                    ))
                                    .monospace(),
                                );
                                ui.label(
                                    RichText::new(format!("Expected time: ~{}", mmss(p.eta_secs())))
                                    .monospace(),
                                );
                            });
                        }
                        (Some(_), _) => {
                            ui.label("Nothing to move in that folder.");
                        }
                        _ => {
                            ui.label("Press Scan to see what can be moved.");
                        }
                    }
                });
            });

            if matches!(self.phase, Phase::Running | Phase::Finished) && self.progress.1 > 0 {
                let (d, t) = self.progress;
                ui.add(
                    egui::ProgressBar::new(d as f32 / t as f32)
                    .text(format!("{d} / {t} files"))
                    .animate(false),
                );
                move_lane(ui, self.phase == Phase::Running);
            }

            let h = (ui.available_height() - 20.0).max(80.0);
            Frame::group(ui.style()).fill(ui.visuals().extreme_bg_color).show(ui, |ui| {
                ui.set_min_size(vec2(ui.available_width(), h));
                ui.label(RichText::new("Terminal :").strong());
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
                ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show_rows(ui, row_h, self.log.len(), |ui, range| {
                    for i in range {
                        ui.label(RichText::new(&self.log[i]).monospace());
                    }
                });
            });
        });

        let mut open = self.show_settings;
        egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(RichText::new("Theme").strong());
            ui.horizontal(|ui| {
                let a = ui.radio_value(&mut self.dark, true, "Dark").changed();
                let b = ui.radio_value(&mut self.dark, false, "Light").changed();
                if a || b {
                    apply_theme(ctx, self.dark);
                }
            });
            ui.separator();
            ui.label(RichText::new("Fonts").strong());
            egui::Grid::new("fonts").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("UI font");
                ui.add(TextEdit::singleline(&mut self.ui_font).desired_width(240.0));
                ui.end_row();
                ui.label("Terminal font");
                ui.add(TextEdit::singleline(&mut self.mono_font).desired_width(240.0));
                ui.end_row();
            });
            if ui.button("Apply").clicked() {
                let msgs = apply_fonts(ctx, &self.ui_font, &self.mono_font);
                self.log.extend(msgs);
            }
        });
        self.show_settings = open;

        let mut open = self.show_extras;
        egui::Window::new("Extras")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Coming soon.");
        });
        self.show_extras = open;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancel.store(true, Ordering::Relaxed);
        if !self.use_local {
            remote::unmount(&remote::mount_dir());
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
        .with_inner_size([900.0, 700.0])
        .with_min_inner_size([760.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("FTP Sorter", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
