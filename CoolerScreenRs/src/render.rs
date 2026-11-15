//! 软件渲染：把背景 + 文字画进 320×240 画布，再转成屏幕要的 240×320 RGB565。

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use crate::settings::{self, parse_color, Settings, DESIGN_H, DESIGN_W};
use crate::sensors::Snapshot;

pub const BUF_W: usize = 240;
pub const BUF_H: usize = 320;

const DIRS4: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const DIRS_DIAG: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const DIRS_FAR: [(i32, i32); 8] = [
    (2, 0),
    (-2, 0),
    (0, 2),
    (0, -2),
    (2, 2),
    (2, -2),
    (-2, 2),
    (-2, -2),
];

pub struct Canvas {
    pub px: Vec<u8>, // RGBA
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            px: vec![0; (DESIGN_W * DESIGN_H * 4) as usize],
        }
    }

    pub fn clear(&mut self, c: [u8; 3]) {
        for i in 0..(DESIGN_W * DESIGN_H) as usize {
            let o = i * 4;
            self.px[o] = c[0];
            self.px[o + 1] = c[1];
            self.px[o + 2] = c[2];
            self.px[o + 3] = 255;
        }
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= DESIGN_W as i32 || y >= DESIGN_H as i32 || a <= 0.0 {
            return;
        }
        let o = ((y as u32 * DESIGN_W + x as u32) * 4) as usize;
        let a = a.clamp(0.0, 1.0);
        for k in 0..3 {
            let src = c[k] as f32;
            let dst = self.px[o + k] as f32;
            self.px[o + k] = (dst * (1.0 - a) + src * a).round() as u8;
        }
        self.px[o + 3] = 255;
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [u8; 3], a: f32) {
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let x1 = (x + w).ceil() as i32;
        let y1 = (y + h).ceil() as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.blend(xx, yy, c, a);
            }
        }
    }

    /// 圆角块
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 3], a: f32) {
        for yy in y.floor() as i32..(y + h).ceil() as i32 {
            for xx in x.floor() as i32..(x + w).ceil() as i32 {
                let fx = xx as f32 + 0.5;
                let fy = yy as f32 + 0.5;
                let cx = fx.clamp(x + r, x + w - r);
                let cy = fy.clamp(y + r, y + h - r);
                let dx = fx - cx;
                let dy = fy - cy;
                if dx * dx + dy * dy <= r * r {
                    self.blend(xx, yy, c, a);
                }
            }
        }
    }

    /// 画一张 RGBA 图，按 fit 规则铺满 320×240。
    #[allow(dead_code)]
    pub fn draw_image(&mut self, img: &Img, fill: bool, opacity: f32) -> bool {
        self.draw_rgba(img.w, img.h, &img.rgba, fill, opacity)
    }

    /// 同上，但直接吃裸像素切片，省掉一次 300KB 的拷贝。
    ///
    /// 返回 true 表示整张画布已经被不透明地盖满了，调用方就不用再清底。
    pub fn draw_rgba(
        &mut self,
        img_w: u32,
        img_h: u32,
        rgba: &[u8],
        fill: bool,
        opacity: f32,
    ) -> bool {
        if img_w == 0 || img_h == 0 {
            return false;
        }
        let dw = DESIGN_W as f32;
        let dh = DESIGN_H as f32;
        let (sw, sh) = (img_w as f32, img_h as f32);
        let scale = if fill {
            (dw / sw).max(dh / sh)
        } else {
            (dw / sw).min(dh / sh).min(1.0)
        };
        let out_w = sw * scale;
        let out_h = sh * scale;
        let off_x = (dw - out_w) / 2.0;
        let off_y = (dh - out_h) / 2.0;
        let opacity = opacity.clamp(0.0, 1.0);

        // 快速通道：正好 1:1 且不透明，直接搬像素。
        // 视频每帧都是 320×240，走的都是这条路。
        if (scale - 1.0).abs() < 1e-6
            && opacity >= 0.999
            && img_w == DESIGN_W
            && img_h == DESIGN_H
            && rgba.len() >= (DESIGN_W * DESIGN_H * 4) as usize
        {
            let px = &mut self.px;
            for i in 0..(DESIGN_W * DESIGN_H) as usize {
                let s = i * 4;
                let a = rgba[s + 3];
                if a == 255 {
                    px[s] = rgba[s];
                    px[s + 1] = rgba[s + 1];
                    px[s + 2] = rgba[s + 2];
                    px[s + 3] = 255;
                } else if a > 0 {
                    let k = a as f32 / 255.0;
                    px[s] = (px[s] as f32 * (1.0 - k) + rgba[s] as f32 * k) as u8;
                    px[s + 1] = (px[s + 1] as f32 * (1.0 - k) + rgba[s + 1] as f32 * k) as u8;
                    px[s + 2] = (px[s + 2] as f32 * (1.0 - k) + rgba[s + 2] as f32 * k) as u8;
                    px[s + 3] = 255;
                }
            }
            return true;
        }

        // 一般路径：先把源坐标算进查找表，内层循环就只剩整数运算
        let mut xmap = vec![usize::MAX; DESIGN_W as usize];
        let mut covered = true;
        for x in 0..DESIGN_W as usize {
            let sx = ((x as f32 + 0.5 - off_x) / scale).floor();
            if sx >= 0.0 && sx < sw {
                xmap[x] = sx as usize;
            } else {
                covered = false;
            }
        }
        let mut ymap = vec![usize::MAX; DESIGN_H as usize];
        for y in 0..DESIGN_H as usize {
            let sy = ((y as f32 + 0.5 - off_y) / scale).floor();
            if sy >= 0.0 && sy < sh {
                ymap[y] = sy as usize;
            } else {
                covered = false;
            }
        }

        let stride = img_w as usize;
        let mut opaque = covered && opacity >= 0.999 && fill;
        for y in 0..DESIGN_H as usize {
            let sy = ymap[y];
            if sy == usize::MAX {
                continue;
            }
            let row = sy * stride;
            for x in 0..DESIGN_W as usize {
                let sx = xmap[x];
                if sx == usize::MAX {
                    continue;
                }
                let o = (row + sx) * 4;
                if o + 3 >= rgba.len() {
                    continue;
                }
                let a = (rgba[o + 3] as f32 / 255.0) * opacity;
                if a < 0.999 {
                    opaque = false;
                }
                if a <= 0.0 {
                    continue;
                }
                self.blend(x as i32, y as i32, [rgba[o], rgba[o + 1], rgba[o + 2]], a);
            }
        }
        opaque
    }

    /// 转成屏幕 buffer（240×320 RGB565 LE）。
    pub fn to_screen(&self, out: &mut [u8]) {
        self.to_screen_rot(out, 0);
    }

    /// 带旋转的版本。
    ///
    /// 设计画布是 320×240 横的，屏幕 buffer 是 240×320 竖的 —— 默认那一下
    /// 相当于顺时针转了 90°（正好铺满）。`rot` 是用户额外点的角度：
    /// 0 就是现在这样，90 再转一格，依此类推。转完不是正好铺满的时候
    /// （90 / 270）就等比缩进中间，多出来的边留黑。
    pub fn to_screen_rot(&self, out: &mut [u8], rot: u32) {
        let pw = DESIGN_W as i32;
        let ph = DESIGN_H as i32;
        let t = (90 + rot % 360) % 360;
        let (rw, rh) = if t == 90 || t == 270 {
            (ph, pw)
        } else {
            (pw, ph)
        };
        let s = (BUF_W as f32 / rw as f32).min(BUF_H as f32 / rh as f32);
        let ox = (BUF_W as f32 - rw as f32 * s) * 0.5;
        let oy = (BUF_H as f32 - rh as f32 * s) * 0.5;
        for by in 0..BUF_H {
            for bx in 0..BUF_W {
                let idx = (by * BUF_W + bx) * 2;
                let u = (bx as f32 + 0.5 - ox) / s;
                let v = (by as f32 + 0.5 - oy) / s;
                if u < 0.0 || v < 0.0 || u >= rw as f32 || v >= rh as f32 {
                    out[idx] = 0;
                    out[idx + 1] = 0;
                    continue;
                }
                let (ui, vi) = (u as i32, v as i32);
                let (dx, dy) = match t {
                    90 => (vi, ph - 1 - ui),
                    180 => (pw - 1 - ui, ph - 1 - vi),
                    270 => (pw - 1 - vi, ui),
                    _ => (ui, vi),
                };
                if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                    out[idx] = 0;
                    out[idx + 1] = 0;
                    continue;
                }
                let o = ((dy * pw + dx) * 4) as usize;
                let r = self.px[o] as u16;
                let g = self.px[o + 1] as u16;
                let b = self.px[o + 2] as u16;
                let vv: u16 = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
                out[idx] = (vv & 0xFF) as u8;
                out[idx + 1] = (vv >> 8) as u8;
            }
        }
    }
}

