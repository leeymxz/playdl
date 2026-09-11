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
    // 安装布局：{app}\bin\playdl.exe 或 {app}\playdl.exe
    let layouts = [
        dir.join("bin").join("playdl.exe"),
        dir.join("bin").join("pdl.exe"),
        dir.join("playdl.exe"),
        dir.join("pdl.exe"),
    ];
    for p in layouts {
        if p.exists() {
            return Some(p);
        }
    }
    // PATH 查找
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
    selected: Option<u64>,
    paused: Vec<u64>,
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
            selected: None,
            paused: Vec::new(),
        }
    }
}

/// 统计某个分类的任务数量
fn count_category(s: &State, cat: &str) -> usize {
    s.jobs
        .iter()
        .filter(|j| match cat {
            "全部" => true,
            "未完成" => matches!(j.status, Status::Queued | Status::Running),
            "已完成" => matches!(j.status, Status::Done),
            "压缩包" => {
                j.file.ends_with(".zip")
                    || j.file.ends_with(".rar")
                    || j.file.ends_with(".7z")
                    || j.file.ends_with(".tar")
            }
            "文档" => {
                j.file.ends_with(".pdf")
                    || j.file.ends_with(".doc")
                    || j.file.ends_with(".docx")
                    || j.file.ends_with(".txt")
            }
            _ => true,
        })
        .count()
}

