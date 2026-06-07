#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

type AppResult<T> = Result<T, Box<dyn Error>>;

const FRAME_MAX: &[u8] = b"[FRAME MAX]";
const SPECTRUM: &[u8] = b"[SPECTRUM]";

#[derive(Clone, Debug)]
struct Config {
    spectrum: SpectrumConfig,
    existing_policy: ExistingPolicy,
    min_frame_max: u32,
    allow_small_frame_name_contains: Vec<String>,
    blacklist_name_contains: Vec<String>,
    case_insensitive: bool,
}

#[derive(Clone, Debug)]
struct SpectrumConfig {
    enabled: bool,
    term: u32,
    life_time: u32,
    color: [u32; 4],
    effect: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExistingPolicy {
    Skip,
    Update,
    Force,
}

#[derive(Clone, Debug)]
struct ReportRow {
    mode: String,
    chr_path: String,
    job: String,
    body_image_path: String,
    ani_path: String,
    frame_max: String,
    has_spectrum_before: bool,
    decision: String,
    reason: String,
    sha256_before: String,
    sha256_after: String,
    backup_path: String,
}

#[derive(Clone, Debug)]
struct ManifestEntry {
    chr_path: String,
    job: String,
    body_image_path: String,
    ani_path: String,
    backup_path: String,
    frame_max: u32,
    action: String,
    sha256_before: String,
    sha256_after: String,
}

#[derive(Clone, Debug)]
struct Manifest {
    created_at: String,
    mode: String,
    source_root: String,
    output_root: String,
    config: Config,
    files: Vec<ManifestEntry>,
}

#[derive(Clone, Debug)]
struct ChrContext {
    rel_chr_path: String,
    chr_dir: PathBuf,
    job: String,
    body_path: Vec<u8>,
    body_path_text: String,
    ani_refs: Vec<Vec<u8>>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 || args[1] == "--gui" {
        run_gui()?;
        return Ok(());
    }

    match args[1].as_str() {
        "scan" => {
            let root = required_arg(&args, "--root")?;
            let report = optional_arg(&args, "--report")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("report_scan.csv"));
            let mut config = load_config(optional_arg(&args, "--config"))?;
            apply_cli_overrides(&mut config, &args);
            let rows = scan(Path::new(&root), &config, "scan")?;
            write_report(&report, &rows)?;
            println!("scan complete: {} rows -> {}", rows.len(), report.display());
        }
        "apply" => {
            let root = required_arg(&args, "--root")?;
            let out = required_arg(&args, "--out")?;
            let backup = required_arg(&args, "--backup")?;
            let report = optional_arg(&args, "--report")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&out).join("report.csv"));
            let mut config = load_config(optional_arg(&args, "--config"))?;
            apply_cli_overrides(&mut config, &args);
            let (rows, manifest_path, archive_path) = apply(
                Path::new(&root),
                Path::new(&out),
                Path::new(&backup),
                &report,
                &config,
            )?;
            println!(
                "apply complete: {} rows, manifest -> {}, 7z -> {}",
                rows.len(),
                manifest_path.display(),
                archive_path.display()
            );
        }
        "restore" | "remove" => {
            let manifest = required_arg(&args, "--manifest")?;
            let target = required_arg(&args, "--target")?;
            let report = optional_arg(&args, "--report")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&target).join("restore_report.csv"));
            let rows = restore(Path::new(&manifest), Path::new(&target), &report)?;
            println!(
                "restore complete: {} rows -> {}",
                rows.len(),
                report.display()
            );
        }
        "default-config" => {
            println!("{}", default_config_json());
        }
        _ => {
            print_usage();
        }
    }

    Ok(())
}

fn run_gui() -> AppResult<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 820.0])
            .with_min_inner_size([760.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PVF ANI 残影批量处理工具",
        options,
        Box::new(|cc| {
            install_chinese_font(&cc.egui_ctx);
            install_gui_style(&cc.egui_ctx);
            Box::new(GuiApp::default())
        }),
    )?;
    Ok(())
}

fn install_chinese_font(ctx: &egui::Context) {
    let font_candidates = [
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simfang.ttf",
        r"C:\Windows\Fonts\simsunb.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
    ];

    let Some(font_bytes) = font_candidates.iter().find_map(|path| fs::read(path).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_string(), egui::FontData::from_owned(font_bytes));
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cjk".to_string());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "cjk".to_string());
    ctx.set_fonts(fonts);
}

fn install_gui_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(44.0, 30.0);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(24.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(17.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(17.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        egui::FontId::new(16.0, egui::FontFamily::Monospace),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    ctx.set_style(style);
}

struct GuiApp {
    export_root: String,
    output_root: String,
    backup_root: String,
    report_path: String,
    manifest_path: String,
    restore_target: String,
    config: Config,
    small_frame_keywords: String,
    blacklist_keywords: String,
    status: String,
    last_report: Option<PathBuf>,
    last_output: Option<PathBuf>,
    last_archive: Option<PathBuf>,
    task: Option<TaskState>,
}

struct TaskState {
    title: String,
    receiver: Receiver<GuiTaskMessage>,
    done: usize,
    total: usize,
    label: String,
}

enum GuiTaskMessage {
    Progress {
        done: usize,
        total: usize,
        label: String,
    },
    Finished(Result<GuiTaskResult, String>),
}

struct GuiTaskResult {
    title: String,
    rows: Vec<ReportRow>,
    report_path: PathBuf,
    manifest_path: Option<PathBuf>,
    output_path: Option<PathBuf>,
    archive_path: Option<PathBuf>,
    failures: Vec<String>,
}

impl Default for GuiApp {
    fn default() -> Self {
        let config = Config::default();
        Self {
            export_root: String::new(),
            output_root: String::new(),
            backup_root: String::new(),
            report_path: String::new(),
            manifest_path: String::new(),
            restore_target: String::new(),
            small_frame_keywords: config.allow_small_frame_name_contains.join(", "),
            blacklist_keywords: config.blacklist_name_contains.join(", "),
            config,
            status: "请选择 PVF 导出目录，然后执行扫描或应用修改。".to_string(),
            last_report: None,
            last_output: None,
            last_archive: None,
            task: None,
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_task(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("PVF ANI 残影批量处理工具");
                    ui.add_space(10.0);

                    ui.group(|ui| {
                ui.label("路径");
                path_row(
                    ui,
                    "PVF 导出目录",
                    &mut self.export_root,
                    BrowseKind::Folder,
                    "从 PVF 编辑器导出的原始目录；工具只读取，不直接修改。",
                );
                if ui
                    .button("根据导出目录生成默认路径")
                    .on_hover_text("自动生成输出目录、备份目录和报告路径。")
                    .clicked()
                {
                    self.fill_default_paths();
                }
                path_row(
                    ui,
                    "修改输出目录",
                    &mut self.output_root,
                    BrowseKind::Folder,
                    "处理后的文件保存到这里；不能与原始导出目录相同，也不能放在原始目录里面。",
                );
                path_row(
                    ui,
                    "备份目录",
                    &mut self.backup_root,
                    BrowseKind::Folder,
                    "每次应用修改前自动创建批次目录，保存修改前的原始 ANI 和 manifest。",
                );
                path_row(
                    ui,
                    "报告输出路径",
                    &mut self.report_path,
                    BrowseKind::SaveFile,
                    "CSV 报告路径；记录每个 ANI 的处理结果、跳过原因和 hash。",
                );
            });

                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.label("残影参数");
                        ui.checkbox(&mut self.config.spectrum.enabled, "启用 [SPECTRUM]")
                            .on_hover_text(
                                "关闭后不会写入残影配置，扫描仍可用于查看哪些文件会被处理。",
                            );
                        help_text(ui, "控制实际写入 ANI 的 [SPECTRUM] 配置。");
                        ui.horizontal(|ui| {
                            ui.label("TERM");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.term)
                                    .clamp_range(0..=99999),
                            )
                            .on_hover_text("残影生成间隔。数值越小，残影越密。默认 70。");
                            ui.label("LIFE TIME");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.life_time)
                                    .clamp_range(0..=99999),
                            )
                            .on_hover_text("残影持续时间。数值越大，拖尾越长。默认 250。");
                        });
                        help_text(ui, "TERM 越小残影越密；LIFE TIME 越大拖尾越长。");
                        ui.horizontal(|ui| {
                            ui.label("R");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.color[0])
                                    .clamp_range(0..=255),
                            )
                            .on_hover_text("残影颜色红色通道，范围 0-255。");
                            ui.label("G");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.color[1])
                                    .clamp_range(0..=255),
                            )
                            .on_hover_text("残影颜色绿色通道，范围 0-255。");
                            ui.label("B");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.color[2])
                                    .clamp_range(0..=255),
                            )
                            .on_hover_text("残影颜色蓝色通道，范围 0-255。");
                            ui.label("A");
                            ui.add(
                                egui::DragValue::new(&mut self.config.spectrum.color[3])
                                    .clamp_range(0..=255),
                            )
                            .on_hover_text("残影透明度/强度，范围 0-255。默认 120。");
                        });
                        help_text(
                            ui,
                            "RGBA 控制残影颜色和透明度；默认 255,255,255,120 是白色半透明。",
                        );
                        ui.horizontal(|ui| {
                            ui.label("EFFECT");
                            egui::ComboBox::from_id_source("effect_combo")
                                .selected_text(&self.config.spectrum.effect)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.config.spectrum.effect,
                                        "NONE".to_string(),
                                        "NONE",
                                    );
                                    ui.selectable_value(
                                        &mut self.config.spectrum.effect,
                                        "LINEARDODGE".to_string(),
                                        "LINEARDODGE",
                                    );
                                });
                        });
                        help_text(
                            ui,
                            "EFFECT 是混合效果：NONE 普通混合；LINEARDODGE 更亮、更接近发光。",
                        );
                    });

                    ui.add_space(8.0);
                    ui.group(|ui| {
                ui.label("筛选规则");
                ui.horizontal(|ui| {
                    ui.label("最小 FRAME MAX");
                    ui.add(
                        egui::DragValue::new(&mut self.config.min_frame_max).clamp_range(1..=999),
                    )
                    .on_hover_text("FRAME MAX 大于等于这个值时才默认处理。默认 4。");
                    ui.checkbox(&mut self.config.case_insensitive, "大小写不敏感")
                        .on_hover_text(
                            "路径、body image path、黑名单和白名单匹配时忽略大小写。建议开启。",
                        );
                });
                help_text(
                    ui,
                    "帧数规则：FRAME MAX <= 2 跳过；等于 3 需命中白名单；大于等于最小值才处理。",
                );
                ui.horizontal(|ui| {
                    ui.label("已有 [SPECTRUM]");
                    egui::ComboBox::from_id_source("existing_policy_combo")
                        .selected_text(existing_policy_label(self.config.existing_policy))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.existing_policy,
                                ExistingPolicy::Skip,
                                "skip",
                            );
                            ui.selectable_value(
                                &mut self.config.existing_policy,
                                ExistingPolicy::Update,
                                "update",
                            );
                            ui.selectable_value(
                                &mut self.config.existing_policy,
                                ExistingPolicy::Force,
                                "force",
                            );
                        });
                });
                help_text(
                    ui,
                    "已有残影策略：skip 跳过；update/force 会删除旧残影块并按当前参数重新插入。",
                );
                ui.label("低帧数白名单");
                ui.text_edit_singleline(&mut self.small_frame_keywords)
                    .on_hover_text(
                        "FRAME MAX == 3 时，文件名包含这些关键词才允许处理。用英文逗号分隔。",
                    );
                help_text(ui, "用于允许 Dash、Move、Attack 这类低帧动作也添加残影。");
                ui.label("文件名黑名单");
                ui.text_edit_singleline(&mut self.blacklist_keywords)
                    .on_hover_text("文件路径或文件名包含这些关键词时直接跳过。用英文逗号分隔。");
                help_text(
                    ui,
                    "用于跳过待机、受击、倒地、休息、占位等不适合加残影的动作。",
                );
            });

                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let idle = self.task.is_none();
                        if ui.add_enabled(idle, egui::Button::new("扫描")).clicked() {
                            self.run_scan();
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new("应用修改"))
                            .clicked()
                        {
                            self.run_apply();
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new("选择 manifest"))
                            .clicked()
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("manifest", &["json"])
                                .pick_file()
                            {
                                self.manifest_path = path.display().to_string();
                            }
                        }
                        if ui
                            .add_enabled(idle, egui::Button::new("从备份恢复 / 移除残影"))
                            .clicked()
                        {
                            self.run_restore();
                        }
                        if ui.button("打开报告").clicked() {
                            self.open_report();
                        }
                        if ui.button("打开 7z 目录").clicked() {
                            self.open_output();
                        }
                    });

                    ui.add_space(8.0);
                    self.show_task_progress(ui);

                    ui.add_space(8.0);
                    ui.group(|ui| {
                        ui.label("恢复");
                        path_row(
                            ui,
                            "manifest.json",
                            &mut self.manifest_path,
                            BrowseKind::OpenFile,
                            "应用修改时生成的清单；恢复/移除只处理清单里记录过的文件。",
                        );
                        path_row(
                            ui,
                            "恢复目标目录",
                            &mut self.restore_target,
                            BrowseKind::Folder,
                            "把备份文件恢复到这个目录；通常选择“修改输出目录”。",
                        );
                    });

                    ui.add_space(8.0);
                    ui.label("状态");
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .show(ui, |ui| {
                            ui.label(&self.status);
                        });
                });
        });
    }
}

