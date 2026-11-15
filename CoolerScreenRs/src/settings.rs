//! 配置模型：与 C# 版 settings.json 完全兼容（字段名一致）。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DESIGN_W: u32 = 320;
pub const DESIGN_H: u32 = 240;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct InfoItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default = "default_font")]
    pub font_family: String,
    #[serde(default = "default_size")]
    pub font_size: f32,
    #[serde(default = "yes")]
    pub bold: bool,
    #[serde(default = "white")]
    pub color: String,
    #[serde(default = "transparent")]
    pub back_color: String,
    #[serde(default = "yes")]
    pub show_unit: bool,
    #[serde(default = "yes")]
    pub show_label: bool,
    #[serde(default = "left_align")]
    pub align: String,
    /// 要不要给这行字压一圈暗边（背景是图片/视频时特别有用）
    #[serde(default = "yes")]
    pub outline: bool,
    #[serde(default = "default_outline_strength")]
    pub outline_strength: f32,
    #[serde(default = "default_outline_color")]
    pub outline_color: String,
}

fn default_outline_strength() -> f32 {
    1.0
}
fn default_outline_color() -> String {
    "#FF080E1A".into()
}

/// 上屏的图形组件。目前只有环形进度表（就是系统信息页里那种圆环）。
///
/// 它和 `InfoItem` 是两套东西：`InfoItem` 是「一行字」，这里是「一个形状」。
/// 坐标记的是**圆心**，这样拖动和缩放都不会让图形乱跳。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Widget {
    /// 跟着哪个数据走（cpuUsage / gpuUsage / ram / disk / cpuTemp …）
    #[serde(default)]
    pub id: String,
    /// 目前只有 Ring
    #[serde(default = "ring_kind")]
    pub kind: String,
    #[serde(default = "yes")]
    pub visible: bool,
    /// 圆心
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    /// 直径
    #[serde(default = "widget_size")]
    pub size: f32,
    /// 环的粗细
    #[serde(default = "widget_thickness")]
    pub thickness: f32,
    #[serde(default = "white")]
    pub color: String,
    /// 底环（没走满那一段）
    #[serde(default = "widget_track")]
    pub track_color: String,
    #[serde(default = "widget_text_size")]
    pub text_size: f32,
    #[serde(default = "yes")]
    pub show_text: bool,
}

fn ring_kind() -> String {
    "Ring".into()
}
fn widget_size() -> f32 {
    54.0
}
fn widget_thickness() -> f32 {
    7.0
}
fn widget_track() -> String {
    "#66FFFFFF".into()
}
fn widget_text_size() -> f32 {
    15.0
}

impl Widget {
    pub fn new(id: &str, x: f32, y: f32) -> Self {
        Self {
            id: id.into(),
            kind: ring_kind(),
            visible: true,
            x,
            y,
            size: widget_size(),
            thickness: widget_thickness(),
            color: "#FFFFFFFF".into(),
            track_color: widget_track(),
            text_size: widget_text_size(),
            show_text: true,
        }
    }
}

fn yes() -> bool {
    true
}
fn default_font() -> String {
    "微软雅黑".into()
}
fn default_size() -> f32 {
    20.0
}
fn white() -> String {
    "#FFFFFFFF".into()
}
fn transparent() -> String {
    "#00000000".into()
}
fn left_align() -> String {
    "Left".into()
}