/// 任务是否匹配分类
fn matches_category(j: &Job, cat: &str) -> bool {
    match cat {
        "全部" => true,
        "未完成" => matches!(j.status, Status::Queued | Status::Running),
        "已完成" => matches!(j.status, Status::Done),
        "压缩包" => {
            j.file.ends_with(".zip")
                || j.file.ends_with(".rar")
                || j.file.ends_with(".7z")
                || j.file.ends_with(".tar")
        }
        "文档" => {
            j.file.ends_with(".pdf")
                || j.file.ends_with(".doc")
                || j.file.ends_with(".docx")
                || j.file.ends_with(".txt")
        }
        _ => true,
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

    fn remove_selected(&mut self) {
        if let Some(id) = self.selected {
            let mut s = self.state.lock().unwrap();
            // 尝试终止子进程
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                if let Some(mut child) = j.child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            s.jobs.retain(|j| j.id != id);
            drop(s);
            self.paused.retain(|&x| x != id);
            self.selected = None;
        }
    }

    fn toggle_pause_selected(&mut self) {
        if let Some(id) = self.selected {
            let mut s = self.state.lock().unwrap();
            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                match &j.status {
                    Status::Running => {
                        // 暂停：终止子进程，标记状态（简单处理：停止并标记暂停）
                        if let Some(mut child) = j.child.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        j.status = Status::Queued;
                    }
                    _ => {
                        // 恢复：重新启动（简化为不处理，仅提示）
                    }
                }
            }
            drop(s);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // background progress updates
        pump(&self.state);

        // ---------- top bar: logo + engine status ----------
        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                // logo chip
                let logo_color = egui::Color32::from_rgb(0, 200, 255);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(34.0, 34.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(rect, 8.0, logo_color);
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
                        egui::RichText::new(text).size(12.0).strong().color(color),
                    );
                });
            });
            ui.add_space(6.0);
        });

        // ---------- toolbar ----------
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // 新建任务
                if ui
                    .add(egui::Button::new(egui::RichText::new("➕ 新建任务").strong()))
                    .clicked()
                {
                    // focus url input
                }
                ui.separator();
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("⏸ 暂停"))
                    .clicked()
                {
                    self.toggle_pause_selected();
                }
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("▶ 继续"))
                    .clicked()
                {
                    self.toggle_pause_selected();
                }
                ui.separator();
                if ui
                    .add_enabled(self.selected.is_some(), egui::Button::new("🗑 删除"))
                    .clicked()
                {
                    self.remove_selected();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("⚙ 设置")
                            .size(12.0)
                            .color(egui::Color32::from_gray(150)),
                    );
                });
            });
            ui.add_space(4.0);

            // input row
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 230.0, 28.0],
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text("粘贴下载链接 https://...")
                        .font(egui::FontId::monospace(13.0)),
                );
                ui.label("连接");
                egui::ComboBox::from_id_salt("conns")
                    .selected_text(if self.conns.trim().is_empty() { "自动" } else { &self.conns })
                    .width(55.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.conns, String::new(), "自动");
                        for n in ["4", "8", "16"] {
                            ui.selectable_value(&mut self.conns, n.to_string(), n);
                        }
                    });
                if ui
                    .add_sized(
                        [80.0, 28.0],
                        egui::Button::new(egui::RichText::new("⬇ 下载").strong().size(13.0)),
                    )
                    .clicked()
                {
                    self.add_download();
                }
            });
            ui.add_space(6.0);
        });

        // ---------- left sidebar: categories with counts ----------
        egui::SidePanel::left("cats")
            .resizable(false)
            .exact_width(160.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                let s = self.state.lock().unwrap();
                let cats = ["全部", "未完成", "已完成", "压缩包", "文档"];
                for c in cats {
                    let count = count_category(&s, c);
                    let label = if count > 0 {
                        format!("{c}   ({count})")
                    } else {
                        c.to_string()
                    };
                    if ui
                        .selectable_label(self.category == c, label)
                        .clicked()
                    {
                        self.category = c.to_string();
                    }
                }
                drop(s);
                ui.add_space(20.0);
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new("PlayDL © 2026 leeymxz")
                            .size(9.0)
                            .color(egui::Color32::from_gray(90)),
                    );
                });
            });

        // ---------- central: task table ----------
        egui::CentralPanel::default().show(ctx, |ui| {
            // 先收集需要渲染的数据，释放锁
            let snapshot: Vec<(u64, String, String, f64, u64, u64, f64, Status)> = {
                let s = self.state.lock().unwrap();
                s.jobs
                    .iter()
                    .filter(|j| matches_category(j, &self.category))
                    .map(|j| {
                        (
                            j.id,
                            j.file.clone(),
                            match &j.status {
                                Status::Queued => "排队中".to_string(),
                                Status::Running => "下载中".to_string(),
                                Status::Done => "已完成".to_string(),
                                Status::Failed(e) => format!("失败 ({e})"),
                            },
                            j.progress,
                            j.done_bytes,
                            j.total_bytes,
                            j.speed,
                            j.status.clone(),
                        )
                    })
                    .collect()
            };

            // column header
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("文件名")
                        .size(11.0)
                        .strong()
                        .color(egui::Color32::from_gray(150)),
                );
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("状态")
                                .size(11.0)
                                .strong()
                                .color(egui::Color32::from_gray(150)),
                        );
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("速度")
                                .size(11.0)
                                .strong()
                                .color(egui::Color32::from_gray(150)),
                        );
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("大小")
                                .size(11.0)
                                .strong()
                                .color(egui::Color32::from_gray(150)),
                        );
                    },
                );
            });
            ui.separator();

            if snapshot.is_empty() {
                ui.add_space(80.0);
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
                for (id, name, status_text, pct, done, total, speed, status) in &snapshot {
                    let status_color = match status {
                        Status::Queued => egui::Color32::from_rgb(255, 200, 60),
                        Status::Running => egui::Color32::from_rgb(0, 200, 255),
                        Status::Done => egui::Color32::from_rgb(0, 255, 140),
                        Status::Failed(_) => egui::Color32::from_rgb(255, 90, 90),
                    };

                    let is_selected = self.selected == Some(*id);
                    let resp = egui::Frame::group(ui.style())
                        .fill(if is_selected {
                            egui::Color32::from_rgb(24, 34, 56)
                        } else {
                            egui::Color32::from_rgb(18, 22, 38)
                        })
                        .stroke(if is_selected {
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 200, 255))
                        } else {
                            egui::Stroke::NONE
                        })
                        .rounding(6.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            // row 1: name + status
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("📄 {name}"))
                                        .size(12.5)
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
                            ui.add_space(5.0);
                            // progress bar with % text
                            let bar = egui::ProgressBar::new(*pct as f32 / 100.0)
                                .desired_width(f32::INFINITY)
                                .fill(egui::Color32::from_rgb(0, 200, 255))
                                .text(format!("{pct:.1}%"));
                            ui.add(bar);
                            ui.add_space(3.0);
                            // row 3: size / speed
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} / {}",
                                        fmt_size(*done),
                                        if *total > 0 { fmt_size(*total) } else { "--".into() }
                                    ))
                                    .size(10.5)
                                    .color(egui::Color32::from_gray(140)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("速度 {}", fmt_speed(*speed)))
                                                .size(10.5)
                                                .color(egui::Color32::from_rgb(0, 200, 255)),
                                        );
                                    },
                                );
                            });
                        });
                    if resp.response.clicked() {
                        self.selected = Some(*id);
                    }
                    ui.add_space(5.0);
                }
            });
        });

        // ---------- bottom status bar ----------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(3.0);
            let s = self.state.lock().unwrap();
            let total = s.jobs.len();
            let running = s
                .jobs
                .iter()
                .filter(|j| matches!(j.status, Status::Running))
                .count();
            let done = s
                .jobs
                .iter()
                .filter(|j| matches!(j.status, Status::Done))
                .count();
            let sum_speed: f64 = s
                .jobs
                .iter()
                .filter(|j| matches!(j.status, Status::Running))
                .map(|j| j.speed)
                .sum();
            drop(s);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("任务 {total}  ·  下载中 {running}  ·  完成 {done}"))
                        .size(11.0)
                        .color(egui::Color32::from_gray(160)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("总速度 {}", fmt_speed(sum_speed)))
                            .size(11.0)
                            .strong()
                            .color(egui::Color32::from_rgb(0, 200, 255)),
                    );
                });
            });
            ui.add_space(3.0);
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
        Box::new(|cc| {
            // 加载中文字体（微软雅黑 / 宋体等），否则中文会显示为方块乱码
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(App::default()))
        }),
    )
}

/// 注册 Windows 系统中文字体到 egui，解决中文乱码/方块问题。
fn install_cjk_font(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};

    let mut fonts = FontDefinitions::default();

    // 候选中文字体路径（按优先级）
    let font_candidates = [
        "C:\\Windows\\Fonts\\msyh.ttc",   // 微软雅黑
        "C:\\Windows\\Fonts\\msyhbd.ttc", // 微软雅黑粗体
        "C:\\Windows\\Fonts\\simhei.ttf", // 黑体
        "C:\\Windows\\Fonts\\simsun.ttc", // 宋体
        "C:\\Windows\\Fonts\\Deng.ttf",   // 等线
        "C:\\Windows\\Fonts\\msyh.ttf",
    ];

    let mut loaded = false;
    for path in font_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), std::sync::Arc::new(FontData::from_owned(bytes)));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk".to_owned());
            }
            loaded = true;
            break;
        }
    }

    if loaded {
        ctx.set_fonts(fonts);
    }
}