/// 把预览用的 RGBA 画布按角度转一下，让界面里看到的就是屏幕上那个朝向。
pub fn rotate_rgba(px: &[u8], w: usize, h: usize, rot: u32) -> (Vec<u8>, usize, usize) {
    let rot = rot % 360;
    if rot == 0 {
        return (px.to_vec(), w, h);
    }
    let (nw, nh) = if rot == 180 { (w, h) } else { (h, w) };
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = match rot {
                90 => (y, h - 1 - x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (w - 1 - y, x),
            };
            let si = (sy * w + sx) * 4;
            let di = (y * nw + x) * 4;
            out[di..di + 4].copy_from_slice(&px[si..si + 4]);
        }
    }
    (out, nw, nh)
}

pub struct Img {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

pub struct Fonts {
    regular: Option<FontVec>,
    bold: Option<FontVec>,
}

impl Fonts {
    pub fn load() -> Self {
        let reg = std::fs::read(r"C:\Windows\Fonts\msyh.ttc")
            .ok()
            .and_then(|d| FontVec::try_from_vec_and_index(d, 0).ok());
        let bold = std::fs::read(r"C:\Windows\Fonts\msyhbd.ttc")
            .ok()
            .and_then(|d| FontVec::try_from_vec_and_index(d, 0).ok());
        Self { regular: reg, bold }
    }