impl GuiApp {
    fn poll_task(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        if let Some(task) = &mut self.task {
            while let Ok(message) = task.receiver.try_recv() {
                match message {
                    GuiTaskMessage::Progress { done, total, label } => {
                        task.done = done;
                        task.total = total;
                        task.label = label;
                    }
                    GuiTaskMessage::Finished(result) => {
                        finished = Some(result);
                        break;
                    }
                }
            }
        }

        if let Some(result) = finished {
            self.task = None;
            self.handle_task_result(result);
        }

        if self.task.is_some() {
            ctx.request_repaint();
        }
    }

    fn show_task_progress(&self, ui: &mut egui::Ui) {
        let Some(task) = &self.task else {
            return;
        };
        ui.group(|ui| {
            ui.label(egui::RichText::new(&task.title).strong());
            let fraction = if task.total == 0 {
                0.0
            } else {
                (task.done as f32 / task.total as f32).clamp(0.0, 1.0)
            };
            let text = if task.total == 0 {
                task.label.clone()
            } else {
                format!("{}  {}/{}", task.label, task.done, task.total)
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .animate(true)
                    .show_percentage()
                    .text(text),
            );
        });
        ui.add_space(8.0);
    }

    fn handle_task_result(&mut self, result: Result<GuiTaskResult, String>) {
        match result {
            Ok(result) => {
                self.last_report = Some(result.report_path.clone());
                if let Some(output_path) = result.output_path.clone() {
                    self.last_output = Some(output_path.clone());
                    self.restore_target = output_path.display().to_string();
                }
                if let Some(manifest_path) = result.manifest_path.clone() {
                    self.manifest_path = manifest_path.display().to_string();
                }

                let would_modify = result
                    .rows
                    .iter()
                    .filter(|row| row.decision == "would_modify")
                    .count();
                let modified = result
                    .rows
                    .iter()
                    .filter(|row| row.decision == "modified")
                    .count();
                let restored = result
                    .rows
                    .iter()
                    .filter(|row| row.decision == "restored")
                    .count();

                let mut status = match result.title.as_str() {
                    "扫描" => format!(
                        "扫描完成：{} 条记录，可修改 {} 个。\n报告：{}",
                        result.rows.len(),
                        would_modify,
                        result.report_path.display()
                    ),
                    "应用修改" => format!(
                        "应用完成：{} 条记录，已修改 {} 个。\n报告：{}",
                        result.rows.len(),
                        modified,
                        result.report_path.display()
                    ),
                    "恢复" => format!(
                        "恢复完成：{} 条记录，已恢复 {} 个。\n报告：{}",
                        result.rows.len(),
                        restored,
                        result.report_path.display()
                    ),
                    _ => format!("{}完成。", result.title),
                };

                if let Some(manifest_path) = result.manifest_path {
                    let _ = write!(status, "\n备份 manifest：{}", manifest_path.display());
                }
                if let Some(archive_path) = result.archive_path.clone() {
                    self.last_archive = Some(archive_path.clone());
                    let _ = write!(status, "\n打包 7z：{}", archive_path.display());
                }

                if !result.failures.is_empty() {
                    let _ = write!(
                        status,
                        "\n\n写入失败 {} 个：\n{}",
                        result.failures.len(),
                        result
                            .failures
                            .iter()
                            .take(30)
                            .map(|failure| format!("- {failure}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    if result.failures.len() > 30 {
                        let _ = write!(status, "\n... 其余失败请查看报告。");
                    }
                }

                self.status = status;
            }
            Err(err) => {
                self.status = err;
            }
        }
    }

    fn fill_default_paths(&mut self) {
        if self.export_root.trim().is_empty() {
            self.status = "请先选择 PVF 导出目录。".to_string();
            return;
        }
        let root = PathBuf::from(self.export_root.trim());
        self.output_root = default_output_root(&root).display().to_string();
        self.backup_root = root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("backup")
            .display()
            .to_string();
        self.report_path = PathBuf::from(&self.output_root)
            .join("report.csv")
            .display()
            .to_string();
        self.restore_target = self.output_root.clone();
    }

    fn sync_keyword_config(&mut self) {
        self.config.allow_small_frame_name_contains = split_keywords(&self.small_frame_keywords);
        self.config.blacklist_name_contains = split_keywords(&self.blacklist_keywords);
    }

    fn run_scan(&mut self) {
        if self.task.is_some() {
            return;
        }
        self.sync_keyword_config();
        let root = self.export_root.trim();
        if root.is_empty() {
            self.status = "扫描失败：PVF 导出目录不能为空。".to_string();
            return;
        }
        let root = PathBuf::from(root);
        let report = self.scan_report_path();
        let config = self.config.clone();
        let (sender, receiver) = mpsc::channel();
        self.last_archive = None;
        self.task = Some(TaskState {
            title: "扫描中".to_string(),
            receiver,
            done: 0,
            total: 0,
            label: "准备扫描...".to_string(),
        });
        self.status = "扫描中，请稍候。".to_string();

        thread::spawn(move || {
            let result = (|| -> Result<GuiTaskResult, String> {
                let rows = scan_with_progress(&root, &config, "scan", |done, total, label| {
                    let _ = sender.send(GuiTaskMessage::Progress {
                        done,
                        total,
                        label: label.to_string(),
                    });
                })
                .map_err(|err| format!("扫描失败：{err}"))?;
                write_report(&report, &rows).map_err(|err| format!("写入扫描报告失败：{err}"))?;
                Ok(GuiTaskResult {
                    title: "扫描".to_string(),
                    rows,
                    report_path: report,
                    manifest_path: None,
                    output_path: None,
                    archive_path: None,
                    failures: Vec::new(),
                })
            })();
            let _ = sender.send(GuiTaskMessage::Finished(result));
        });
    }

    fn run_apply(&mut self) {
        if self.task.is_some() {
            return;
        }
        self.sync_keyword_config();
        if self.export_root.trim().is_empty()
            || self.output_root.trim().is_empty()
            || self.backup_root.trim().is_empty()
        {
            self.status = "应用失败：导出目录、输出目录、备份目录都不能为空。".to_string();
            return;
        }
        let root = PathBuf::from(self.export_root.trim());
        let out = PathBuf::from(self.output_root.trim());
        let backup = PathBuf::from(self.backup_root.trim());
        let report = self.apply_report_path();
        let config = self.config.clone();
        let (sender, receiver) = mpsc::channel();
        self.task = Some(TaskState {
            title: "应用修改中".to_string(),
            receiver,
            done: 0,
            total: 0,
            label: "准备应用修改...".to_string(),
        });
        self.status = "应用修改中，请稍候。".to_string();

        thread::spawn(move || {
            let result = apply_with_progress_collect_errors(
                &root,
                &out,
                &backup,
                &report,
                &config,
                |done, total, label| {
                    let _ = sender.send(GuiTaskMessage::Progress {
                        done,
                        total,
                        label: label.to_string(),
                    });
                },
            )
            .map(
                |(rows, manifest_path, archive_path, failures)| GuiTaskResult {
                    title: "应用修改".to_string(),
                    rows,
                    report_path: report,
                    manifest_path: Some(manifest_path),
                    output_path: Some(out),
                    archive_path: Some(archive_path),
                    failures,
                },
            )
            .map_err(|err| format!("应用失败：{err}"));
            let _ = sender.send(GuiTaskMessage::Finished(result));
        });
    }

    fn run_restore(&mut self) {
        if self.manifest_path.trim().is_empty() || self.restore_target.trim().is_empty() {
            self.status = "恢复失败：manifest 和恢复目标目录不能为空。".to_string();
            return;
        }
        let report = PathBuf::from(self.restore_target.trim()).join("restore_report.csv");
        match restore(
            Path::new(self.manifest_path.trim()),
            Path::new(self.restore_target.trim()),
            &report,
        ) {
            Ok(rows) => {
                self.last_report = Some(report.clone());
                self.last_output = Some(PathBuf::from(self.restore_target.trim()));
                self.status = format!(
                    "恢复完成：{} 条记录，已恢复 {} 个。\n报告：{}",
                    rows.len(),
                    rows.iter().filter(|row| row.decision == "restored").count(),
                    report.display()
                );
            }
            Err(err) => self.status = format!("恢复失败：{err}"),
        }
    }

    fn scan_report_path(&self) -> PathBuf {
        if !self.report_path.trim().is_empty() {
            return PathBuf::from(self.report_path.trim());
        }
        if !self.output_root.trim().is_empty() {
            return PathBuf::from(self.output_root.trim()).join("report_scan.csv");
        }
        PathBuf::from("report_scan.csv")
    }

    fn apply_report_path(&self) -> PathBuf {
        if !self.report_path.trim().is_empty() {
            return PathBuf::from(self.report_path.trim());
        }
        PathBuf::from(self.output_root.trim()).join("report.csv")
    }

    fn open_report(&mut self) {
        let Some(path) = self.last_report.clone().or_else(|| {
            (!self.report_path.trim().is_empty()).then(|| PathBuf::from(self.report_path.trim()))
        }) else {
            self.status = "没有可打开的报告。".to_string();
            return;
        };
        if let Err(err) = Command::new("notepad").arg(&path).spawn() {
            self.status = format!("打开报告失败：{err}");
        }
    }

    fn open_output(&mut self) {
        let Some(path) = self
            .last_archive
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .or_else(|| {
                self.last_output
                    .as_ref()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
            })
            .or_else(|| {
                if self.output_root.trim().is_empty() {
                    None
                } else {
                    let output_root = PathBuf::from(self.output_root.trim());
                    Some(
                        output_root
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or(output_root),
                    )
                }
            })
        else {
            self.status = "没有可打开的 7z 目录。".to_string();
            return;
        };
        if let Err(err) = Command::new("explorer").arg(&path).spawn() {
            self.status = format!("打开 7z 所在目录失败：{err}");
        }
    }
}

enum BrowseKind {
    Folder,
    OpenFile,
    SaveFile,
}

fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, kind: BrowseKind, help: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([130.0, 30.0], egui::Label::new(label));
        let input_width = (ui.available_width() - 92.0).max(220.0);
        ui.add_sized([input_width, 30.0], egui::TextEdit::singleline(value))
            .on_hover_text(help);
        if ui.button("浏览...").clicked() {
            let picked = match kind {
                BrowseKind::Folder => rfd::FileDialog::new().pick_folder(),
                BrowseKind::OpenFile => rfd::FileDialog::new().pick_file(),
                BrowseKind::SaveFile => rfd::FileDialog::new().save_file(),
            };
            if let Some(path) = picked {
                *value = path.display().to_string();
            }
        }
    });
    help_text(ui, help);
}

