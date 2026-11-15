//! 三套外观：明昼（浅色）/ 黑夜（深色）/ 百日橘（图5 的蓝紫）。
//!
//! 所有界面颜色都从 `pal()` 取，切主题只需要换 Settings.theme 再重画一帧。

use eframe::egui;
use std::sync::atomic::{AtomicU8, Ordering};

/// 界面用的一整套颜色。
#[derive(Clone, Copy)]
pub struct Pal {
    /// 窗口底色（卡片之间的空隙）
    pub bg: egui::Color32,
    /// 顶栏 / 侧栏底色
    pub panel: egui::Color32,
    /// 卡片底色
    pub card: egui::Color32,
    /// 卡片里可悬停的浅一档
    pub card_hi: egui::Color32,
    /// 分隔线 / 描边
    pub line: egui::Color32,
    /// 品牌色：选中态、主按钮
    pub accent: egui::Color32,
    /// 品牌色的淡底：选中行背景
    pub accent_dim: egui::Color32,
    /// 主文字
    pub text: egui::Color32,
    /// 次要文字
    pub muted: egui::Color32,
    pub ok: egui::Color32,
    pub warn: egui::Color32,
    pub bad: egui::Color32,
    /// 是不是浅色底（决定 egui Visuals 用 light 还是 dark 打底）
    pub light: bool,
}

pub const THEME_LIGHT: &str = "Light";
pub const THEME_DARK: &str = "Dark";
pub const THEME_SUNSET: &str = "Sunset";

/// 界面上的显示名，顺序就是设置页里的顺序。
pub const THEME_NAMES: [(&str, &str); 3] = [
    (THEME_LIGHT, "明昼"),
    (THEME_DARK, "黑夜"),
    (THEME_SUNSET, "百日橘"),
];

fn c(r: u8, g: u8, b: u8) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

/// 明昼：参考图 2 / 3 的配色 —— 底 #F1F2F4、品牌蓝 #0070D5。
///
/// 结构上不再靠"卡片浮起来"，而是靠发丝线切分区，所以 `card` 只比 `bg`
/// 亮一档（凹槽感），`line` 压得更淡，避免整片界面被描边切碎。
pub fn light() -> Pal {
    Pal {
        bg: c(0xF1, 0xF2, 0xF2),
        panel: c(0xF7, 0xF8, 0xF8),
        card: c(0xFB, 0xFC, 0xFC),
        card_hi: c(0xEA, 0xEC, 0xEC),
        line: c(0xDA, 0xDD, 0xDD),
        accent: c(0x00, 0x70, 0xD5),
        accent_dim: c(0xDD, 0xEB, 0xFA),
        text: c(0x16, 0x19, 0x1B),
        muted: c(0x6B, 0x72, 0x76),
        ok: c(0x2F, 0x8F, 0x62),
        warn: c(0xC0, 0x80, 0x1F),
        bad: c(0xC2, 0x4A, 0x42),
        light: true,
    }
}

/// 黑夜：原来那套深色，只是把底色再压暗一点、线更收。
pub fn dark() -> Pal {
    Pal {
        bg: c(0x0E, 0x11, 0x13),
        panel: c(0x14, 0x18, 0x1B),
        card: c(0x17, 0x1C, 0x1F),
        card_hi: c(0x1E, 0x24, 0x28),
        line: c(0x26, 0x2D, 0x31),
        accent: c(0x4D, 0xA3, 0xFF),
        accent_dim: c(0x18, 0x2C, 0x40),
        text: c(0xE7, 0xED, 0xF2),
        muted: c(0x7E, 0x8A, 0x93),
        ok: c(0x4F, 0xB5, 0x78),
        warn: c(0xD4, 0x9A, 0x3C),
        bad: c(0xD0, 0x60, 0x60),
        light: false,
    }
}

/// 百日橘：用户定的底色 `#F2EEE5`、主题色 `#DB7658`（暖橙红）。
///
/// 这一套是唯一带立绘的主题 —— 米色底配橙红，人物才压得住画面，
/// 换成明昼那种冷白底立绘会显得像贴纸。
pub fn sunset() -> Pal {
    Pal {
        bg: c(0xF2, 0xEE, 0xE5),
        panel: c(0xF7, 0xF4, 0xED),
        card: c(0xFF, 0xFD, 0xF9),
        card_hi: c(0xEF, 0xE7, 0xDC),
        line: c(0xE0, 0xD7, 0xC9),
        accent: c(0xDB, 0x76, 0x58),
        accent_dim: c(0xF7, 0xE3, 0xDC),
        text: c(0x2A, 0x21, 0x1C),
        muted: c(0x7C, 0x6F, 0x66),
        ok: c(0x4C, 0x8C, 0x5A),
        warn: c(0xC0, 0x8A, 0x2E),
        bad: c(0xC2, 0x50, 0x3F),
        light: true,
    }
}

pub fn of(kind: &str) -> Pal {
    match kind {
        THEME_DARK => dark(),
        THEME_SUNSET => sunset(),
        _ => light(),
    }
}

/// 当前主题存在这里，省得把 settings 传进每个画界面的函数。
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_current(kind: &str) {
    let v = match kind {
        THEME_DARK => 1,
        THEME_SUNSET => 2,
        _ => 0,
    };
    CURRENT.store(v, Ordering::Relaxed);
}

pub fn current_kind() -> &'static str {
    match CURRENT.load(Ordering::Relaxed) {
        1 => THEME_DARK,
        2 => THEME_SUNSET,
        _ => THEME_LIGHT,
    }
}

/// 当前主题颜色。调用点遍布界面，只做一次查表，很便宜。
pub fn pal() -> Pal {
    match CURRENT.load(Ordering::Relaxed) {
        1 => dark(),
        2 => sunset(),
        _ => light(),
    }
}

/// 把一套配色灌进 egui 的 Visuals，默认控件才有对的颜色。
pub fn apply(ctx: &egui::Context, kind: &str) {
    let p = of(kind);
    // 必须先把主题钉死，再改 style。egui 的 style 是分 light/dark 两套存的，
    // 默认偏好是"跟随系统"，首帧系统主题还没报到时会落到深色那套 ——
    // 不钉住的话，程序启动的第一帧永远是黑的，第二帧才变白。
    ctx.set_theme(if p.light { egui::Theme::Light } else { egui::Theme::Dark });
    let mut style = (*ctx.style()).clone();
    let mut v = if p.light { egui::Visuals::light() } else { egui::Visuals::dark() };

    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = if p.light { c(0xFF, 0xFF, 0xFF) } else { c(0x0E, 0x13, 0x1A) };
    v.faint_bg_color = p.card_hi;
    v.window_stroke = egui::Stroke::new(1.0, p.line);
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.selection.bg_fill = p.accent_dim;
    v.selection.stroke = egui::Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.widgets.noninteractive.bg_fill = p.card;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.muted);
    v.widgets.inactive.bg_fill = p.card_hi;
    v.widgets.inactive.weak_bg_fill = p.card_hi;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_fill = p.card_hi;
    v.widgets.hovered.weak_bg_fill = p.card_hi;
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, p.accent);
    v.widgets.active.bg_fill = p.accent_dim;
    v.widgets.active.weak_bg_fill = p.accent_dim;
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, p.accent);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.corner_radius = egui::CornerRadius::same(7);
    }

    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    ctx.set_style(style);
}