impl InfoItem {
    pub fn new(id: &str, label: &str, x: f32, y: f32, size: f32, color: &str, bold: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            visible: true,
            x,
            y,
            font_family: default_font(),
            font_size: size,
            bold,
            color: color.into(),
            back_color: transparent(),
            show_unit: true,
            show_label: true,
            align: left_align(),
            outline: true,
            outline_strength: default_outline_strength(),
            outline_color: default_outline_color(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Settings {
    #[serde(default)]
    pub background_path: String,
    /// None / Image / Video
    #[serde(default = "kind_none")]
    pub background_kind: String,
    #[serde(default = "yes")]
    pub background_fill: bool,
    #[serde(default = "one")]
    pub background_opacity: f32,
    #[serde(default = "fallback")]
    pub fallback_color: String,
    #[serde(default = "fps15")]
    pub target_fps: u32,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default = "yes")]
    pub auto_kill_boomstar: bool,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub cpu_temp_choice: String,
    #[serde(default = "ffmpeg_default")]
    pub ffmpeg_path: String,
    /// 主题外观：Light（明昼）/ Dark（黑夜）/ Sunset（百日橘）
    #[serde(default = "theme_default")]
    pub theme: String,
    /// 画面旋转：0 / 90 / 180 / 270（顺时针，作用在整幅画面上）
    #[serde(default)]
    pub rotation: u32,
    /// 关掉之后不再读硬盘的使用率和温度（这项在部分机器上会让 HWiNFO
    /// 频繁去查 SMART，硬盘会隔几秒响一下）。
    #[serde(default = "yes")]
    pub disk_scan: bool,
    #[serde(default)]
    pub items: Vec<InfoItem>,
    /// 上屏的图形组件（环形进度表之类）
    #[serde(default)]
    pub widgets: Vec<Widget>,
    /// 存下来的样式候选（缩略图 + 一整套布局）
    #[serde(default)]
    pub styles: Vec<SavedStyle>,
}

/// 一套存下来的样式：字段布局 + 偏移 + 旋转 + 背景 + 主题，外加一张缩略图。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct SavedStyle {
    #[serde(default)]
    pub name: String,
    /// 缩略图 PNG 的绝对路径
    #[serde(default)]
    pub thumb: String,
    #[serde(default)]
    pub items: Vec<InfoItem>,
    #[serde(default)]
    pub widgets: Vec<Widget>,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub rotation: u32,
    #[serde(default = "kind_none")]
    pub background_kind: String,
    #[serde(default)]
    pub background_path: String,
    #[serde(default = "yes")]
    pub background_fill: bool,
    #[serde(default = "one")]
    pub background_opacity: f32,
    #[serde(default = "fallback")]
    pub fallback_color: String,
    #[serde(default = "theme_default")]
    pub theme: String,
}

impl SavedStyle {
    /// 从当前设置里抓一份快照（缩略图另外写）
    pub fn capture(s: &Settings, thumb: &str) -> Self {
        Self {
            name: String::new(),
            thumb: thumb.into(),
            items: s.items.clone(),
            widgets: s.widgets.clone(),
            offset_x: s.offset_x,
            offset_y: s.offset_y,
            rotation: s.rotation % 360,
            background_kind: s.background_kind.clone(),
            background_path: s.background_path.clone(),
            background_fill: s.background_fill,
            background_opacity: s.background_opacity,
            fallback_color: s.fallback_color.clone(),
            theme: s.theme.clone(),
        }
    }

    /// 把这套样式摊回设置里
    pub fn apply_to(&self, s: &mut Settings) {
        s.items = self.items.clone();
        s.widgets = self.widgets.clone();
        s.offset_x = self.offset_x;
        s.offset_y = self.offset_y;
        s.rotation = self.rotation % 360;
        s.background_kind = self.background_kind.clone();
        s.background_path = self.background_path.clone();
        s.background_fill = self.background_fill;
        s.background_opacity = self.background_opacity;
        s.fallback_color = self.fallback_color.clone();
        s.theme = self.theme.clone();
    }
}

fn kind_none() -> String {
    "None".into()
}
fn one() -> f32 {
    1.0
}
fn fps15() -> u32 {
    15
}
fn fallback() -> String {
    "#FF101018".into()
}
fn ffmpeg_default() -> String {
    String::new()
}
fn theme_default() -> String {
    crate::theme::THEME_LIGHT.into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background_path: String::new(),
            background_kind: kind_none(),
            background_fill: true,
            background_opacity: 1.0,
            fallback_color: fallback(),
            target_fps: 15,
            start_minimized: false,
            auto_kill_boomstar: true,
            offset_x: 0.0,
            offset_y: 0.0,
            cpu_temp_choice: String::new(),
            ffmpeg_path: ffmpeg_default(),
            theme: theme_default(),
            rotation: 0,
            disk_scan: true,
            items: default_items(),
            widgets: Vec::new(),
            styles: Vec::new(),
        }
    }
}

pub fn default_items() -> Vec<InfoItem> {
    vec![
        InfoItem::new("cpuTemp", "CPU", 8.0, 6.0, 26.0, "#FFFFFFFF", true),
        InfoItem::new("gpuTemp", "GPU", 170.0, 6.0, 26.0, "#FFFFFFFF", true),
        InfoItem::new("cpuUsage", "CPU", 8.0, 48.0, 20.0, "#FFB8E0FF", true),
        InfoItem::new("gpuUsage", "GPU", 170.0, 48.0, 20.0, "#FFB8E0FF", true),
        InfoItem::new("cpuClock", "CPU", 8.0, 80.0, 15.0, "#FF8FD0FF", false),
        InfoItem::new("gpuClock", "GPU", 170.0, 80.0, 15.0, "#FF8FD0FF", false),
        InfoItem::new("disk", "DISK", 8.0, 108.0, 15.0, "#FFA0FFC0", false),
        InfoItem::new("ram", "RAM", 170.0, 108.0, 15.0, "#FFA0FFC0", false),
        InfoItem::new("clock", "", 8.0, 148.0, 15.0, "#FFC0C0FF", false),
    ]
}