fn help_text(ui: &mut egui::Ui, text: &str) {
    ui.add_space(1.0);
    ui.label(egui::RichText::new(text).small().weak());
}

fn split_keywords(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn existing_policy_label(policy: ExistingPolicy) -> &'static str {
    match policy {
        ExistingPolicy::Skip => "skip",
        ExistingPolicy::Update => "update",
        ExistingPolicy::Force => "force",
    }
}

fn default_output_root(root: &Path) -> PathBuf {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let name = root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("pvf_export");
    parent.join(format!("{name}_spectrum"))
}

fn print_usage() {
    eprintln!(
        "Usage:
  spectrum_tool scan --root <export_root> [--config config.json] [--report report_scan.csv]
  spectrum_tool apply --root <export_root> --out <output_root> --backup <backup_root> [--config config.json] [--report report.csv] [--existing skip|update|force]
  spectrum_tool restore --manifest <manifest.json> --target <output_root> [--report restore_report.csv]
  spectrum_tool remove --manifest <manifest.json> --target <output_root> [--report restore_report.csv]
  spectrum_tool default-config"
    );
}

fn required_arg(args: &[String], name: &str) -> AppResult<String> {
    optional_arg(args, name).ok_or_else(|| format!("missing required argument {name}").into())
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn apply_cli_overrides(config: &mut Config, args: &[String]) {
    if let Some(existing) = optional_arg(args, "--existing") {
        if let Some(policy) = parse_existing_policy(&existing) {
            config.existing_policy = policy;
        }
    }
}

fn load_config(path: Option<String>) -> AppResult<Config> {
    let mut config = Config::default();
    let Some(path) = path else {
        return Ok(config);
    };

    let text = fs::read_to_string(path)?;
    if let Some(enabled) = json_bool(&text, "enabled") {
        config.spectrum.enabled = enabled;
    }
    if let Some(term) = json_u32(&text, "term") {
        config.spectrum.term = term;
    }
    if let Some(life_time) = json_u32(&text, "life_time") {
        config.spectrum.life_time = life_time;
    }
    if let Some(color) = json_u32_array(&text, "color") {
        if color.len() == 4 {
            config.spectrum.color = [color[0], color[1], color[2], color[3]];
        }
    }
    if let Some(effect) = json_string(&text, "effect") {
        config.spectrum.effect = effect;
    }
    if let Some(policy) = json_string(&text, "existing_spectrum_policy")
        .and_then(|value| parse_existing_policy(&value))
    {
        config.existing_policy = policy;
    }
    if let Some(min_frame_max) = json_u32(&text, "min_frame_max") {
        config.min_frame_max = min_frame_max;
    }
    if let Some(values) = json_string_array(&text, "allow_small_frame_name_contains") {
        config.allow_small_frame_name_contains = values;
    }
    if let Some(values) = json_string_array(&text, "blacklist_name_contains") {
        config.blacklist_name_contains = values;
    }
    if let Some(case_insensitive) = json_bool(&text, "case_insensitive") {
        config.case_insensitive = case_insensitive;
    }

    Ok(config)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            spectrum: SpectrumConfig {
                enabled: true,
                term: 70,
                life_time: 250,
                color: [255, 255, 255, 120],
                effect: "NONE".to_string(),
            },
            existing_policy: ExistingPolicy::Skip,
            min_frame_max: 4,
            allow_small_frame_name_contains: vec![
                "Dash".to_string(),
                "Move".to_string(),
                "Run".to_string(),
                "Step".to_string(),
                "Slash".to_string(),
                "Charge".to_string(),
                "Rush".to_string(),
                "Jump".to_string(),
                "Attack".to_string(),
            ],
            blacklist_name_contains: vec![
                "dummy".to_string(),
                "Ghost.ani".to_string(),
                "Ghost_Dodge".to_string(),
                "GetItem".to_string(),
                "Sit".to_string(),
                "Rest".to_string(),
                "SIMPLE_Rest".to_string(),
                "Stay".to_string(),
                "Damage".to_string(),
                "Down".to_string(),
                "Overturn".to_string(),
            ],
            case_insensitive: true,
        }
    }
}

fn parse_existing_policy(value: &str) -> Option<ExistingPolicy> {
    match value.to_ascii_lowercase().as_str() {
        "skip" => Some(ExistingPolicy::Skip),
        "update" => Some(ExistingPolicy::Update),
        "force" => Some(ExistingPolicy::Force),
        _ => None,
    }
}

fn scan(root: &Path, config: &Config, mode: &str) -> AppResult<Vec<ReportRow>> {
    let contexts = load_chr_contexts(root)?;
    let mut rows = Vec::new();
    for context in contexts {
        for ani_ref in &context.ani_refs {
            let row = evaluate_ani(root, &context, ani_ref, config, mode)?;
            rows.push(row);
        }
    }
    Ok(rows)
}

fn scan_with_progress<F>(
    root: &Path,
    config: &Config,
    mode: &str,
    mut progress: F,
) -> AppResult<Vec<ReportRow>>
where
    F: FnMut(usize, usize, &str),
{
    let contexts = load_chr_contexts(root)?;
    let total = contexts
        .iter()
        .map(|context| context.ani_refs.len())
        .sum::<usize>();
    let mut rows = Vec::new();
    let mut done = 0;
    progress(done, total, "开始扫描 ANI 引用");

    for context in contexts {
        for ani_ref in &context.ani_refs {
            let row = evaluate_ani(root, &context, ani_ref, config, mode)?;
            rows.push(row);
            done += 1;
            progress(done, total, &format!("扫描 {}", context.rel_chr_path));
        }
    }

    Ok(rows)
}

