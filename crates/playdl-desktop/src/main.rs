// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! PlayDL Desktop GUI — native IDM-style download manager window.
//!
//! A real desktop window (egui/eframe), no browser. Left: category sidebar.
//! Right: task list with progress, speed and status. Downloads are driven by
//! spawning the `playdl` CLI as a child process.

use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

// ---------------- shared state ----------------

#[derive(Clone)]
enum Status {
    Queued,
    Running,
    Done,
    Failed(String),
}

struct Job {
    id: u64,
    url: String,
    file: String,
    status: Status,
    progress: f64,
    speed: f64,
    done_bytes: u64,
    total_bytes: u64,
    child: Option<Child>,
    started_bytes: u64,
    started_at: std::time::Instant,
}

impl Job {
    fn new(id: u64, url: String) -> Self {
        let file = sanitize_filename(&url);
        Self {
            id,
            url,
            file,
            status: Status::Queued,
            progress: 0.0,
            speed: 0.0,
            done_bytes: 0,
            total_bytes: 0,
            child: None,
            started_bytes: 0,
            started_at: std::time::Instant::now(),
        }
    }
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    next_id: u64,
}

type Shared = Arc<Mutex<State>>;

// ---------------- helpers ----------------

fn sanitize_filename(url: &str) -> String {
    let path = url.split('?').next().unwrap_or("download").trim_end_matches('/');
    let name = path.rsplit('/').next().unwrap_or("download").to_string();
    if name.is_empty() || name == "download" {
        "download.bin".into()
    } else {
        name
    }
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn find_playdl() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["playdl.exe", "pdl.exe", "playdl", "pdl"] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    for name in ["playdl.exe", "pdl.exe"] {
        if let Ok(cwd) = std::env::current_dir() {
            let p = cwd.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn engine_ok() -> bool {
    find_playdl().is_some()
}

fn fmt_size(b: u64) -> String {
    if b < 1024 {
        return format!("{b} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut v = b as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

fn fmt_speed(bps: f64) -> String {
    if bps <= 0.0 {
        return "--".into();
    }
    format!("{}/s", fmt_size(bps as u64))
}

// ---------------- background pump ----------------

/// Update progress from the files on disk, reap finished children.
fn pump(state: &Shared) {
    let mut s = state.lock().unwrap();
    let now = std::time::Instant::now();
    let mut finished: Vec<usize> = Vec::new();

    for (i, job) in s.jobs.iter_mut().enumerate() {
        if !matches!(job.status, Status::Running) {
            continue;
        }
        let elapsed = now.duration_since(job.started_at).as_secs_f64();
        let current = file_size(std::path::Path::new(&job.file));
        if elapsed > 0.5 {
            job.speed = (current - job.started_bytes) as f64 / elapsed;
        }
        job.done_bytes = current;
        if let Some(child) = job.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                job.child = None;
                if status.success() {
                    job.status = Status::Done;
                    job.progress = 100.0;
                } else {
                    job.status = Status::Failed(format!("exit {:?}", status.code()));
                }
                finished.push(i);
            }
        }
    }
    drop(s);
}

// ---------------- native app ----------------

struct App {
    state: Shared,
    url_input: String,
    conns: String,
    category: String,
    engine: bool,
    need_pump: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            url_input: String::new(),
            conns: "8".into(),
            category: "全部".into(),
            engine: engine_ok(),
            need_pump: false,
        }
    }
}

impl App {
    fn add_download(&mut self) {
        let url = self.url_input.trim().to_string();
        if url.is_empty() {
            return;
        }
        let exe = match find_playdl() {
            Some(e) => e,
            None => return,
        };

        let mut s = self.state.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        let mut job = Job::new(id, url.clone());
        job.file = sanitize_filename(&url);
        s.jobs.push(job);
        drop(s);

        // spawn playdl child
        let mut cmd = Command::new(&exe);
        cmd.arg("--json");
        if !self.conns.trim().is_empty() {
            cmd.arg("-x").arg(self.conns.trim());
        }
        let out = format!("{}", sanitize_filename(&url));
        cmd.arg("-o").arg(&out).arg(&url);
        cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
        match cmd.spawn() {
            Ok(child) => {
                let mut s = self.state.lock().unwrap();
                if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                    j.status = Status::Running;
                    j.child = Some(child);
                    j.started_at = std::time::Instant::now();
                    j.started_bytes = file_size(std::path::Path::new(&out));
                }
                drop(s);
            }
            Err(e) => {
                let mut s = self.state.lock().unwrap();
                if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                    j.status = Status::Failed(format!("spawn: {e}"));
                }
                drop(s);
            }
        }
        self.url_input.clear();
        self.need_pump = true;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // background progress updates
        if self.need_pump {
            pump(&self.state);
        }
        if ctx.input(|i| i.time - 0.0 > 0.0) {
            // poll occasionally
            pump(&self.state);
        }

        // ---------- top bar ----------
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                // fake logo chip
                let logo_color = egui::Color32::from_rgb(0, 200, 255);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(34.0, 34.0),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .rect_filled(rect, 8.0, logo_color);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "▶",
                    egui::FontId::proportional(16.0),
                    egui::Color32::WHITE,
                );

                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("PlayDL 下载管理器")
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::from_rgb(0, 200, 255)),
                    );
                    ui.label(
                        egui::RichText::new("v0.1.0 · Multi-Source Download Accelerator")
                            .size(10.0)
                            .color(egui::Color32::from_gray(120)),
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, color) = if self.engine {
                        ("● 引擎就绪", egui::Color32::from_rgb(0, 255, 140))
                    } else {
                        ("● 引擎离线", egui::Color32::from_rgb(255, 80, 80))
                    };
                    ui.label(
                        egui::RichText::new(text)
                            .size(12.0)
                            .strong()
                            .color(color),
                    );
                });
            });
            ui.add_space(6.0);

            // input row
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 260.0, 30.0],
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text("粘贴下载链接 https://...")
                        .font(egui::FontId::monospace(13.0)),
                );
                ui.label("连接");
                egui::ComboBox::from_id_salt("conns")
                    .selected_text(if self.conns.trim().is_empty() { "自动" } else { &self.conns })
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.conns, String::new(), "自动");
                        for n in ["4", "8", "16"] {
                            ui.selectable_value(&mut self.conns, n.to_string(), n);
                        }
                    });
                if ui
                    .add_sized(
                        [90.0, 30.0],
                        egui::Button::new(
                            egui::RichText::new("⬇ 下载").strong().size(13.0),
                        ),
                    )
                    .clicked()
                {
                    self.add_download();
                }
            });
            ui.add_space(8.0);
        });

        // ---------- left sidebar (categories) ----------
        egui::SidePanel::left("cats")
            .resizable(false)
            .exact_width(150.0)
            .show(ctx, |ui| {
                ui.add_space(10.0);
                let cats = ["全部", "未完成", "已完成", "压缩包", "文档"];
                for c in cats {
                    if ui
                        .selectable_label(self.category == c, c)
                        .clicked()
                    {
                        self.category = c.to_string();
                    }
                }
                ui.add_space(20.0);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new("PlayDL © 2026 leeymxz")
                            .size(9.0)
                            .color(egui::Color32::from_gray(90)),
                    );
                });
            });

        // ---------- central task list ----------
        egui::CentralPanel::default().show(ctx, |ui| {
            let s = self.state.lock().unwrap();

            let filtered: Vec<usize> = s
                .jobs
                .iter()
                .enumerate()
                .filter(|(_, j)| match self.category.as_str() {
                    "全部" => true,
                    "未完成" => matches!(j.status, Status::Queued | Status::Running),
                    "已完成" => matches!(j.status, Status::Done),
                    "压缩包" => j.file.ends_with(".zip")
                        || j.file.ends_with(".rar")
                        || j.file.ends_with(".7z")
                        || j.file.ends_with(".tar"),
                    "文档" => j.file.ends_with(".pdf")
                        || j.file.ends_with(".doc")
                        || j.file.ends_with(".docx")
                        || j.file.ends_with(".txt"),
                    _ => true,
                })
                .map(|(i, _)| i)
                .collect();

            ui.label(
                egui::RichText::new(format!("{} — {} 个任务", self.category, filtered.len()))
                    .size(12.0)
                    .color(egui::Color32::from_gray(140)),
            );
            ui.add_space(6.0);

            if filtered.is_empty() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("⚡").size(42.0));
                    ui.label("在上方输入链接，点击「下载」开始");
                    ui.label(
                        egui::RichText::new("PlayDL 支持多线程加速 · 断点续传 · 多源合并")
                            .size(11.0)
                            .color(egui::Color32::from_gray(110)),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for idx in filtered {
                    let job = &s.jobs[idx];
                    let name = job.file.clone();
                    let status_text = match &job.status {
                        Status::Queued => "排队中".to_string(),
                        Status::Running => "下载中".to_string(),
                        Status::Done => "已完成".to_string(),
                        Status::Failed(e) => format!("失败 ({e})"),
                    };
                    let status_color = match &job.status {
                        Status::Queued => egui::Color32::from_rgb(255, 200, 60),
                        Status::Running => egui::Color32::from_rgb(0, 200, 255),
                        Status::Done => egui::Color32::from_rgb(0, 255, 140),
                        Status::Failed(_) => egui::Color32::from_rgb(255, 90, 90),
                    };
                    let pct = job.progress;
                    let done = job.done_bytes;
                    let total = job.total_bytes;
                    let speed = job.speed;

                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgb(20, 24, 40))
                        .rounding(8.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("📄 {name}"))
                                        .size(13.0)
                                        .strong(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(status_text)
                                                .size(11.0)
                                                .strong()
                                                .color(status_color),
                                        );
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            let bar = egui::ProgressBar::new(pct as f32 / 100.0)
                                .desired_width(f32::INFINITY)
                                .fill(egui::Color32::from_rgb(0, 200, 255))
                                .text(format!("{pct:.1}%"));
                            ui.add(bar);
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} / {}",
                                        fmt_size(done),
                                        if total > 0 { fmt_size(total) } else { "--".into() }
                                    ))
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(140)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("速度 {}", fmt_speed(speed)))
                                                .size(11.0)
                                                .color(egui::Color32::from_rgb(0, 200, 255)),
                                        );
                                    },
                                );
                            });
                        });
                    ui.add_space(6.0);
                }
            });
            drop(s);
        });
    }
}

// ---------------- entry ----------------

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([820.0, 580.0])
            .with_min_inner_size([680.0, 440.0])
            .with_title("PlayDL 下载管理器"),
        ..Default::default()
    };
    eframe::run_native(
        "PlayDL 下载管理器",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}