impl Settings {
    pub fn path() -> PathBuf {
        let mut p = std::env::current_exe().unwrap_or_default();
        p.pop();
        p.push("settings.json");
        p
    }

    pub fn load() -> Self {
        let p = Self::path();
        match std::fs::read_to_string(&p) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    if s.items.is_empty() {
                        s.items = default_items();
                    }
                    s.target_fps = s.target_fps.clamp(1, 30);
                    s
                }
                Err(e) => {
                    let mut log = Self::path();
                    log.pop();
                    log.push("settings_error.txt");
                    let _ = std::fs::write(log, format!("解析 {} 失败：{e}", p.display()));
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, text);
        }
    }
}

/// 可叠加的字段类型。`unit` 为空表示不带单位。
pub struct CatalogEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub unit: &'static str,
}

pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry { id: "cpuTemp", name: "CPU 温度", unit: "°C" },
    CatalogEntry { id: "cpuUsage", name: "CPU 使用率", unit: "%" },
    CatalogEntry { id: "cpuClock", name: "CPU 频率", unit: "MHz" },
    CatalogEntry { id: "gpuTemp", name: "GPU 温度", unit: "°C" },
    CatalogEntry { id: "gpuUsage", name: "GPU 使用率", unit: "%" },
    CatalogEntry { id: "gpuClock", name: "GPU 频率", unit: "MHz" },
    CatalogEntry { id: "disk", name: "磁盘占用率", unit: "%" },
    CatalogEntry { id: "diskTemp", name: "硬盘温度", unit: "°C" },
    CatalogEntry { id: "ram", name: "内存占用率", unit: "%" },
    CatalogEntry { id: "fan", name: "风扇转速", unit: " RPM" },
    CatalogEntry { id: "gpuPower", name: "GPU 功耗", unit: "W" },
    CatalogEntry { id: "clock", name: "当前时间", unit: "" },
];

pub fn entry(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

pub fn catalog_name(id: &str) -> &'static str {
    entry(id).map(|e| e.name).unwrap_or("未知字段")
}

/// 取字段数值。取不到返回 None。
pub fn value_of(id: &str, s: &crate::sensors::Snapshot) -> Option<f32> {
    if id == "clock" {
        return Some(1.0);
    }
    let v = match id {
        "cpuTemp" => s.cpu_temp,
        "cpuUsage" => s.cpu_usage,
        "cpuClock" => s.cpu_clock,
        "gpuTemp" => s.gpu_temp,
        "gpuUsage" => s.gpu_usage,
        "gpuClock" => s.gpu_clock,
        "gpuPower" => s.gpu_power,
        "fan" => s.fan_rpm,
        "disk" => s.disk_usage,
        "diskTemp" => s.disk_temp,
        "ram" => s.ram_usage,
        _ => 0.0,
    };
    if v > 0.0 {
        Some(v)
    } else {
        None
    }
}

pub fn format_value(id: &str, v: f32) -> String {
    if id == "clock" {
        return crate::sensors::now_hm();
    }
    v.round().to_string()
}

/// "#AARRGGBB" 或 "#RRGGBB" → Color32
pub fn parse_color(text: &str) -> egui::Color32 {
    let t = text.trim().trim_start_matches('#');
    let parse = |s: &str| u8::from_str_radix(s, 16).unwrap_or(255);
    match t.len() {
        8 => {
            let a = parse(&t[0..2]);
            let r = parse(&t[2..4]);
            let g = parse(&t[4..6]);
            let b = parse(&t[6..8]);
            egui::Color32::from_rgba_unmultiplied(r, g, b, a)
        }
        6 => egui::Color32::from_rgb(parse(&t[0..2]), parse(&t[2..4]), parse(&t[4..6])),
        _ => egui::Color32::WHITE,
    }
}

pub fn format_color(c: egui::Color32) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        c.a(),
        c.r(),
        c.g(),
        c.b()
    )
}