fn apply(
    root: &Path,
    out: &Path,
    backup_root: &Path,
    report_path: &Path,
    config: &Config,
) -> AppResult<(Vec<ReportRow>, PathBuf, PathBuf)> {
    if path_is_same_or_inside(out, root)? {
        return Err("output root must not be the same as, or inside, source root".into());
    }
    if path_is_same_or_inside(backup_root, root)? {
        return Err("backup root must not be the same as, or inside, source root".into());
    }
    if path_is_inside(report_path, root)? {
        return Err("report path must not be inside source root".into());
    }

    fs::create_dir_all(out)?;
    copy_tree(root, out)?;

    let batch_name = timestamp_name();
    let backup_batch = backup_root.join(batch_name);
    let backup_files = backup_batch.join("files");
    fs::create_dir_all(&backup_files)?;

    let contexts = load_chr_contexts(root)?;
    let mut rows = Vec::new();
    let mut manifest_entries = Vec::new();
    let mut written = HashSet::new();

    for context in contexts {
        for ani_ref in &context.ani_refs {
            let mut row = evaluate_ani(root, &context, ani_ref, config, "apply")?;

            if row.decision != "would_modify" {
                rows.push(row);
                continue;
            }

            let Some(ani_path) = resolve_ani_path(&context.chr_dir, ani_ref) else {
                row.decision = "skipped".to_string();
                row.reason = "invalid_ani_path".to_string();
                rows.push(row);
                continue;
            };

            let rel = relative_path(root, &ani_path)?;
            let rel_text = normalize_path_text(&rel);
            if written.contains(&rel_text) {
                row.decision = "skipped".to_string();
                row.reason = "duplicate_ani_reference".to_string();
                rows.push(row);
                continue;
            }
            let src_bytes = fs::read(&ani_path)?;
            let backup_file = backup_files.join(&rel);
            if let Some(parent) = backup_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&ani_path, &backup_file)?;

            let action = if contains_marker(&src_bytes, SPECTRUM, config.case_insensitive) {
                match config.existing_policy {
                    ExistingPolicy::Update => "update",
                    ExistingPolicy::Force => "force",
                    ExistingPolicy::Skip => "skip",
                }
            } else {
                "insert"
            };

            let new_bytes = rewrite_spectrum(&src_bytes, &config.spectrum, config.existing_policy)?;
            let out_file = out.join(&rel);
            if let Some(parent) = out_file.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out_file, &new_bytes)?;

            let sha_before = sha256_hex(&src_bytes);
            let sha_after = sha256_hex(&new_bytes);
            let manifest_backup_path = absolute_path_text(&backup_file)?;

            row.decision = "modified".to_string();
            row.reason = format!("{action}ed");
            row.sha256_before = sha_before.clone();
            row.sha256_after = sha_after.clone();
            row.backup_path = manifest_backup_path.clone();

            written.insert(rel_text.clone());
            manifest_entries.push(ManifestEntry {
                chr_path: context.rel_chr_path.clone(),
                job: context.job.clone(),
                body_image_path: context.body_path_text.clone(),
                ani_path: rel_text,
                backup_path: manifest_backup_path,
                frame_max: row.frame_max.parse().unwrap_or_default(),
                action: action.to_string(),
                sha256_before: sha_before,
                sha256_after: sha_after,
            });
            rows.push(row);
        }
    }

    write_report(report_path, &rows)?;

    let manifest = Manifest {
        created_at: human_timestamp(),
        mode: "apply".to_string(),
        source_root: root.display().to_string(),
        output_root: out.display().to_string(),
        config: config.clone(),
        files: manifest_entries,
    };

    let manifest_text = manifest.to_json();
    let backup_manifest = backup_batch.join("manifest.json");
    fs::write(&backup_manifest, &manifest_text)?;
    fs::write(out.join("manifest.json"), &manifest_text)?;
    let archive_path = default_7z_path(out);
    create_output_7z_from_source(root, out, &archive_path)?;

    Ok((rows, backup_manifest, archive_path))
}

fn apply_with_progress_collect_errors<F>(
    root: &Path,
    out: &Path,
    backup_root: &Path,
    report_path: &Path,
    config: &Config,
    mut progress: F,
) -> AppResult<(Vec<ReportRow>, PathBuf, PathBuf, Vec<String>)>
where
    F: FnMut(usize, usize, &str),
{
    if path_is_same_or_inside(out, root)? {
        return Err("output root must not be the same as, or inside, source root".into());
    }
    if path_is_same_or_inside(backup_root, root)? {
        return Err("backup root must not be the same as, or inside, source root".into());
    }
    if path_is_inside(report_path, root)? {
        return Err("report path must not be inside source root".into());
    }

    let mut failures = Vec::new();
    progress(0, 0, "复制原始目录到输出目录");
    fs::create_dir_all(out)?;
    copy_tree_collect_errors(root, out, &mut failures);

    let batch_name = timestamp_name();
    let backup_batch = backup_root.join(batch_name);
    let backup_files = backup_batch.join("files");
    fs::create_dir_all(&backup_files)?;

    let contexts = load_chr_contexts(root)?;
    let total = contexts
        .iter()
        .map(|context| context.ani_refs.len())
        .sum::<usize>();
    let mut rows = Vec::new();
    let mut manifest_entries = Vec::new();
    let mut written = HashSet::new();
    let mut done = 0;
    progress(done, total, "开始应用修改");

    for context in contexts {
        for ani_ref in &context.ani_refs {
            let mut row = evaluate_ani(root, &context, ani_ref, config, "apply")?;

            if row.decision != "would_modify" {
                rows.push(row);
                done += 1;
                progress(done, total, &format!("检查 {}", context.rel_chr_path));
                continue;
            }

            let Some(ani_path) = resolve_ani_path(&context.chr_dir, ani_ref) else {
                row.decision = "skipped".to_string();
                row.reason = "invalid_ani_path".to_string();
                rows.push(row);
                done += 1;
                progress(done, total, &format!("跳过 {}", context.rel_chr_path));
                continue;
            };

            let rel = match relative_path(root, &ani_path) {
                Ok(rel) => rel,
                Err(err) => {
                    push_failed_row(
                        &mut row,
                        "relative_path_failed",
                        &err.to_string(),
                        &mut failures,
                    );
                    rows.push(row);
                    done += 1;
                    progress(done, total, &format!("失败 {}", context.rel_chr_path));
                    continue;
                }
            };

            let rel_text = normalize_path_text(&rel);
            if written.contains(&rel_text) {
                row.decision = "skipped".to_string();
                row.reason = "duplicate_ani_reference".to_string();
                rows.push(row);
                done += 1;
                progress(done, total, &format!("跳过重复 {}", rel_text));
                continue;
            }

            let src_bytes = match fs::read(&ani_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    push_failed_row(&mut row, "read_failed", &err.to_string(), &mut failures);
                    rows.push(row);
                    done += 1;
                    progress(done, total, &format!("读取失败 {}", rel_text));
                    continue;
                }
            };

            let backup_file = backup_files.join(&rel);
            if let Some(parent) = backup_file.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    push_failed_row(
                        &mut row,
                        "backup_dir_failed",
                        &err.to_string(),
                        &mut failures,
                    );
                    rows.push(row);
                    done += 1;
                    progress(done, total, &format!("备份失败 {}", rel_text));
                    continue;
                }
            }
            if let Err(err) = fs::copy(&ani_path, &backup_file) {
                push_failed_row(
                    &mut row,
                    "backup_copy_failed",
                    &err.to_string(),
                    &mut failures,
                );
                rows.push(row);
                done += 1;
                progress(done, total, &format!("备份失败 {}", rel_text));
                continue;
            }

            let action = if contains_marker(&src_bytes, SPECTRUM, config.case_insensitive) {
                match config.existing_policy {
                    ExistingPolicy::Update => "update",
                    ExistingPolicy::Force => "force",
                    ExistingPolicy::Skip => "skip",
                }
            } else {
                "insert"
            };

            let new_bytes =
                match rewrite_spectrum(&src_bytes, &config.spectrum, config.existing_policy) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        push_failed_row(
                            &mut row,
                            "rewrite_failed",
                            &err.to_string(),
                            &mut failures,
                        );
                        rows.push(row);
                        done += 1;
                        progress(done, total, &format!("生成失败 {}", rel_text));
                        continue;
                    }
                };

            let out_file = out.join(&rel);
            if let Some(parent) = out_file.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    push_failed_row(
                        &mut row,
                        "output_dir_failed",
                        &err.to_string(),
                        &mut failures,
                    );
                    rows.push(row);
                    done += 1;
                    progress(done, total, &format!("写入失败 {}", rel_text));
                    continue;
                }
            }
            if let Err(err) = fs::write(&out_file, &new_bytes) {
                push_failed_row(
                    &mut row,
                    "output_write_failed",
                    &err.to_string(),
                    &mut failures,
                );
                rows.push(row);
                done += 1;
                progress(done, total, &format!("写入失败 {}", rel_text));
                continue;
            }

            let sha_before = sha256_hex(&src_bytes);
            let sha_after = sha256_hex(&new_bytes);
            let manifest_backup_path = match absolute_path_text(&backup_file) {
                Ok(path) => path,
                Err(err) => {
                    push_failed_row(
                        &mut row,
                        "backup_path_failed",
                        &err.to_string(),
                        &mut failures,
                    );
                    rows.push(row);
                    done += 1;
                    progress(done, total, &format!("记录失败 {}", rel_text));
                    continue;
                }
            };

            row.decision = "modified".to_string();
            row.reason = format!("{action}ed");
            row.sha256_before = sha_before.clone();
            row.sha256_after = sha_after.clone();
            row.backup_path = manifest_backup_path.clone();

            written.insert(rel_text.clone());
            manifest_entries.push(ManifestEntry {
                chr_path: context.rel_chr_path.clone(),
                job: context.job.clone(),
                body_image_path: context.body_path_text.clone(),
                ani_path: rel_text.clone(),
                backup_path: manifest_backup_path,
                frame_max: row.frame_max.parse().unwrap_or_default(),
                action: action.to_string(),
                sha256_before: sha_before,
                sha256_after: sha_after,
            });
            rows.push(row);
            done += 1;
            progress(done, total, &format!("修改 {}", rel_text));
        }
    }

    progress(total, total, "写入报告和 manifest");
    write_report(report_path, &rows)?;

    let manifest = Manifest {
        created_at: human_timestamp(),
        mode: "apply".to_string(),
        source_root: root.display().to_string(),
        output_root: out.display().to_string(),
        config: config.clone(),
        files: manifest_entries,
    };

    let manifest_text = manifest.to_json();
    let backup_manifest = backup_batch.join("manifest.json");
    fs::write(&backup_manifest, &manifest_text)?;
    fs::write(out.join("manifest.json"), &manifest_text)?;
    let archive_path = default_7z_path(out);
    progress(total, total, "打包 7z");
    if let Err(err) =
        create_output_7z_from_source_collect_errors(root, out, &archive_path, &mut failures)
    {
        failures.push(format!("创建 7z 失败：{} -> {err}", archive_path.display()));
    }

    Ok((rows, backup_manifest, archive_path, failures))
}