    fn pick(&self, bold: bool) -> Option<&FontVec> {
        if bold {
            self.bold.as_ref().or(self.regular.as_ref())
        } else {
            self.regular.as_ref()
        }
    }

    pub fn text_width(&self, text: &str, size: f32, bold: bool) -> f32 {
        let Some(f) = self.pick(bold) else {
            return text.chars().count() as f32 * size * 0.55;
        };
        let scaled = f.as_scaled(PxScale::from(size));
        let mut w = 0.0;
        for ch in text.chars() {
            let id = f.glyph_id(ch);
            w += scaled.h_advance(id);
        }
        w
    }

    /// 在画布上画文字，返回宽度。
    ///
    /// `halo` 是描边强度（0 = 不描边），`halo_rgb` 是描边颜色。
    /// 背景是图片/视频时打开，否则白字压在亮背景上就没法看了。
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text(
        &self,
        cv: &mut Canvas,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        bold: bool,
        color: egui::Color32,
        opacity: f32,
        halo: f32,
        halo_rgb: [u8; 3],
    ) -> f32 {
        let Some(f) = self.pick(bold) else { return 0.0 };
        let scaled = f.as_scaled(PxScale::from(size));
        let base_top = scaled.ascent();
        let c = [color.r(), color.g(), color.b()];
        let mut pen_x = x;
        let mut total = 0.0;

        // 每个字形只光栅化一次，覆盖率先收进临时表，
        // 之后同一份覆盖率既画描边也画本体，省一半开销。
        let mut cov_buf: Vec<(i32, i32, f32)> = Vec::with_capacity(512);

        for ch in text.chars() {
            let id = f.glyph_id(ch);
            let adv = scaled.h_advance(id);
            let glyph = id.with_scale_and_position(
                PxScale::from(size),
                ab_glyph::point(pen_x, y + base_top),
            );
            if let Some(og) = f.outline_glyph(glyph) {
                let bounds = og.px_bounds();
                cov_buf.clear();
                og.draw(|gx, gy, cov| {
                    if cov <= 0.02 {
                        return;
                    }
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    cov_buf.push((px, py, cov));
                });

                if halo > 0.0 {
                    // 8 邻域压暗一圈，视觉上等于 1px 描边 + 3px 柔和投影
                    let a_ring = halo * 0.55 * opacity;
                    let a_far = halo * 0.22 * opacity;
                    for &(px, py, cov) in cov_buf.iter() {
                        let k = cov.min(1.0);
                        for d in DIRS4.iter() {
                            cv.blend(px + d.0, py + d.1, halo_rgb, a_ring * k);
                        }
                        for d in DIRS_DIAG.iter() {
                            cv.blend(px + d.0, py + d.1, halo_rgb, a_ring * 0.7 * k);
                        }
                        for d in DIRS_FAR.iter() {
                            cv.blend(px + d.0, py + d.1, halo_rgb, a_far * k);
                        }
                    }
                }

                let a_text = opacity * (color.a() as f32 / 255.0);
                for &(px, py, cov) in cov_buf.iter() {
                    cv.blend(px, py, c, cov * a_text);
                }
            }
            pen_x += adv;
            total += adv;
        }
        total
    }
}

/// 视频帧直接整块搬进画布（320×240 RGB24 → RGBA，不透明）。
fn blit_rgb(cv: &mut Canvas, raw: &[u8]) {
    let n = (DESIGN_W * DESIGN_H) as usize;
    let px = &mut cv.px;
    if raw.len() < n * 3 {
        return;
    }
    for i in 0..n {
        let s = i * 3;
        let o = i * 4;
        px[o] = raw[s];
        px[o + 1] = raw[s + 1];
        px[o + 2] = raw[s + 2];
        px[o + 3] = 255;
    }
}

/// 视频帧半透明地叠到画布上（`opacity` < 1 时走这条）。
fn blend_rgb(cv: &mut Canvas, raw: &[u8], opacity: f32) {
    let n = (DESIGN_W * DESIGN_H) as usize;
    if raw.len() < n * 3 {
        return;
    }
    let a = opacity.clamp(0.0, 1.0);
    for i in 0..n {
        let s = i * 3;
        let x = (i as u32 % DESIGN_W) as i32;
        let y = (i as u32 / DESIGN_W) as i32;
        cv.blend(x, y, [raw[s], raw[s + 1], raw[s + 2]], a);
    }
}

/// 一个字段最终画在哪儿：文字、字号、左上角 X/Y、宽度。
///
/// 渲染和界面里的拖动命中都走这一份算法，两边算出来的框才会严丝合缝。
fn item_layout(
    item: &settings::InfoItem,
    s: &Settings,
    fonts: &Fonts,
    snap: &Snapshot,
) -> Option<(String, f32, f32, f32, f32)> {
    let v = settings::value_of(&item.id, snap)?;
    let value = settings::format_value(&item.id, v);
    let unit = if item.show_unit {
        settings::entry(&item.id).map(|e| e.unit).unwrap_or("")
    } else {
        ""
    };
    let label = if item.show_label && !item.label.is_empty() && item.id != "clock" {
        format!("{} ", item.label)
    } else {
        String::new()
    };
    let text = format!("{label}{value}{unit}");
    let size = item.font_size.max(6.0);
    let w = fonts.text_width(&text, size, item.bold);
    let mut x = item.x + s.offset_x;
    let y = item.y + s.offset_y;
    match item.align.as_str() {
        "Center" => x -= w / 2.0,
        "Right" => x -= w,
        _ => {}
    }
    Some((text, size, x, y, w))
}

/// 每个可见字段在设计画布里的包围盒（给界面做拖动命中 + 高亮用）。
pub fn item_boxes(
    fonts: &Fonts,
    s: &Settings,
    snap: &Snapshot,
) -> Vec<(usize, f32, f32, f32, f32)> {
    let mut out = Vec::new();
    for (i, item) in s.items.iter().enumerate() {
        if !item.visible {
            continue;
        }
        if let Some((_, size, x, y, w)) = item_layout(item, s, fonts, snap) {
            out.push((i, x, y, w.max(8.0), (size * 1.25).max(10.0)));
        }
    }
    out
}

/// 组装一帧画面。
///
/// `bg_raw` 是视频的裸 RGB24 帧（320×240），给了就用它，省掉转 RGBA 的那次拷贝。
pub fn compose(
    cv: &mut Canvas,
    fonts: &Fonts,
    s: &Settings,
    snap: &Snapshot,
    bg: Option<&Img>,
    bg_raw: Option<&[u8]>,
) -> Vec<u8> {
    let fallback = parse_color(&s.fallback_color);
    let op = s.background_opacity.clamp(0.0, 1.0);
    // 视频以前只在完全不透明时才画，一旦拉了透明度条就直接消失 ——
    // 因为视频帧走 bg_raw、bg_img 是 None，会落到"只铺纯色底"那条。
    // 现在按同一个透明度混上去。
    match bg_raw {
        Some(raw) if op >= 0.999 => blit_rgb(cv, raw),
        Some(raw) => {
            cv.clear([fallback.r(), fallback.g(), fallback.b()]);
            blend_rgb(cv, raw, op);
        }
        None => {
            cv.clear([fallback.r(), fallback.g(), fallback.b()]);
            if let Some(img) = bg {
                cv.draw_rgba(img.w, img.h, &img.rgba, s.background_fill, op);
            }
        }
    }

    for item in &s.items {
        if !item.visible {
            continue;
        }
        let Some((text, size, x, y, w)) = item_layout(item, s, fonts, snap) else {
            continue;
        };
        let back = parse_color(&item.back_color);
        if back.a() > 0 {
            cv.round_rect(
                x - 3.0,
                y - 2.0,
                w + 6.0,
                size + 4.0,
                4.0,
                [back.r(), back.g(), back.b()],
                back.a() as f32 / 255.0,
            );
        }
        let color = parse_color(&item.color);
        // 描边：每个字段自己决定开不开、多强、什么颜色。
        // 字号越大描边越明显一点（大字的边缘面积大，弱描边会显脏）。
        let (halo, halo_rgb) = if item.outline {
            let k = (size / 26.0).clamp(0.75, 1.15);
            let oc = parse_color(&item.outline_color);
            (
                (item.outline_strength * k).clamp(0.0, 3.0),
                [oc.r(), oc.g(), oc.b()],
            )
        } else {
            (0.0, [0, 0, 0])
        };
        fonts.draw_text(
            cv, &text, x, y, size, item.bold, color, 1.0, halo, halo_rgb,
        );
    }

    // ---------- 图形组件 ----------
    // 字是"一行",这些是"一个形状"。坐标记的是圆心,缩放围绕圆心走,
    // 这样拖动了之后再调大小不会跳位。
    for w in &s.widgets {
        if !w.visible {
            continue;
        }
        match w.kind.as_str() {
            "Ring" => draw_ring(cv, fonts, w, snap, s),
            _ => {}
        }
    }

    let mut out = vec![0u8; BUF_W * BUF_H * 2];
    cv.to_screen_rot(&mut out, s.rotation);
    out
}

/// 环形进度表：底环 + 从 12 点方向顺时针走的进度弧 + 中间百分比。
///
/// 参考图上那个圆环的调子是"环细、字实、留白多" —— 环太粗会变成老式仪表盘，
/// 所以默认 54 直径配 7 的环宽，再粗就要挤掉中间的字。
fn draw_ring(
    cv: &mut Canvas,
    fonts: &Fonts,
    w: &settings::Widget,
    snap: &Snapshot,
    s: &Settings,
) {
    let cx = w.x + s.offset_x;
    let cy = w.y + s.offset_y;
    let r = (w.size * 0.5).max(6.0);
    let th = w.thickness.clamp(1.0, r * 0.6);

    let Some(v) = settings::value_of(&w.id, snap) else {
        return;
    };
    // 温度类不是百分比，按 0~100°C 折算；其余原本就是百分比。
    let frac = if settings::entry(&w.id).map(|e| e.unit).unwrap_or("") == "°C" {
        (v / 100.0).clamp(0.0, 1.0)
    } else {
        (v / 100.0).clamp(0.0, 1.0)
    };

    let track = parse_color(&w.track_color);
    let col = parse_color(&w.color);

    // 底环：一圈画满
    ring_arc(
        cv,
        cx,
        cy,
        r - th * 0.5,
        th,
        0.0,
        1.0,
        [track.r(), track.g(), track.b()],
        track.a() as f32 / 255.0,
    );
    // 进度弧
    if frac > 0.001 {
        ring_arc(
            cv,
            cx,
            cy,
            r - th * 0.5,
            th,
            0.0,
            frac,
            [col.r(), col.g(), col.b()],
            col.a() as f32 / 255.0,
        );
    }

    if w.show_text {
        let text = format!("{:.0}%", v);
        let size = w.text_size.max(6.0);
        let tw = fonts.text_width(&text, size, true);
        let tx = cx - tw * 0.5;
        let ty = cy - size * 0.5;
        fonts.draw_text(cv, &text, tx, ty, size, true, col, 1.0, 0.0, [0, 0, 0]);
    }
}

/// 画一段圆环弧。`from`/`to` 是 0~1 的进度，0 在 12 点方向、顺时针。
///
/// 逐像素判断"到圆心的距离落在环宽内、且角度在区间内"，比先画圆再贴图省事，
/// 也天然抗锯齿（用距离做覆盖度）。
#[allow(clippy::too_many_arguments)]
fn ring_arc(
    cv: &mut Canvas,
    cx: f32,
    cy: f32,
    mid_r: f32,
    th: f32,
    from: f32,
    to: f32,
    c: [u8; 3],
    a: f32,
) {
    let half = th * 0.5;
    let outer = mid_r + half;
    let x0 = (cx - outer).floor() as i32;
    let x1 = (cx + outer).ceil() as i32;
    let y0 = (cy - outer).floor() as i32;
    let y1 = (cy + outer).ceil() as i32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = ((outer - d).min(d - (mid_r - half))).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            // 0 在 12 点、顺时针：atan2(dx, -dy)
            let mut t = dx.atan2(-dy) / std::f32::consts::TAU;
            if t < 0.0 {
                t += 1.0;
            }
            if t < from || t > to {
                continue;
            }
            cv.blend(px, py, c, a * cov);
        }
    }
}