fn restore(manifest_path: &Path, target: &Path, report_path: &Path) -> AppResult<Vec<ReportRow>> {
    let text = fs::read_to_string(manifest_path)?;
    let entries = parse_manifest_entries(&text);
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut rows = Vec::new();

    for entry in entries {
        let backup_path = resolve_manifest_path(manifest_dir, &entry.backup_path);
        let target_path = target.join(path_from_manifest(&entry.ani_path));
        let backup_bytes = fs::read(&backup_path);

        match backup_bytes {
            Ok(bytes) => {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&target_path, &bytes)?;
                rows.push(ReportRow {
                    mode: "restore".to_string(),
                    chr_path: entry.chr_path,
                    job: entry.job,
                    body_image_path: entry.body_image_path,
                    ani_path: entry.ani_path,
                    frame_max: entry.frame_max.to_string(),
                    has_spectrum_before: false,
                    decision: "restored".to_string(),
                    reason: "backup_restored".to_string(),
                    sha256_before: entry.sha256_after,
                    sha256_after: sha256_hex(&bytes),
                    backup_path: entry.backup_path,
                });
            }
            Err(err) => {
                rows.push(ReportRow {
                    mode: "restore".to_string(),
                    chr_path: entry.chr_path,
                    job: entry.job,
                    body_image_path: entry.body_image_path,
                    ani_path: entry.ani_path,
                    frame_max: entry.frame_max.to_string(),
                    has_spectrum_before: false,
                    decision: "skipped".to_string(),
                    reason: format!("missing_backup:{err}"),
                    sha256_before: entry.sha256_after,
                    sha256_after: String::new(),
                    backup_path: entry.backup_path,
                });
            }
        }
    }

    write_report(report_path, &rows)?;
    Ok(rows)
}

fn load_chr_contexts(root: &Path) -> AppResult<Vec<ChrContext>> {
    let mut chr_files = Vec::new();
    walk_files_with_ext(root, "chr", &mut chr_files)?;
    chr_files.sort();

    let mut contexts = Vec::new();
    for chr_path in chr_files {
        let bytes = fs::read(&chr_path)?;
        let rel_chr_path = normalize_path_text(&relative_path(root, &chr_path)?);
        let chr_dir = chr_path
            .parent()
            .ok_or_else(|| format!("chr path has no parent: {}", chr_path.display()))?
            .to_path_buf();
        let job = extract_section_backtick_value(&bytes, b"[job]")
            .map(|value| display_bytes(&value))
            .unwrap_or_default();
        let Some(body_path) = extract_section_backtick_value(&bytes, b"[body image path]") else {
            contexts.push(ChrContext {
                rel_chr_path,
                chr_dir,
                job,
                body_path: Vec::new(),
                body_path_text: String::new(),
                ani_refs: Vec::new(),
            });
            continue;
        };
        let ani_refs = extract_ani_refs(&bytes);
        contexts.push(ChrContext {
            rel_chr_path,
            chr_dir,
            job,
            body_path_text: display_bytes(&body_path),
            body_path,
            ani_refs,
        });
    }

    Ok(contexts)
}

fn evaluate_ani(
    root: &Path,
    context: &ChrContext,
    ani_ref: &[u8],
    config: &Config,
    mode: &str,
) -> AppResult<ReportRow> {
    let ani_path = resolve_ani_path(&context.chr_dir, ani_ref);
    let ani_rel = ani_path
        .as_ref()
        .and_then(|path| relative_path(root, path).ok())
        .map(|path| normalize_path_text(&path))
        .unwrap_or_else(|| display_bytes(ani_ref));

    let mut row = ReportRow {
        mode: mode.to_string(),
        chr_path: context.rel_chr_path.clone(),
        job: context.job.clone(),
        body_image_path: context.body_path_text.clone(),
        ani_path: ani_rel,
        frame_max: String::new(),
        has_spectrum_before: false,
        decision: "skipped".to_string(),
        reason: String::new(),
        sha256_before: String::new(),
        sha256_after: String::new(),
        backup_path: String::new(),
    };

    if context.body_path.is_empty() {
        row.reason = "skip_chr_no_body_path".to_string();
        return Ok(row);
    }

    let Some(ani_path) = ani_path else {
        row.reason = "invalid_ani_path".to_string();
        return Ok(row);
    };

    let bytes = match fs::read(&ani_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            row.reason = "missing_ani".to_string();
            return Ok(row);
        }
        Err(err) => return Err(err.into()),
    };

    let sha_before = sha256_hex(&bytes);
    row.sha256_before = sha_before.clone();
    row.sha256_after = sha_before;

    if !contains_body_image_path(&bytes, &context.body_path, config.case_insensitive) {
        row.reason = "skip_not_body_ani".to_string();
        return Ok(row);
    }

    if let Some(hit) = blacklist_keyword_hit(
        &ani_path,
        &config.blacklist_name_contains,
        config.case_insensitive,
    ) {
        row.reason = format!("blacklist:{hit}");
        return Ok(row);
    }

    let Some(frame_max) = parse_frame_max(&bytes) else {
        row.reason = "invalid_no_frame_max".to_string();
        return Ok(row);
    };
    row.frame_max = frame_max.to_string();

    if frame_max <= 2 {
        row.reason = "frame<=2".to_string();
        return Ok(row);
    }
    if frame_max == 3
        && keyword_hit(
            &ani_path,
            &config.allow_small_frame_name_contains,
            config.case_insensitive,
        )
        .is_none()
    {
        row.reason = "frame_3_not_allowed".to_string();
        return Ok(row);
    }
    if frame_max > 3 && frame_max < config.min_frame_max {
        row.reason = format!("frame<{}", config.min_frame_max);
        return Ok(row);
    }

    row.has_spectrum_before = contains_marker(&bytes, SPECTRUM, config.case_insensitive);
    if row.has_spectrum_before && config.existing_policy == ExistingPolicy::Skip {
        row.reason = "existing_spectrum_skip".to_string();
        return Ok(row);
    }
    if !config.spectrum.enabled {
        row.reason = "spectrum_disabled".to_string();
        return Ok(row);
    }

    row.decision = "would_modify".to_string();
    row.reason = if row.has_spectrum_before {
        match config.existing_policy {
            ExistingPolicy::Update => "existing_spectrum_update".to_string(),
            ExistingPolicy::Force => "existing_spectrum_force".to_string(),
            ExistingPolicy::Skip => "existing_spectrum_skip".to_string(),
        }
    } else {
        "insert".to_string()
    };

    Ok(row)
}

fn rewrite_spectrum(
    bytes: &[u8],
    spectrum: &SpectrumConfig,
    policy: ExistingPolicy,
) -> AppResult<Vec<u8>> {
    let mut base = bytes.to_vec();
    if contains_marker(&base, SPECTRUM, true) {
        match policy {
            ExistingPolicy::Skip => return Ok(base),
            ExistingPolicy::Update | ExistingPolicy::Force => {
                base = remove_spectrum_block(&base);
            }
        }
    }
    insert_spectrum_before_frame_max(&base, spectrum)
}

fn insert_spectrum_before_frame_max(bytes: &[u8], spectrum: &SpectrumConfig) -> AppResult<Vec<u8>> {
    let Some(pos) = find_ascii_case_insensitive(bytes, FRAME_MAX) else {
        return Err("cannot insert spectrum: missing [FRAME MAX]".into());
    };
    let newline = detect_newline(bytes);
    let block = spectrum_block(spectrum, newline);
    let mut out = Vec::with_capacity(bytes.len() + block.len());
    out.extend_from_slice(&bytes[..pos]);
    if pos > 0 && !ends_with_newline(&out) {
        out.extend_from_slice(newline.as_bytes());
    }
    out.extend_from_slice(block.as_bytes());
    out.extend_from_slice(&bytes[pos..]);
    Ok(out)
}

fn remove_spectrum_block(bytes: &[u8]) -> Vec<u8> {
    let Some(start) = find_ascii_case_insensitive(bytes, SPECTRUM) else {
        return bytes.to_vec();
    };
    let end = find_next_top_level_section(bytes, start + SPECTRUM.len());
    let end = end.unwrap_or(bytes.len());
    let mut out = Vec::with_capacity(bytes.len().saturating_sub(end - start));
    out.extend_from_slice(&bytes[..start]);
    out.extend_from_slice(&bytes[end..]);
    out
}

fn spectrum_block(config: &SpectrumConfig, newline: &str) -> String {
    format!(
        "[SPECTRUM]{newline}\t1{newline}\t[SPECTRUM TERM]{newline}\t\t{term}{newline}\t[SPECTRUM LIFE TIME]{newline}\t\t{life_time}{newline}\t[SPECTRUM COLOR]{newline}\t\t{r}\t{g}\t{b}\t{a}{newline}\t[SPECTRUM EFFECT]{newline}\t\t`{effect}`{newline}",
        term = config.term,
        life_time = config.life_time,
        r = config.color[0],
        g = config.color[1],
        b = config.color[2],
        a = config.color[3],
        effect = config.effect,
    )
}

fn extract_section_backtick_value(bytes: &[u8], section: &[u8]) -> Option<Vec<u8>> {
    let start = find_ascii_case_insensitive(bytes, section)?;
    let after = start + section.len();
    let end = find_next_top_level_section(bytes, after).unwrap_or(bytes.len());
    let area = &bytes[after..end];
    let first_tick = area.iter().position(|b| *b == b'`')? + after;
    let second_tick = bytes[first_tick + 1..end].iter().position(|b| *b == b'`')? + first_tick + 1;
    Some(bytes[first_tick + 1..second_tick].to_vec())
}

fn extract_ani_refs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0;
    while let Some(open_rel) = bytes[index..].iter().position(|b| *b == b'`') {
        let open = index + open_rel;
        let Some(close_rel) = bytes[open + 1..].iter().position(|b| *b == b'`') else {
            break;
        };
        let close = open + 1 + close_rel;
        let value = &bytes[open + 1..close];
        if ascii_ends_with(value, b".ani") {
            let key = normalize_path_bytes(value, true);
            if seen.insert(key) {
                refs.push(value.to_vec());
            }
        }
        index = close + 1;
    }
    refs
}

fn parse_frame_max(bytes: &[u8]) -> Option<u32> {
    let start = find_ascii_case_insensitive(bytes, FRAME_MAX)? + FRAME_MAX.len();
    let end = find_next_top_level_section(bytes, start).unwrap_or(bytes.len());
    let rest = &bytes[start..end];
    let digit_start = rest.iter().position(|b| b.is_ascii_digit())?;
    let mut end = digit_start;
    while end < rest.len() && rest[end].is_ascii_digit() {
        end += 1;
    }
    std::str::from_utf8(&rest[digit_start..end])
        .ok()?
        .parse()
        .ok()
}

fn contains_normalized_path(haystack: &[u8], needle: &[u8], case_insensitive: bool) -> bool {
    let haystack = normalize_path_bytes(haystack, case_insensitive);
    let needle = normalize_path_bytes(needle, case_insensitive);
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn contains_body_image_path(haystack: &[u8], body_path: &[u8], case_insensitive: bool) -> bool {
    if contains_normalized_path(haystack, body_path, case_insensitive) {
        return true;
    }

    let haystack = normalize_path_bytes(haystack, case_insensitive);
    let body_path = normalize_path_bytes(body_path, case_insensitive);
    let Some(token_pos) = find_bytes(&body_path, b"%04d") else {
        return false;
    };
    let prefix = &body_path[..token_pos];
    let suffix = &body_path[token_pos + 4..];

    let mut index = 0;
    while let Some(prefix_offset) = find_bytes(&haystack[index..], prefix) {
        let start = index + prefix_offset;
        let digits_start = start + prefix.len();
        let digits_end = digits_start + 4;
        if digits_end <= haystack.len()
            && haystack[digits_start..digits_end]
                .iter()
                .all(u8::is_ascii_digit)
            && haystack[digits_end..].starts_with(suffix)
        {
            return true;
        }
        index = start + 1;
    }

    false
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn normalize_path_bytes(bytes: &[u8], case_insensitive: bool) -> Vec<u8> {
    bytes
        .iter()
        .map(|b| match *b {
            b'\\' => b'/',
            value if case_insensitive => value.to_ascii_lowercase(),
            value => value,
        })
        .collect()
}

fn resolve_ani_path(chr_dir: &Path, ani_ref: &[u8]) -> Option<PathBuf> {
    let text = display_bytes(ani_ref).replace('\\', "/");
    if text.trim().is_empty() {
        return None;
    }
    if text.starts_with('/') || text.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut path = PathBuf::from(chr_dir);
    for part in text.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

fn keyword_hit(path: &Path, keywords: &[String], case_insensitive: bool) -> Option<String> {
    let text = path
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .unwrap_or_else(|| normalize_slashes(&path.display().to_string()));
    let haystack = if case_insensitive {
        text.to_ascii_lowercase()
    } else {
        text
    };
    keywords.iter().find_map(|keyword| {
        let needle = if case_insensitive {
            keyword.to_ascii_lowercase()
        } else {
            keyword.clone()
        };
        haystack.contains(&needle).then(|| keyword.clone())
    })
}

fn blacklist_keyword_hit(
    path: &Path,
    keywords: &[String],
    case_insensitive: bool,
) -> Option<String> {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .unwrap_or_else(|| normalize_slashes(&path.display().to_string()));
    let haystack = if case_insensitive {
        file_name.to_ascii_lowercase()
    } else {
        file_name.clone()
    };

    keywords.iter().find_map(|keyword| {
        let needle = if case_insensitive {
            keyword.to_ascii_lowercase()
        } else {
            keyword.clone()
        };
        find_keyword_boundary_match(&file_name, &haystack, &needle).then(|| keyword.clone())
    })
}

fn find_keyword_boundary_match(original: &str, haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }

    let mut search_start = 0;
    while let Some(offset) = haystack[search_start..].find(needle) {
        let start = search_start + offset;
        let end = start + needle.len();
        if keyword_boundary_ok(original.as_bytes(), start, end) {
            return true;
        }
        search_start = start + 1;
    }
    false
}

fn keyword_boundary_ok(original: &[u8], start: usize, end: usize) -> bool {
    let before_ok = start == 0
        || !original[start - 1].is_ascii_alphanumeric()
        || (original[start].is_ascii_uppercase() && original[start - 1].is_ascii_lowercase());
    let after_ok = end >= original.len()
        || !original[end].is_ascii_alphanumeric()
        || (original[end].is_ascii_uppercase() && original[end - 1].is_ascii_lowercase());
    before_ok || after_ok
}

fn contains_marker(bytes: &[u8], marker: &[u8], case_insensitive: bool) -> bool {
    if case_insensitive {
        find_ascii_case_insensitive(bytes, marker).is_some()
    } else {
        bytes.windows(marker.len()).any(|window| window == marker)
    }
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| ascii_eq_ignore_case(window, needle))
}

fn find_next_top_level_section(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        if (index == 0 || bytes[index - 1] == b'\n') && bytes[index] == b'[' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn walk_files_with_ext(root: &Path, ext: &str, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_files_with_ext(&path, ext, out)?;
        } else if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(ext))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(&source_path, &dest_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &dest_path)?;
        }
    }
    Ok(())
}

fn copy_tree_collect_errors(src: &Path, dst: &Path, failures: &mut Vec<String>) {
    if let Err(err) = fs::create_dir_all(dst) {
        failures.push(format!("创建输出目录失败：{} -> {err}", dst.display()));
        return;
    }
    let entries = match fs::read_dir(src) {
        Ok(entries) => entries,
        Err(err) => {
            failures.push(format!("读取目录失败：{} -> {err}", src.display()));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                failures.push(format!("读取目录项失败：{} -> {err}", src.display()));
                continue;
            }
        };
        let source_path = entry.path();
        let dest_path = dst.join(entry.file_name());
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                failures.push(format!(
                    "读取文件类型失败：{} -> {err}",
                    source_path.display()
                ));
                continue;
            }
        };
        if file_type.is_dir() {
            copy_tree_collect_errors(&source_path, &dest_path, failures);
        } else if file_type.is_file() {
            if let Some(parent) = dest_path.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    failures.push(format!("创建输出目录失败：{} -> {err}", parent.display()));
                    continue;
                }
            }
            if let Err(err) = fs::copy(&source_path, &dest_path) {
                failures.push(format!(
                    "复制文件失败：{} -> {}：{err}",
                    source_path.display(),
                    dest_path.display()
                ));
            }
        }
    }
}

fn default_7z_path(out: &Path) -> PathBuf {
    let name = out
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("output");
    out.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}.7z"))
}

fn create_output_7z_from_source(
    source_root: &Path,
    output_root: &Path,
    archive_path: &Path,
) -> AppResult<()> {
    let mut failures = Vec::new();
    create_output_7z_from_source_collect_errors(
        source_root,
        output_root,
        archive_path,
        &mut failures,
    )?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

fn create_output_7z_from_source_collect_errors(
    source_root: &Path,
    output_root: &Path,
    archive_path: &Path,
    failures: &mut Vec<String>,
) -> AppResult<()> {
    let seven_zip = find_7z_command()
        .ok_or_else(|| "未找到 7z.exe。请安装 7-Zip，或把 7z.exe 加入 PATH。".to_string())?;

    let mut source_files = Vec::new();
    collect_all_files(source_root, &mut source_files)?;
    source_files.sort();

    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut list_content = vec![0xef, 0xbb, 0xbf];
    for source_file in source_files {
        let rel = match relative_path(source_root, &source_file) {
            Ok(rel) => rel,
            Err(err) => {
                failures.push(format!(
                    "计算 7z 相对路径失败：{} -> {err}",
                    source_file.display()
                ));
                continue;
            }
        };
        let output_file = output_root.join(&rel);
        let entry_name = normalize_path_text(&rel);
        if let Err(err) = fs::metadata(&output_file) {
            failures.push(format!(
                "7z 缺少输出文件：{} -> {}：{err}",
                entry_name,
                output_file.display()
            ));
            continue;
        }
        list_content.extend_from_slice(entry_name.as_bytes());
        list_content.extend_from_slice(b"\r\n");
    }

    if list_content.len() <= 3 {
        return Err("没有可打包的输入文件。".into());
    }

    if archive_path.exists() {
        fs::remove_file(archive_path)?;
    }

    let list_path = archive_path.with_extension("7z.list.txt");
    fs::write(&list_path, list_content)?;

    let output = command_no_window(&seven_zip)
        .current_dir(output_root)
        .arg("a")
        .arg("-t7z")
        .arg("-mx=9")
        .arg("-y")
        .arg("-scsUTF-8")
        .arg(archive_path)
        .arg(format!("@{}", list_path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let _ = fs::remove_file(&list_path);
    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("7z 打包失败：{}\n{}", stderr.trim(), stdout.trim()).into());
    }
    Ok(())
}

fn collect_all_files(root: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_all_files(&path, out)?;
        } else if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn find_7z_command() -> Option<PathBuf> {
    for candidate in ["7z.exe", "7za.exe", "7zr.exe", "7z", "7za", "7zr"] {
        let output = command_no_window(Path::new(candidate))
            .arg("i")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                return Some(PathBuf::from(candidate));
            }
        }
    }
    None
}

fn command_no_window(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn push_failed_row(row: &mut ReportRow, reason: &str, err: &str, failures: &mut Vec<String>) {
    row.decision = "failed".to_string();
    row.reason = format!("{reason}:{err}");
    failures.push(format!("{} -> {}：{}", row.ani_path, reason, err));
}

fn write_report(path: &Path, rows: &[ReportRow]) -> AppResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut csv = String::new();
    csv.push_str("mode,chr_path,job,body_image_path,ani_path,frame_max,has_spectrum_before,decision,reason,sha256_before,sha256_after,backup_path\n");
    for row in rows {
        write_csv_row(
            &mut csv,
            &[
                &row.mode,
                &row.chr_path,
                &row.job,
                &row.body_image_path,
                &row.ani_path,
                &row.frame_max,
                if row.has_spectrum_before {
                    "true"
                } else {
                    "false"
                },
                &row.decision,
                &row.reason,
                &row.sha256_before,
                &row.sha256_after,
                &row.backup_path,
            ],
        );
    }
    fs::write(path, csv)?;
    Ok(())
}

fn write_csv_row(out: &mut String, cells: &[&str]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&csv_escape(cell));
    }
    out.push('\n');
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

impl Manifest {
    fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        json_field(&mut out, 1, "created_at", &self.created_at, true);
        json_field(&mut out, 1, "mode", &self.mode, true);
        json_field(&mut out, 1, "source_root", &self.source_root, true);
        json_field(&mut out, 1, "output_root", &self.output_root, true);
        out.push_str("  \"config_snapshot\": ");
        out.push_str(&config_json(&self.config, 2));
        out.push_str(",\n");
        out.push_str("  \"files\": [\n");
        for (index, file) in self.files.iter().enumerate() {
            out.push_str("    {\n");
            json_field(&mut out, 3, "chr_path", &file.chr_path, true);
            json_field(&mut out, 3, "job", &file.job, true);
            json_field(&mut out, 3, "body_image_path", &file.body_image_path, true);
            json_field(&mut out, 3, "ani_path", &file.ani_path, true);
            json_field(&mut out, 3, "backup_path", &file.backup_path, true);
            let _ = writeln!(out, "      \"frame_max\": {},", file.frame_max);
            json_field(&mut out, 3, "action", &file.action, true);
            json_field(&mut out, 3, "sha256_before", &file.sha256_before, true);
            json_field(&mut out, 3, "sha256_after", &file.sha256_after, false);
            out.push_str("    }");
            if index + 1 != self.files.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }
}

fn config_json(config: &Config, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let pad2 = " ".repeat(indent + 2);
    format!(
        "{{\n{pad2}\"spectrum\": {{\n{pad2}  \"enabled\": {enabled},\n{pad2}  \"term\": {term},\n{pad2}  \"life_time\": {life_time},\n{pad2}  \"color\": [{r}, {g}, {b}, {a}],\n{pad2}  \"effect\": \"{effect}\"\n{pad2}}},\n{pad2}\"existing_spectrum_policy\": \"{policy}\",\n{pad2}\"min_frame_max\": {min_frame_max},\n{pad2}\"allow_small_frame_name_contains\": {allow},\n{pad2}\"blacklist_name_contains\": {blacklist},\n{pad2}\"case_insensitive\": {case_insensitive}\n{pad}}}",
        enabled = config.spectrum.enabled,
        term = config.spectrum.term,
        life_time = config.spectrum.life_time,
        r = config.spectrum.color[0],
        g = config.spectrum.color[1],
        b = config.spectrum.color[2],
        a = config.spectrum.color[3],
        effect = json_escape(&config.spectrum.effect),
        policy = match config.existing_policy {
            ExistingPolicy::Skip => "skip",
            ExistingPolicy::Update => "update",
            ExistingPolicy::Force => "force",
        },
        min_frame_max = config.min_frame_max,
        allow = json_string_list(&config.allow_small_frame_name_contains),
        blacklist = json_string_list(&config.blacklist_name_contains),
        case_insensitive = config.case_insensitive,
    )
}

fn default_config_json() -> String {
    config_json(&Config::default(), 0)
}

fn json_field(out: &mut String, indent: usize, key: &str, value: &str, comma: bool) {
    let _ = writeln!(
        out,
        "{}\"{}\": \"{}\"{}",
        " ".repeat(indent * 2),
        key,
        json_escape(value),
        if comma { "," } else { "" }
    );
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn json_string_list(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "\"{}\"", json_escape(value));
    }
    out.push(']');
    out
}

fn parse_manifest_entries(text: &str) -> Vec<ManifestEntry> {
    let mut entries = Vec::new();
    let Some(files_index) = text.find("\"files\"") else {
        return entries;
    };
    let Some(array_start_rel) = text[files_index..].find('[') else {
        return entries;
    };
    let mut index = files_index + array_start_rel + 1;
    while let Some(obj_start_rel) = text[index..].find('{') {
        let obj_start = index + obj_start_rel;
        let Some(obj_end) = find_matching_brace(text, obj_start) else {
            break;
        };
        let object = &text[obj_start..=obj_end];
        entries.push(ManifestEntry {
            chr_path: json_string(object, "chr_path").unwrap_or_default(),
            job: json_string(object, "job").unwrap_or_default(),
            body_image_path: json_string(object, "body_image_path").unwrap_or_default(),
            ani_path: json_string(object, "ani_path").unwrap_or_default(),
            backup_path: json_string(object, "backup_path").unwrap_or_default(),
            frame_max: json_u32(object, "frame_max").unwrap_or_default(),
            action: json_string(object, "action").unwrap_or_default(),
            sha256_before: json_string(object, "sha256_before").unwrap_or_default(),
            sha256_after: json_string(object, "sha256_after").unwrap_or_default(),
        });
        index = obj_end + 1;
    }
    entries
}

fn find_matching_brace(text: &str, start: usize) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;
    let mut depth = 0u32;
    for (offset, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn json_string(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let key_pos = text.find(&pattern)?;
    let colon = text[key_pos + pattern.len()..].find(':')? + key_pos + pattern.len();
    let mut index = colon + 1;
    while text
        .as_bytes()
        .get(index)
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        index += 1;
    }
    if text.as_bytes().get(index) != Some(&b'"') {
        return None;
    }
    parse_json_string_at(text, index)
}

fn parse_json_string_at(text: &str, quote_index: usize) -> Option<String> {
    let mut out = String::new();
    let mut escaped = false;
    for ch in text[quote_index + 1..].chars() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn json_u32(text: &str, key: &str) -> Option<u32> {
    let pattern = format!("\"{key}\"");
    let key_pos = text.find(&pattern)?;
    let colon = text[key_pos + pattern.len()..].find(':')? + key_pos + pattern.len();
    let rest = &text[colon + 1..];
    let start = rest.find(|ch: char| ch.is_ascii_digit())?;
    let digits: String = rest[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn json_bool(text: &str, key: &str) -> Option<bool> {
    let pattern = format!("\"{key}\"");
    let key_pos = text.find(&pattern)?;
    let colon = text[key_pos + pattern.len()..].find(':')? + key_pos + pattern.len();
    let rest = text[colon + 1..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_u32_array(text: &str, key: &str) -> Option<Vec<u32>> {
    let inside = json_array_body(text, key)?;
    let mut values = Vec::new();
    for item in inside.split(',') {
        let value = item.trim();
        if !value.is_empty() {
            values.push(value.parse().ok()?);
        }
    }
    Some(values)
}

fn json_string_array(text: &str, key: &str) -> Option<Vec<String>> {
    let inside = json_array_body(text, key)?;
    let mut values = Vec::new();
    let mut index = 0;
    while let Some(quote_rel) = inside[index..].find('"') {
        let quote = index + quote_rel;
        let parsed = parse_json_string_at(&inside, quote)?;
        index = quote + parsed.len() + 2;
        values.push(parsed);
    }
    Some(values)
}

fn json_array_body(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let key_pos = text.find(&pattern)?;
    let colon = text[key_pos + pattern.len()..].find(':')? + key_pos + pattern.len();
    let array_start = text[colon + 1..].find('[')? + colon + 1;
    let array_end = text[array_start + 1..].find(']')? + array_start + 1;
    Some(text[array_start + 1..array_end].to_string())
}

fn relative_path(root: &Path, path: &Path) -> AppResult<PathBuf> {
    let root_raw = absolute_path_text(root)?;
    let path_raw = absolute_path_text(path)?;
    let root_cmp = root_raw.to_ascii_lowercase();
    let path_cmp = path_raw.to_ascii_lowercase();
    if root_cmp == path_cmp {
        return Ok(PathBuf::new());
    }
    let prefix = format!("{root_cmp}/");
    if !path_cmp.starts_with(&prefix) {
        return Err(format!("path is outside root: {}", path.display()).into());
    }
    Ok(path_from_manifest(&path_raw[root_raw.len() + 1..]))
}

fn path_is_same_or_inside(path: &Path, parent: &Path) -> AppResult<bool> {
    let path_text = absolute_path_text(path)?.to_ascii_lowercase();
    let parent_text = absolute_path_text(parent)?.to_ascii_lowercase();
    Ok(path_text == parent_text || path_text.starts_with(&format!("{parent_text}/")))
}

fn path_is_inside(path: &Path, parent: &Path) -> AppResult<bool> {
    let path_text = absolute_path_text(path)?.to_ascii_lowercase();
    let parent_text = absolute_path_text(parent)?.to_ascii_lowercase();
    Ok(path_text.starts_with(&format!("{parent_text}/")))
}

fn absolute_path_text(path: &Path) -> AppResult<String> {
    Ok(normalize_path_text(&absolute_lexical(path)?))
}

fn absolute_lexical(path: &Path) -> AppResult<PathBuf> {
    let input = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    Ok(normalize_components(&input))
}

fn normalize_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn normalize_path_text(path: &Path) -> String {
    normalize_slashes(&path.display().to_string())
}

fn normalize_slashes(value: &str) -> String {
    value.replace('\\', "/")
}

fn path_from_manifest(value: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for part in value.split('/') {
        if !part.is_empty() {
            path.push(part);
        }
    }
    path
}

fn resolve_manifest_path(manifest_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path_from_manifest(value))
    }
}

fn display_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

fn detect_newline(bytes: &[u8]) -> &'static str {
    if bytes.windows(2).any(|window| window == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn ends_with_newline(bytes: &[u8]) -> bool {
    bytes.ends_with(b"\n") || bytes.ends_with(b"\r")
}

fn ascii_ends_with(bytes: &[u8], suffix: &[u8]) -> bool {
    bytes.len() >= suffix.len()
        && ascii_eq_ignore_case(&bytes[bytes.len() - suffix.len()..], suffix)
}

fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn timestamp_name() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("batch_{secs}")
}

fn human_timestamp() -> String {
    timestamp_name()
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() + 8) % 64 != 0 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let j = i * 4;
            *word = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_spectrum_before_frame_max() {
        let source = b"#PVF_File\r\n\r\n[LOOP]\r\n    1\r\n[FRAME MAX]\r\n    8\r\n";
        let out = insert_spectrum_before_frame_max(source, &Config::default().spectrum).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("[SPECTRUM]\r\n    1\r\n"));
        assert!(text.find("[SPECTRUM]").unwrap() < text.find("[FRAME MAX]").unwrap());
        assert!(text.contains("[SPECTRUM EFFECT]\r\n        `NONE`\r\n"));
    }

    #[test]
    fn scan_and_apply_body_only() {
        let root = unique_temp_dir("spectrum_tool_apply");
        let export = root.join("export");
        let out = root.join("out");
        let backup = root.join("backup");
        let chr_dir = export.join("character").join("swordman");
        let ani_dir = chr_dir.join("Animation");
        fs::create_dir_all(&ani_dir).unwrap();
        fs::write(
            chr_dir.join("swordman.chr"),
            b"[job]\n    `[swordman]`\n[body image path]\n    `Character/swordman/Equipment/Avatar/skin/sm_body%04d.img`\n[dash motion]\n    `Animation/Dash.ani`\n[effect motion]\n    `Animation/Effect.ani`\n",
        )
        .unwrap();
        fs::write(
            ani_dir.join("Dash.ani"),
            b"#PVF_File\n`Character/swordman/Equipment/Avatar/skin/sm_body%04d.img`\n[FRAME MAX]\n    8\n",
        )
        .unwrap();
        fs::write(
            ani_dir.join("Effect.ani"),
            b"#PVF_File\n`Character/common/effect.img`\n[FRAME MAX]\n    8\n",
        )
        .unwrap();

        let report = out.join("report.csv");
        let (rows, manifest, archive_path) =
            apply(&export, &out, &backup, &report, &Config::default()).unwrap();
        assert_eq!(
            rows.iter().filter(|row| row.decision == "modified").count(),
            1
        );
        let modified =
            fs::read_to_string(out.join("character/swordman/Animation/Dash.ani")).unwrap();
        assert!(modified.contains("[SPECTRUM]"));
        let untouched =
            fs::read_to_string(out.join("character/swordman/Animation/Effect.ani")).unwrap();
        assert!(!untouched.contains("[SPECTRUM]"));
        assert_eq!(archive_path.extension().and_then(OsStr::to_str), Some("7z"));
        let archive_entries = seven_z_entry_names(&archive_path);
        assert!(archive_entries.contains(&"character/swordman/swordman.chr".to_string()));
        assert!(archive_entries.contains(&"character/swordman/Animation/Dash.ani".to_string()));
        assert!(archive_entries.contains(&"character/swordman/Animation/Effect.ani".to_string()));
        assert!(!archive_entries.contains(&"report.csv".to_string()));
        assert!(!archive_entries.contains(&"manifest.json".to_string()));

        let restore_report = out.join("restore_report.csv");
        restore(&manifest, &out, &restore_report).unwrap();
        let restored =
            fs::read_to_string(out.join("character/swordman/Animation/Dash.ani")).unwrap();
        assert!(!restored.contains("[SPECTRUM]"));

        let output_manifest = out.join("manifest.json");
        apply(&export, &out, &backup, &report, &Config::default()).unwrap();
        restore(&output_manifest, &out, &restore_report).unwrap();
        let restored_from_output_manifest =
            fs::read_to_string(out.join("character/swordman/Animation/Dash.ani")).unwrap();
        assert!(!restored_from_output_manifest.contains("[SPECTRUM]"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_output_and_backup_inside_source_root() {
        let root = unique_temp_dir("spectrum_tool_paths");
        let export = root.join("export");
        fs::create_dir_all(&export).unwrap();

        let inside_out = export.join("out");
        let outside_backup = root.join("backup");
        let outside_report = root.join("report.csv");
        let err = apply(
            &export,
            &inside_out,
            &outside_backup,
            &outside_report,
            &Config::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("output root"));

        let outside_out = root.join("out");
        let inside_backup = export.join("backup");
        let err = apply(
            &export,
            &outside_out,
            &inside_backup,
            &outside_report,
            &Config::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("backup root"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn section_and_frame_parsing_do_not_cross_into_next_section() {
        let chr = b"[body image path]\n[dash motion]\n    `Animation/Dash.ani`\n";
        assert!(extract_section_backtick_value(chr, b"[body image path]").is_none());

        let ani = b"[FRAME MAX]\n[NEXT]\n    8\n";
        assert_eq!(parse_frame_max(ani), None);
    }

    #[test]
    fn ani_refs_dedupe_slash_variants_and_reject_parent_traversal() {
        let chr = b"`Animation/Dash.ani`\n`Animation\\Dash.ani`\n";
        assert_eq!(extract_ani_refs(chr).len(), 1);

        let root = unique_temp_dir("spectrum_tool_traversal");
        let export = root.join("export");
        let chr_dir = export.join("character").join("swordman");
        fs::create_dir_all(&chr_dir).unwrap();
        fs::write(
            chr_dir.join("swordman.chr"),
            b"[body image path]\n    `Character/swordman/Equipment/Avatar/skin/sm_body%04d.img`\n[dash motion]\n    `../Dash.ani`\n",
        )
        .unwrap();

        let rows = scan(&export, &Config::default(), "scan").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reason, "invalid_ani_path");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blacklist_matches_file_name_not_parent_directories() {
        let keywords = vec!["Down".to_string()];
        assert_eq!(
            blacklist_keyword_hit(
                Path::new("E:/baidu_pan/download/dnf/character/fighter/animation/Dash.ani"),
                &keywords,
                true,
            ),
            None
        );
        assert_eq!(
            blacklist_keyword_hit(
                Path::new("E:/baidu_pan/download/dnf/character/fighter/animation/Down.ani"),
                &keywords,
                true,
            ),
            Some("Down".to_string())
        );
    }

    #[test]
    fn blacklist_avoids_word_internal_false_positives() {
        let sit = vec!["Sit".to_string()];
        assert_eq!(
            blacklist_keyword_hit(Path::new("NormalInquisitor_Attack1.ani"), &sit, true),
            None
        );
        assert_eq!(
            blacklist_keyword_hit(Path::new("LifeDepriveDisposition1.ani"), &sit, true),
            None
        );
        assert_eq!(
            blacklist_keyword_hit(Path::new("HolyFlameSit.ani"), &sit, true),
            Some("Sit".to_string())
        );

        let down = vec!["Down".to_string()];
        assert_eq!(
            blacklist_keyword_hit(Path::new("shadownormalattack1.ani"), &down, true),
            None
        );
        assert_eq!(
            blacklist_keyword_hit(Path::new("ThrowDownKick.ani"), &down, true),
            Some("Down".to_string())
        );
    }

    #[test]
    fn body_path_template_matches_concrete_four_digit_img() {
        let body = b"Character/fighter/ATEquipment/Avatar/skin/fm_body%04d.img";
        let ani =
            b"`character/fighter/atequipment/avatar/skin/fm_body0000.img`\n[FRAME MAX]\n    5\n";
        assert!(contains_body_image_path(ani, body, true));

        let other = b"`character/fighter/atequipment/avatar/skin/fm_body00.img`";
        assert!(!contains_body_image_path(other, body, true));
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{name}_{nanos}"))
    }

    fn seven_z_entry_names(path: &Path) -> Vec<String> {
        let seven_zip = find_7z_command().unwrap();
        let output = command_no_window(&seven_zip)
            .arg("l")
            .arg("-slt")
            .arg(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .filter_map(|line| line.strip_prefix("Path = "))
            .filter(|entry| entry.contains('/') || entry.contains('\\'))
            .map(|entry| {
                normalize_slashes(entry)
                    .trim_start_matches("./")
                    .to_string()
            })
            .collect()
    }
}
