//! CoolerSTAR —影驰 Boomstar 星耀 B360 水冷控制台Rust 版
//!
//! 架构
//! - UI 线程egui编辑设置、显示预觀
//! - 传感器线程500ms 轮询 HWiNFO
//! - 渲染 / 推帧线程按目标帧率合成画面并写共享内存

// 编成 GUI 子系统双击启动时不会先弹一个黑底控制台窗口出来
// 想临时看 println 的调试输出甀`cargo build` 皀debug 版那里仍是 console
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod hwinfo;
mod render;
mod screen;
mod sensors;
mod settings;
mod theme;
mod tray;
mod video;

use eframe::egui;
use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke};
use render::{Canvas, Fonts, Img};
use theme::pal;
use screen::Screen;
use sensors::{Provider, Snapshot};
use settings::{Settings, DESIGN_H, DESIGN_W};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use video::VideoSource;

// ---------- 名字 ----------
/// 软件名窗口标题和标题栏都跟着宀
pub const APP_NAME: &str = "CoolerSTAR";
/// 副标题这块屏是谁家的界面上左上角显示的就是这一行
pub const APP_SUB: &str = "影驰Boomstar星耀水冷控制台";
pub const APP_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// 把窗口的可见区域清空 / 恢复
///
/// 关掉窗口或者「启动后最小化」的时候eframe 画完第一帧会自己 `set_visible(true)`
/// 见宀native/epi_integration.rs 皀post_rendering我们排队皀
/// `ViewportCommand::Minimized(true)` 永远比它晚一步窗口会在屏幕正中露一下
/// 那时它还没画过任何东西就是一闪而过的黑框
///
/// 给窗口设一个空区域之后窗口该显示显示、该有任务栏按钮有任务栏按钮
/// 但一个像素都不画 —那一下自然就看不见了。也不用再把窗口挪到 -32000
/// 那种地方省得显示的时候还得挪回来
#[cfg(windows)]
fn set_blank_region(hwnd: isize, blank: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
    let h = HWND(hwnd as *mut core::ffi::c_void);
    let ok = unsafe {
        if blank {
            // SetWindowRgn 成功之后这个区域归系统管我们自己不能删
            let rgn = CreateRectRgn(0, 0, 0, 0);
            SetWindowRgn(h, Some(rgn), true)
        } else {
            SetWindowRgn(h, None, true)
        }
    };
    if std::env::var_os("COOLERSTAR_DEBUG").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::temp_dir().join("coolerstar_rgn.log"))
        {
            let _ = writeln!(f, "set_blank_region hwnd={hwnd} blank={blank} ret={ok}");
        }
    }
}

#[cfg(not(windows))]
fn set_blank_region(_hwnd: isize, _blank: bool) {}

/// 把窗口从任务栏上摘掉 / 挂回去
///
/// egui 0.32 皀`ViewportCommand` 里没最SkipTaskbar只能自己动手
/// 给窗口加 `WS_EX_TOOLWINDOW`Explorer 就不给它发任务栏按钮了
/// 改完必须 `SetWindowPos(SWP_FRAMECHANGED)` 通知一次否则要等下次
/// 窗口重建才生效。无边框窗口本来没有标题栏所以这个扩展样开
/// 除了任务栏按钮之外不会带来别的视觉变化
#[cfg(windows)]
fn set_taskbar_visible(hwnd: isize, visible: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_TOOLWINDOW,
    };
    if hwnd == 0 {
        return;
    }
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let bit = WS_EX_TOOLWINDOW.0 as isize;
        let want = if visible { ex & !bit } else { ex | bit };
        if want == ex {
            return;
        }
        SetWindowLongPtrW(h, GWL_EXSTYLE, want);
        let _ = SetWindowPos(
            h,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn set_taskbar_visible(_hwnd: isize, _visible: bool) {}

/// 真正把窗口藏起来 / 露出来
///
/// 「最小化」和「藏起来」是两件事最小化只是收进任务栏Alt+Tab 里还留着
/// 有些人装了第三方任务栏这台机器上是 MyDockFinder连 `WS_EX_TOOLWINDOW`
/// 都不认照样占一格。用户要的是「关掉窗口只剩托盘图标后台继续跑」
/// 那就往`ShowWindow(SW_HIDE)` —窗口不在画面上、不进任务栏、不迀Alt+Tab
/// 但进程、线程、渲染全都在跑。托盘图标是我们自己建的跟窗口无关
#[cfg(windows)]
fn show_window(hwnd: isize, show: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsWindowVisible, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
    };
    if hwnd == 0 {
        return;
    }
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        if show {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        } else if IsWindowVisible(h).as_bool() {
            // 只在真的还露着的时候才藏一次。eframe 画完第一帧会强制
            // set_visible(true)所以每帧都得核一遍但又不能每帧都调
            // ShowWindow会打断绘制
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn show_window(_hwnd: isize, _show: bool) {}

/// 什eframe 皀CreationContext 里把主窗口句柄掏出来`App::new` 时窗口已经建好
/// 但还没显示正好赶在它露面之前动手
#[cfg(windows)]
fn window_hwnd(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle() {
        Ok(w) => match w.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => 0,
        },
        Err(_) => 0,
    }
}

#[cfg(not(windows))]
fn window_hwnd(_cc: &eframe::CreationContext<'_>) -> isize {
    0
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// 屏幕设备屏上画靀+ 样式候退
    Device,
    /// 系统信息CPU / GPU / 内存 / 存储 四块实时数据
    Fields,
    /// 属性显示显示列表 + 文字属怀
    Attrs,
    /// 设置主题、背景、出图、开机自吀
    Settings,
}

pub struct App {
    settings: Arc<Mutex<Settings>>,
    snap: Arc<Mutex<Snapshot>>,
    info: Arc<Mutex<RuntimeInfo>>,
    preview: Arc<Mutex<Option<Vec<u8>>>>,
    running: Arc<AtomicBool>,
    worker_stop: Arc<AtomicBool>,

    tab: Tab,
    /// 「文字属性」里正在编辑的字殀
    selected: usize,
    /// 预览里正在拖的字段下标以及按下时指针相对文字左上角的偏秀
    drag_item: Option<usize>,
    drag_item_grab: egui::Vec2,
    /// 预览里正在拖的图形组件下标以及按下时指针相对圆心的偏移
    selected_widget: Option<usize>,
    drag_widget: Option<usize>,
    drag_grab: egui::Vec2,
    /// 量文字宽度用的字体跟渲染线程同一套两边算出来的框才一致
    fonts: render::Fonts,
    /// 样式候选卡里翻到第几页每顀3×5 = 15 个
    style_page: usize,
    /// 正在等用户确认删除的样式下标
    style_pending_del: Option<usize>,

    font_defs_loaded: bool,
    preview_tex: Option<egui::TextureHandle>,
    preview_dirty: bool,
    /// 当前预览纹理的像素尺寸旋转后会变
    preview_dims: (f32, f32),
    /// 上次上传预览时的旋转角变了就知道要重传
    preview_rot: u32,
    /// 样式候选缩略图按文件路径缓存
    style_thumbs: std::collections::HashMap<String, egui::TextureHandle>,
    /// 左侧栏底部那张角色立绘编译时打包进去不依赖外部文件
    /// 按主颀id 缓存 —三套配色的立绘不同切主题时要换图
    /// 但每张只该解码一次
    hero_tex: std::collections::HashMap<&'static str, egui::TextureHandle>,

    /// 开机自启的实际状态读注册表不信配置里的值
    auto_start: bool,
    /// 启动时就该藏着配置里勾了「启动后最小化」或者带亀-tray 参数
    /// 窗口的可见区域一开始就被清空所以那一下强制显示也画不出东西
    start_hidden: bool,
    /// 已经隐藏过一次了避免重复发命令
    hid_once: bool,
    /// 窗口现在是不是藏在托盘里藏起来就不用刷预览纹理了
    hidden: bool,
    /// 用户希望窗口待在托盘里。这只是一愿望"真正的隐藏甀update() 釀
    /// 皀ShowWindow(SW_HIDE) 执行不再走 winit 的最小化
    want_hidden: bool,
    /// 上一帧窗口是不是最小化的。用来做边沿检测从最小化变回正常
    /// 说明用户自己把窗口弄回来了那时才清 want_hidden
    was_minimized: bool,
    /// 窗口当前是不是真的藏着`ShowWindow(SW_HIDE)` 过
    win_hidden: bool,
    /// 窗口有没有真正露过面。启动即隐藏的那种窗口一直不可见
    /// 佀winit 报的 minimized 昀false —得靠这个技hidden 撑住
    ever_shown: bool,
    /// 主窗口句柄0 = 没拿到
    hwnd: isize,
    /// 窗口当前的可见区域是不是空的避免重复调 SetWindowRgn
    region_blank: bool,

    // 运行页状态甀worker 回填
    last_render: std::time::Instant,
    last_saved: String,
    log: Vec<String>,
}

#[derive(Default)]
pub struct RuntimeInfo {
    pub source: String,
    pub cpu_name: String,
    pub gpu_name: String,
    pub temp_source: String,
    pub temp_candidates: Vec<(String, f32)>,
    pub screen_error: String,
    pub video_error: String,
    pub helper_ok: bool,
    pub elevated: bool,
    pub screen_ready: bool,
    pub created_section: bool,
    pub loop_note: String,
    pub bg_label: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);

        let s = Settings::load();
        theme::set_current(&s.theme);
        theme::apply(&cc.egui_ctx, &s.theme);
        let tray_arg = std::env::args().any(|a| {
            a.eq_ignore_ascii_case("-tray") || a.eq_ignore_ascii_case("--tray")
        });
        let snap = Arc::new(Mutex::new(Snapshot::default()));
        let settings = Arc::new(Mutex::new(s.clone()));
        let info = Arc::new(Mutex::new(RuntimeInfo {
            elevated: sensors::is_elevated(),
            helper_ok: sensors::process_running("USBLCD"),
            ..Default::default()
        }));
        let preview = Arc::new(Mutex::new(None));
        let running = Arc::new(AtomicBool::new(true));
        let worker_stop = Arc::new(AtomicBool::new(false));

        // ---- 传感器线稀----
        {
            let snap = snap.clone();
            let settings = settings.clone();
            let stop = worker_stop.clone();
            let info = info.clone();
            std::thread::spawn(move || {
                let mut p = Provider::new();
                // HWINFO.exe 不在跑就拉一次。它得是管理员ShellExecute 皀runas
                // 圀UAC 设置戀不提礀时不会弹框不然用户会看到一次确认
                let mut last_hw_spawn = std::time::Instant::now() - std::time::Duration::from_secs(60);
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(s) = settings.lock() {
                        p.temp_choice = s.cpu_temp_choice.clone();
                        p.disk_scan = s.disk_scan;
                    }
                    p.poll();
                    // 没数据、HWINFO 也没在跑 ↀ每隔 20 秒试着拉一欀
                    if !p.hw_ok
                        && !sensors::hwinfo_running()
                        && last_hw_spawn.elapsed().as_secs() >= 20
                    {
                        let _ = sensors::start_hwinfo();
                        last_hw_spawn = std::time::Instant::now();
                    }
                    if let Ok(mut g) = snap.lock() {
                        *g = p.snap.clone();
                    }
                    if let Ok(mut i) = info.lock() {
                        i.source = p.source.clone();
                        i.cpu_name = p.cpu_name.clone();
                        i.gpu_name = p.gpu_name.clone();
                        i.temp_source = p.temp_source.clone();
                        i.temp_candidates = p.temp_candidates.clone();
                        if p.hw_ok {
                            i.video_error = i.video_error.clone();
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            });
        }

        // ---- 渲染 / 推帧线程 ----
        {
            let settings = settings.clone();
            let snap = snap.clone();
            let preview = preview.clone();
            let stop = worker_stop.clone();
            let running = running.clone();
            let info = info.clone();
            std::thread::spawn(move || {
                let mut canvas = Canvas::new();
                let fonts = Fonts::load();
                let mut scr = Screen::new();
                let mut bg_img: Option<Img> = None;
                let mut bg_sig = String::new();
                let mut vsrc: Option<VideoSource> = None;
                let mut ffmpeg = sensors::find_ffmpeg();
                let mut ffmpeg_cfg = String::new();
                let mut last_frame = std::time::Instant::now();
                // 枚举进程很贵没必要每帧都做两秒查一次够亀
                let mut helper_ok = false;
                let mut helper_checked = std::time::Instant::now() - std::time::Duration::from_secs(30);

                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let cfg = match settings.lock() {
                        Ok(g) => g.clone(),
                        Err(_) => break,
                    };

                    // 界面上手填的 ffmpeg 路径优先改了要立刻生效
                    if cfg.ffmpeg_path != ffmpeg_cfg {
                        ffmpeg_cfg = cfg.ffmpeg_path.clone();
                        ffmpeg = if ffmpeg_cfg.is_empty() {
                            sensors::find_ffmpeg()
                        } else if std::path::Path::new(&ffmpeg_cfg).exists() {
                            Some(ffmpeg_cfg.clone())
                        } else {
                            sensors::find_ffmpeg()
                        };
                        // 路径换了得重建视频源
                        bg_sig.clear();
                    }

                    // 背景变化才重新加轀
                    let sig = format!("{}|{}", cfg.background_kind, cfg.background_path);
                    if sig != bg_sig {
                        bg_sig = sig.clone();
                        vsrc.take();
                        bg_img = None;
                        match cfg.background_kind.as_str() {
                            "Image" => {
                                bg_img = load_image(&cfg.background_path);
                            }
                            "Video" => {
                                if ffmpeg.is_none() {
                                    ffmpeg = sensors::find_ffmpeg();
                                }
                                if let Some(f) = &ffmpeg {
                                    vsrc = Some(VideoSource::start(&cfg.background_path, f));
                                }
                            }
                            _ => {}
                        }
                        if let Ok(mut i) = info.lock() {
                            i.video_error = vsrc
                                .as_ref()
                                .and_then(|v| v.error.lock().ok().map(|e| e.clone()))
                                .unwrap_or_default();
                            i.loop_note = vsrc.as_ref().map(|v| v.note.clone()).unwrap_or_default();
                            i.bg_label = bg_label(&cfg);
                        }
                    }

                    // 视频最新帧直接留裀RGB24合成时整块搬进画布
                    let mut bg_raw: Option<Vec<u8>> = None;
                    if let Some(v) = &vsrc {
                        if let Some(raw) = v.raw_peek() {
                            bg_raw = Some(raw);
                            bg_img = None;
                            if let Ok(mut i) = info.lock() {
                                i.video_error = v.error.lock().map(|e| e.clone()).unwrap_or_default();
                            }
                        }
                    }

                    let snap_now = snap.lock().map(|g| g.clone()).unwrap_or_default();
                    let frame = render::compose(
                        &mut canvas,
                        &fonts,
                        &cfg,
                        &snap_now,
                        bg_img.as_ref(),
                        bg_raw.as_deref(),
                    );

                    // 预览用原尺寸 RGBA
                    if let Ok(mut p) = preview.lock() {
                        *p = Some(canvas.px.clone());
                    }

                    if helper_checked.elapsed().as_secs_f32() >= 2.0 {
                        helper_ok = sensors::process_running("USBLCD");
                        helper_checked = std::time::Instant::now();
                    }

                    if running.load(Ordering::Relaxed) {
                        if !scr.is_open {
                            let ok = scr.open();
                            if ok && !helper_ok {
                                let _ = sensors::start_helper();
                                helper_ok = true;
                            }
                        }
                        if scr.is_open {
                            scr.send(&frame);
                        }
                        if let Ok(mut i) = info.lock() {
                            i.screen_ready = scr.is_open;
                            i.created_section = scr.created;
                            i.screen_error = scr.error.clone();
                            i.helper_ok = helper_ok;
                        }
                    }

                    let target = 1000u64 / cfg.target_fps.clamp(1, 30) as u64;
                    let spent = last_frame.elapsed().as_millis() as u64;
                    if spent < target {
                        std::thread::sleep(std::time::Duration::from_millis(target - spent));
                    }
                    last_frame = std::time::Instant::now();
                }
                if let Some(mut v) = vsrc {
                    v.stop();
                }
            });
        }

        let mut app = Self {
            settings,
            snap,
            info,
            preview,
            running,
            worker_stop,
            tab: Tab::Device,
            selected: 0,
            drag_item: None,
            drag_item_grab: egui::Vec2::ZERO,
            selected_widget: None,
            drag_widget: None,
            drag_grab: egui::Vec2::ZERO,
            fonts: render::Fonts::load(),
            style_page: 0,
            style_pending_del: None,
            font_defs_loaded: true,
            preview_tex: None,
            preview_dirty: true,
            preview_dims: (DESIGN_W as f32, DESIGN_H as f32),
            preview_rot: 0,
            style_thumbs: std::collections::HashMap::new(),
            hero_tex: std::collections::HashMap::new(),
            auto_start: autostart::enabled(),
            start_hidden: s.start_minimized || tray_arg,
            hid_once: false,
            hidden: false,
            want_hidden: false,
            was_minimized: false,
            win_hidden: false,
            ever_shown: false,
            hwnd: window_hwnd(cc),
            region_blank: false,
            last_render: std::time::Instant::now(),
            last_saved: String::new(),
            log: Vec::new(),
        };

        // 趁第一帧还没画完、eframe 还没把窗口显示出来先把可见区域清空
        // 这一下赶在它露面之前那 30 多毫秒的黑框就没了
        if app.start_hidden {
            app.region_blank = true;
            set_blank_region(app.hwnd, true);
        }
        app
    }

    /// 首次进入时把托盘挂上并按配置决定要不要藏起杀
    fn setup_shell(&mut self, ctx: &egui::Context) {
        if self.font_defs_loaded && !self.hid_once {
            tray::spawn(ctx.clone(), self.running.clone());
            self.hid_once = true;
            if self.start_hidden {
                // 不最小化直接标记成"要藏"。下一帀update 釀ShowWindow(SW_HIDE)
                // 会把它从画面上、任务栏、Alt+Tab 里一起摘掉
                // 之所以不先最小化最小化本身就会在任务栏留一格而第三方任务栀
                // MyDockFinder还可能认不出来藏不干净
                self.hidden = true;
                self.want_hidden = true;
            }
        }
    }

    /// 让窗口的「可见区域」跟「该不该露脸」保持一致
    ///
    /// 缩在托盘里或者启动就藏着的时候把可见区域清空窗口还占着位置
    /// 任务栏按钮也照旧但一个像素都不画从小托盘回来时再恢复
    /// 这样就不需要靠窗外的坐标来"藀窗口位置能正常记住
    fn sync_blank_region(&mut self) {
        let blank = self.start_hidden && !self.ever_shown;
        if blank != self.region_blank {
            self.region_blank = blank;
            set_blank_region(self.hwnd, blank);
        }
    }

    fn cfg(&self) -> Settings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 自绘的标题条
    ///
    /// 窗口是无边框的`main()` 釀`with_decorations(false)`系统那圈标题栀
    /// 是深灰的跟浅色面板压根不是一个色系而且非客户区还会把左边立绘压掀
    /// 一截。所以拖动、双击最大化、最小化/最大化/关闭三个按钮都得自己画
    fn title_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let h = 34.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), h),
            egui::Sense::hover(),
        );

        // 底部一条发丝线把标题条和内容分开
        ui.painter()
            .rect_filled(rect, CornerRadius::same(0), pal().panel);
        ui.painter().line_segment(
            [
                egui::pos2(rect.left(), rect.bottom() - 0.5),
                egui::pos2(rect.right(), rect.bottom() - 0.5),
            ],
            Stroke::new(1.0, pal().line),
        );

        // ---------- 左边名孀+ 版本 ----------
        let name_font = egui::FontId::proportional(12.5);
        ui.painter().text(
            rect.left_center() + egui::vec2(18.0, 0.0),
            egui::Align2::LEFT_CENTER,
            APP_NAME,
            name_font.clone(),
            pal().text,
        );
        let name_w = ui.fonts(|f| {
            f.layout_no_wrap(APP_NAME.to_owned(), name_font, pal().text)
                .size()
                .x
        });
        ui.painter().text(
            rect.left_center() + egui::vec2(18.0 + name_w + 9.0, 0.5),
            egui::Align2::LEFT_CENTER,
            APP_VERSION,
            egui::FontId::proportional(10.0),
            pal().muted,
        );

        // ---------- 右边三个窗口按钀----------
        let bw = 36.0;
        let close = egui::Rect::from_min_size(
            egui::pos2(rect.right() - bw, rect.top()),
            egui::vec2(bw, h),
        );
        let maxi = close.translate(egui::vec2(-bw, 0.0));
        let mini = maxi.translate(egui::vec2(-bw, 0.0));

        if win_btn(ui, mini, WinBtn::Min).clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if win_btn(ui, maxi, WinBtn::Max).clicked() {
            let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
        }
        if win_btn(ui, close, WinBtn::Close).clicked() {
            // 走正常的关闭请求update() 里会把它翻译戀收进托盘"
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // ---------- 拖动 / 双击最大化 ----------
        let drag = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(mini.left().max(rect.left()), rect.bottom()),
        );
        let resp = ui.interact(
            drag,
            ui.id().with("title_drag"),
            egui::Sense::click_and_drag(),
        );
        if resp.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if resp.double_clicked() {
            let maxed = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
        }
    }

    /// 无边框窗口没有系统给的缩放热区四个边和四个角得自己铺一层
    fn window_grips(&mut self, ctx: &egui::Context) {
        use egui::{CursorIcon, ResizeDirection as D};
        let s = ctx.screen_rect();
        let g = 5.0;
        let grips: [(D, egui::Rect, CursorIcon); 8] = [
            (
                D::North,
                egui::Rect::from_min_max(
                    egui::pos2(s.left() + g, s.top()),
                    egui::pos2(s.right() - g, s.top() + g),
                ),
                CursorIcon::ResizeVertical,
            ),
            (
                D::South,
                egui::Rect::from_min_max(
                    egui::pos2(s.left() + g, s.bottom() - g),
                    egui::pos2(s.right() - g, s.bottom()),
                ),
                CursorIcon::ResizeVertical,
            ),
            (
                D::West,
                egui::Rect::from_min_max(
                    egui::pos2(s.left(), s.top() + g),
                    egui::pos2(s.left() + g, s.bottom() - g),
                ),
                CursorIcon::ResizeHorizontal,
            ),
            (
                D::East,
                egui::Rect::from_min_max(
                    egui::pos2(s.right() - g, s.top() + g),
                    egui::pos2(s.right(), s.bottom() - g),
                ),
                CursorIcon::ResizeHorizontal,
            ),
            (
                D::NorthWest,
                egui::Rect::from_min_max(s.min, s.min + egui::vec2(g, g)),
                CursorIcon::ResizeNwSe,
            ),
            (
                D::NorthEast,
                egui::Rect::from_min_max(
                    egui::pos2(s.right() - g, s.top()),
                    egui::pos2(s.right(), s.top() + g),
                ),
                CursorIcon::ResizeNeSw,
            ),
            (
                D::SouthWest,
                egui::Rect::from_min_max(
                    egui::pos2(s.left(), s.bottom() - g),
                    egui::pos2(s.left() + g, s.bottom()),
                ),
                CursorIcon::ResizeNeSw,
            ),
            (
                D::SouthEast,
                egui::Rect::from_min_max(s.max - egui::vec2(g, g), s.max),
                CursorIcon::ResizeNwSe,
            ),
        ];
        // 这里不建 Area。Area 是独立的 layer会参与指针命中的排庀—哪怕它
        // 只是贴在边上 5px指针掠过时也会改变"谁在 hover"表现出来就是按钀
        // 一直在闪、文字控件的光标在箭头和 I 形之间反复跳。改成直接读指针位置
        // 层层排序里就没有第二个东西了
        let (hover, pressed) = ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.primary_pressed(),
            )
        });
        let Some(p) = hover else { return };
        for (dir, r, cursor) in grips {
            if !r.contains(p) {
                continue;
            }
            ctx.set_cursor_icon(cursor);
            if pressed {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
            break;
        }
    }
    fn edit<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> R {
        let mut g = self.settings.lock().unwrap();
        f(&mut g)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 窗口实际是不是缩着以 winit 报的状态为准。want_hidden 只是"想让它在托盘釀
        // 的意囀—用户从任务栏把窗口点回来之后这个意图就过期了得清掉
        // 否则 hidden 会一直为真预览纹理永远不刷新看起来就是一块黑
        //
        // 注意只能做「最小化 ↀ正常」的下降沿检测发完 Minimized(true) 之后
        // 要过几帧 winit 才真正生效那几帀minimized 还是 false直接按
        // "没最小化就清掉意囀会误清窗口就收不进托盘了
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if self.was_minimized && !minimized {
            self.want_hidden = false;
            self.ever_shown = true;
        }
        self.was_minimized = minimized;
        // 启动即隐藏的窗口从来没有 minimized 过得单独撑佀hidden
        self.hidden = minimized || (self.start_hidden && !self.ever_shown);

        // 藏在托盘里的时候窗口根本看不见没必要跟着刷露出来也只要十几帧就够看
        let ui_period = if self.hidden {
            std::time::Duration::from_millis(500)
        } else {
            std::time::Duration::from_millis(70)
        };
        ctx.request_repaint_after(ui_period);

        self.setup_shell(ctx);

        // 托盘动作必须在「要不要藏」之前处理。
        //
        // 窗口藏着的时候 update() 本来不会跑，是托盘线程直接 ShowWindow 把它
        // 撬开的（见 tray::wake_main_window）。撬开之后这一帧 want_hidden 还是
        // true，如果先执行下面那段隐藏，就会当场又把它藏回去，然后 update()
        // 再次停摆 —— 表现就是点了托盘没反应。所以先读托盘请求。
        if tray::show_requested() {
            self.want_hidden = false;
            self.ever_shown = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);        }

        // 「收进托盘」真的把窗口藏起来不是最小化
        //
        // 最小化只是收进任务栏Alt+Tab 里还有它第三方任务栏MyDockFinder
        // 也照样给它留一格。用户要的是「窗口没了托盘图标还在后台继续出图」
        // 那就只能 ShowWindow(SW_HIDE)。托盘图标是自建的窗口跟主窗口无关
        // 藏起来不影响它
        //
        // 两个方向都要每帧核eframe 画完第一帧会强制 set_visible(true)
        // 只藏一次会被它顶回来。函数内部用 IsWindowVisible 判过不会重复调
        if self.want_hidden {
            show_window(self.hwnd, false);
            if !self.win_hidden {
                self.win_hidden = true;
                set_taskbar_visible(self.hwnd, false);
            }
        } else if self.win_hidden {
            self.win_hidden = false;
            show_window(self.hwnd, true);
            set_taskbar_visible(self.hwnd, true);
        }

        // 关窗按钮 = 收进托盘除非是从托盘里点了退出
        if ctx.input(|i| i.viewport().close_requested()) && !tray::allow_close() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.want_hidden = true;
        }

        // 托盘菜单动作（显示窗口在上面「收进托盘」之前就处理过了）
        if tray::toggle_requested() {
            let now = !self.running.load(Ordering::Relaxed);
            self.running.store(now, Ordering::SeqCst);
            self.log.push(format!(
                "托盘：{}出图",
                if now { "开始" } else { "停止" }
            ));
        }
        if tray::exit_requested() {
            self.worker_stop.store(true, Ordering::SeqCst);
            self.cfg().save();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // 「用户自己把窗口叫回来了」只有一种可靠判据winit 报的 minimized 什
        // true 变回 false上面那段下降沿检测。以前这里还多一杀
        //「want_hidden && minimized_sent && !minimized 就算叫回来了」—
        // 那是错的命令刚发出去、winit 还没生效的那几帧釀minimized 仍是 false
        // 于是第一帧就技已经露过靀写死了空区域立刻被恢复
        // 冷启动那一闪又回来了
        self.hidden = minimized || (self.start_hidden && !self.ever_shown);
        self.sync_blank_region();
        // 任务栏按钮跟着「想不想待在托盘里」走而且每帧都对一遀—
        // winit 处理最小化 / 边框变更时会把扩展样式重写一遍单次设置会被抹掉
        // 状态没变时这个函数立刻 return每帧调没有代价
        if self.win_hidden {
            set_taskbar_visible(self.hwnd, false);
        }

        // 预览纹理按当前旋转角转好再传界面上看到的就是屏幕上那个朝向
        let px = if self.hidden {
            None
        } else {
            self.preview.lock().ok().and_then(|g| g.clone())
        };
        let rot_now = self.settings.lock().map(|s| s.rotation % 360).unwrap_or(0);
        if let Some(px) = px {
            let (buf, w, h) = render::rotate_rgba(&px, DESIGN_W as usize, DESIGN_H as usize, rot_now);
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &buf);
            self.preview_dims = (w as f32, h as f32);
            match &mut self.preview_tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.preview_tex =
                        Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR))
                }
            }
            self.preview_rot = rot_now;
        }

        let info = self.info.lock().map(|i| InfoSnapshot::from(&*i)).unwrap_or_default();
        let mut cfg = self.cfg();

        // 无边框窗口得自己铺缩放热匀
        self.window_grips(ctx);

        egui::CentralPanel::default()
            .frame(Frame::new().fill(pal().bg).inner_margin(Margin::same(0)))
            .show(ctx, |ui| {
                self.title_bar(ui, ctx);
                ui.horizontal_top(|ui| {
                    ui.set_width(ui.available_width());
                    // ---------- 左侧栏四项导航 + 立绘 ----------
                    ui.vertical(|ui| {
                        ui.set_width(232.0);
                        ui.add_space(14.0);
                        for (target, text, hint) in [
                            (Tab::Device, "屏幕设备", "画面与样式"),
                            (Tab::Fields, "系统信息", "实时数据"),
                            (Tab::Attrs, "属性显示", "显示列表与文字"),
                            (Tab::Settings, "设置", "外观与运行"),
                        ] {
                            nav_item(ui, &mut self.tab, target, text, hint);
                        }
                        // 三套配色各配一张立绘跟着主题一起换
                        // 明昼是白发精灵、百日橘是橙发的、黑夜是黑发的
                        // 明昼/百日橘那两张都是浅底黑夜那张是深底
                        // 所以人物本身也得跟着色调走不然贴在深色面板上像贴纸
                        // 素材都裁成全身、头顶留一点空按底边对齐、水平居中
                        let hero_kind = theme::current_kind();
                        let (hero_w, hero_h) = hero_native(hero_kind);
                        let avail_h = ui.available_height() - 6.0;
                        let scale: f32 =
                            (232.0f32 * 0.96 / hero_w).min(avail_h.max(0.0) / hero_h);
                        let rest = avail_h - hero_h * scale;
                        if rest > 0.0 {
                            ui.add_space(rest);
                        }
                        hero(ui, &mut self.hero_tex, ctx, hero_kind);
                    });

                    // ---------- 右侧内容匀----------
                    ui.vertical(|ui| {
                        // 右边畀18px 呼吸佀—卡片贴着窗口边会让人觉得被切掉了
                        // 尤其无边框窗口那圈空白就是唯一皀边框"
                        ui.set_width((ui.available_width() - 18.0).max(320.0));
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.add_space(2.0);
                            let (title, sub) = match self.tab {
                                Tab::Device => ("屏幕设备", "屏上画面与样式候选"),
                                Tab::Fields => ("系统信息", "这台机器现在的实时数据"),
                                Tab::Attrs => ("属性显示", "哪些字段上屏、长什么样"),
                                Tab::Settings => ("设置", "外观、背景、运行方式"),
                            };
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(title).size(17.0).strong().color(pal().text),
                                );
                                ui.label(RichText::new(sub).size(11.0).color(pal().muted));
                            });
                            // 标题行右侧原来是两个状态胶囊去掉亀—这几条状怀
                            // 在「设置」页的运行卡片里都有摆在这儿只是抢版面
                        });
                        ui.add_space(14.0);
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add_space(2.0);
                                match self.tab {
                                    Tab::Device => self.ui_device(ui, &mut cfg, &info),
                                    Tab::Fields => self.ui_fields(ui, &mut cfg, &info),
                                    Tab::Attrs => self.ui_attrs(ui, &mut cfg),
                                    Tab::Settings => self.ui_settings(ui, &mut cfg, &info),
                                }
                                ui.add_space(18.0);
                            });
                    });
                });
            });

        // 设置真正变了才回写避免每两秒刷一次盘
        let new_json = serde_json::to_string(&cfg).unwrap_or_default();
        if new_json != self.last_saved {
            self.edit(|s| *s = cfg.clone());
            cfg.save();
            self.last_saved = new_json;
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker_stop.store(true, Ordering::SeqCst);
        self.cfg().save();
    }
}

/// 侀UI 用的 info 拷贝避免持锁
#[derive(Default, Clone)]
struct InfoSnapshot {
    source_label: String,
    hw_ok: bool,
    screen_error: String,
    screen_ready: bool,
    helper_ok: bool,
    elevated: bool,
    temp_source: String,
    temp_candidates: Vec<(String, f32)>,
    cpu_name: String,
    gpu_name: String,
    video_error: String,
    loop_note: String,
    bg_label: String,
}

impl InfoSnapshot {
    /// CPU 名的短版i9-14900K 这种把厂商词和多余空格去掉
    fn cpu_name_short(&self) -> String {
        let n = self.cpu_name.trim();
        if n.is_empty() || n == " " {
            return String::new();
        }
        let mut s = n.to_string();
        for drop in [
            "Intel(R) ",
            "Intel ",
            "AMD ",
            "Core(TM) ",
            "Core ",
            "12th Gen ",
            "13th Gen ",
            "14th Gen ",
            "CPU @ ",
        ] {
            s = s.replace(drop, "");
        }
        if let Some(i) = s.find(" @") {
            s.truncate(i);
        }
        s.trim().chars().take(22).collect()
    }

    fn gpu_name_short(&self) -> String {
        let n = self.gpu_name.trim();
        if n.is_empty() || n == " " {
            return String::new();
        }
        let mut s = n.to_string();
        for drop in ["NVIDIA ", "GeForce ", "AMD ", "Radeon ", "Intel(R) ", "Graphics"] {
            s = s.replace(drop, "");
        }
        s.trim().chars().take(22).collect()
    }
}

impl From<&RuntimeInfo> for InfoSnapshot {
    fn from(i: &RuntimeInfo) -> Self {
        Self {
            source_label: if i.source.is_empty() {
                "数据源：未连接".into()
            } else {
                i.source.clone()
            },
            hw_ok: !i.source.is_empty() && i.source.contains("HWiNFO"),
            screen_error: i.screen_error.clone(),
            screen_ready: i.screen_ready,
            helper_ok: i.helper_ok,
            elevated: i.elevated,
            temp_source: i.temp_source.clone(),
            temp_candidates: i.temp_candidates.clone(),
            cpu_name: i.cpu_name.clone(),
            gpu_name: i.gpu_name.clone(),
            video_error: i.video_error.clone(),
            loop_note: i.loop_note.clone(),
            bg_label: i.bg_label.clone(),
        }
    }
}

impl App {
    /// 屏幕设备页左边是屏上画面右上角能转右边是样式候选
    fn ui_device(&mut self, ui: &mut egui::Ui, cfg: &mut Settings, info: &InfoSnapshot) {
        let snap_now = self.snap.lock().map(|s| s.clone()).unwrap_or_default();
        let gap = 14.0;
        let avail = ui.available_width();
        // 两列等宽 —用户要的是「观感对称」一辀56% 一辀44% 永远差一截
        let col_w = ((avail - gap) * 0.5).max(280.0);
        let (left_w, right_w) = (col_w, col_w);

        // 先把预览尺寸算出来两张卡都按同一高度对齐右边内容没满也不会缩成一条
        let card_pad = 36.0;
        let pv_w = (left_w - card_pad).max(80.0);
        // 预览昀4:3 的整块不再往外套黑框 —卡片高度就照着它算
        // 多出来的高度全给画面。以前按"卡片吃满一页、画面居中缩进黑桀做
        // 画面永远只有中间那一小块
        let pv_h = (pv_w * DESIGN_H as f32 / DESIGN_W as f32).max(120.0);
        // 标题血+ 预览 + 透明度条 + 背景几项控件 ≀230
        let card_h = (pv_h + 230.0).clamp(360.0, ui.available_height().max(360.0));

        ui.horizontal_top(|ui| {
            // ---------- 左屏上画面 ----------
            ui.vertical(|ui| {
                ui.set_width(left_w);
                big_card_min(ui, card_h, |ui| {
                    ui.horizontal(|ui| {
                        section_title(ui, "屏上画面");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ghost_btn(ui, "画面旋转").clicked() {
                                cfg.rotation = (cfg.rotation + 90) % 360;
                            }
                            ui.label(
                                RichText::new(format!("{}°", cfg.rotation))
                                    .size(11.0)
                                    .color(pal().muted),
                            );
                        });
                    });

                    // ---------- 预览 ----------
                    // 不再套黑框整块画面区就是屏幕内容本身外面那圈深色
                    // 只在还没渲染出第一帧的时候露一下。以前用 4:3 黑框 + 居中
                    // 缩放画面只占中间一小块看着像在看缩略图
                    let size = egui::vec2(pv_w, pv_h);
                    let (rect, presp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());

                    let (tw, th) = self.preview_dims;
                    let fit = (rect.width() / tw.max(1.0))
                        .min(rect.height() / th.max(1.0))
                        .max(0.0001);
                    // 整张画布等比装进去contain—必须看得见全部内容
                    // 不然拖组件时会有地方够不着
                    let img_rect = egui::Rect::from_center_size(
                        rect.center(),
                        egui::vec2(tw * fit, th * fit),
                    );

                    if let Some(tex) = &self.preview_tex {
                        ui.painter().image(
                            tex.id(),
                            img_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        ui.painter()
                            .rect_filled(img_rect, CornerRadius::same(6), pal().card_hi);
                    }

                    // ---------- 预览里直接拖 ----------
                    // 组件和文字都记在设计画布20×240里而预览可能被转过 90°
                    // 所以两边换算都要带上旋转
                    let rot = self.preview_rot;
                    let ptr = ui.ctx().input(|i| i.pointer.interact_pos());
                    let mut hit_sel = None;
                    let mut hit_item = None;

                    if let Some(p) = ptr {
                        if (presp.drag_started() || presp.clicked()) && img_rect.contains(p) {
                            let (dx, dy) = preview_to_design(p, img_rect, rot);
                            // 先看有没有点在圆环里再看文孀—圆环大抢不过文字才怀
                            let mut best: Option<(usize, f32)> = None;
                            for (i, w) in cfg.widgets.iter().enumerate() {
                                if !w.visible {
                                    continue;
                                }
                                let d = ((dx - w.x).powi(2) + (dy - w.y).powi(2)).sqrt();
                                if d <= (w.size * 0.5).max(7.0)
                                    && best.map_or(true, |(_, bd)| d < bd)
                                {
                                    best = Some((i, d));
                                }
                            }
                            if let Some((i, _)) = best {
                                hit_sel = Some(i);
                                if presp.drag_started() {
                                    self.drag_widget = Some(i);
                                    self.drag_grab =
                                        egui::vec2(dx - cfg.widgets[i].x, dy - cfg.widgets[i].y);
                                }
                            } else {
                                // 没点中环就找文字落在哪块字的框里就抓哪一
                                let boxes = render::item_boxes(&self.fonts, cfg, &snap_now);
                                let mut hit: Option<(usize, f32)> = None;
                                for (i, bx, by, bw, bh) in boxes {
                                    if dx >= bx - 2.0
                                        && dx <= bx + bw + 2.0
                                        && dy >= by - 2.0
                                        && dy <= by + bh + 2.0
                                    {
                                        let area = bw * bh;
                                        if hit.map_or(true, |(_, a)| area < a) {
                                            hit = Some((i, area));
                                        }
                                    }
                                }
                                if let Some((i, _)) = hit {
                                    hit_item = Some(i);
                                    if presp.drag_started() {
                                        self.drag_item = Some(i);
                                        self.drag_item_grab = egui::vec2(
                                            dx - cfg.items[i].x,
                                            dy - cfg.items[i].y,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if let Some(i) = hit_sel {
                        self.selected_widget = Some(i);
                    }
                    if let Some(i) = hit_item {
                        self.selected = i;
                    }

                    if let Some(i) = self.drag_widget {
                        if presp.dragged() {
                            if let Some(p) = ptr {
                                let (dx, dy) = preview_to_design(p, img_rect, rot);
                                if let Some(w) = cfg.widgets.get_mut(i) {
                                    w.x = (dx - self.drag_grab.x)
                                        .clamp(-40.0, DESIGN_W as f32 + 40.0);
                                    w.y = (dy - self.drag_grab.y)
                                        .clamp(-40.0, DESIGN_H as f32 + 40.0);
                                }
                            }
                        }
                        if presp.drag_stopped() {
                            self.drag_widget = None;
                        }
                    }
                    if let Some(i) = self.drag_item {
                        if presp.dragged() {
                            if let Some(p) = ptr {
                                let (dx, dy) = preview_to_design(p, img_rect, rot);
                                if let Some(it) = cfg.items.get_mut(i) {
                                    it.x = (dx - self.drag_item_grab.x)
                                        .clamp(-400.0, DESIGN_W as f32 + 200.0);
                                    it.y = (dy - self.drag_item_grab.y)
                                        .clamp(-400.0, DESIGN_H as f32 + 200.0);
                                }
                            }
                        }
                        if presp.drag_stopped() {
                            self.drag_item = None;
                        }
                    }

                    // 组件在预览里的位置提示选中的画实圈其余画虚一点的圈
                    if !cfg.widgets.is_empty() {
                        let w_scale = fit;
                        for (i, w) in cfg.widgets.iter().enumerate() {
                            if !w.visible {
                                continue;
                            }
                            let c = design_to_preview(w.x, w.y, img_rect, rot);
                            let r = (w.size * 0.5 * w_scale).max(4.0);
                            let on = self.selected_widget == Some(i);
                            let col = if on {
                                pal().accent
                            } else {
                                pal().accent.gamma_multiply(0.45)
                            };
                            ui.painter().circle_stroke(
                                c,
                                r,
                                Stroke::new(if on { 1.6 } else { 1.0 }, col),
                            );
                        }
                    }

                    // 选中的那块字也框一下好知道现在拖的是谁
                    if let Some((_, bx, by, bw, bh)) =
                        render::item_boxes(&self.fonts, cfg, &snap_now)
                            .into_iter()
                            .find(|(i, ..)| *i == self.selected)
                    {
                        let corners = [
                            (bx, by),
                            (bx + bw, by),
                            (bx + bw, by + bh),
                            (bx, by + bh),
                        ];
                        let pts: Vec<egui::Pos2> = corners
                            .iter()
                            .map(|(x, y)| design_to_preview(*x, *y, img_rect, rot))
                            .collect();
                        let stroke = Stroke::new(1.2, pal().accent.gamma_multiply(0.8));
                        for k in 0..4 {
                            ui.painter()
                                .line_segment([pts[k], pts[(k + 1) % 4]], stroke);
                        }
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        label(ui, "透明度");
                        ui.add(
                            egui::Slider::new(&mut cfg.background_opacity, 0.0..=1.0)
                                .fixed_decimals(2),
                        );
                    });
                    ui.add_space(6.0);
                    self.ui_bg_controls(ui, cfg);
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "背景：{}　·　文字和圆环都能在画面上直接拖",
                            if info.bg_label.is_empty() {
                                "未设置".to_string()
                            } else {
                                info.bg_label.clone()
                            }
                        ))
                        .size(11.0)
                        .color(pal().muted),
                    );
                });
            });

            ui.add_space(gap);

            // ---------- 右样式候退----------
            ui.vertical(|ui| {
                ui.set_width(right_w);
                big_card_min(ui, card_h, |ui| {
                    ui.horizontal(|ui| {
                        section_title(ui, "样式候选");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let save_resp = primary_btn(ui, "保存当前样式");
                            if save_resp.clicked() {
                                match self.save_style(cfg) {
                                    Ok(_) => {
                                        // 先把新样式写回共享的 Settings否则下一帀
                                        // self.cfg() 还是旧的一份刚存的样式就没了
                                        self.edit(|s| *s = cfg.clone());
                                        cfg.save();
                                        self.last_saved =
                                            serde_json::to_string(cfg).unwrap_or_default();
                                    }
                                    Err(e) => {
                                        self.log.push(format!("保存样式失败：{e}"));
                                    }
                                }
                            }
                        });
                    });
                    ui.label(
                        RichText::new("点缩略图套用，点右上角 × 删掉")
                            .size(10.5)
                            .color(pal().muted),
                    );
                    ui.add_space(8.0);

                    let per_page = 15usize; // 3 列 × 5 行
                    let total = cfg.styles.len();
                    // 永远多留一格空的空框昀下一个样式往这儿攀的提示
                    // 存满 15 个就自动翻到下一页那一格正好在最后
                    let slots = total + 1;
                    let pages = ((slots + per_page - 1) / per_page).max(1);
                    if self.style_page >= pages {
                        self.style_page = pages - 1;
                    }
                    let start = self.style_page * per_page;
                    let end = (start + per_page).min(slots);

                    // 缩略图缓存文件在就加载
                    for i in start..total.min(end) {
                        let path = cfg.styles[i].thumb.clone();
                        if !path.is_empty() && !self.style_thumbs.contains_key(&path) {
                            if let Ok(bytes) = std::fs::read(&path) {
                                if let Ok(img) = image::load_from_memory(&bytes) {
                                    let rgba = img.to_rgba8();
                                    let (w, h) = rgba.dimensions();
                                    let ci = egui::ColorImage::from_rgba_unmultiplied(
                                        [w as usize, h as usize],
                                        rgba.as_raw(),
                                    );
                                    let key = format!("style_thumb_{i}_{path}");
                                    let t = ui.ctx().load_texture(
                                        key,
                                        ci,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.style_thumbs.insert(path.clone(), t);
                                }
                            }
                        }
                    }

                    let cols = 3usize;
                    let cell_gap = 10.0;
                    let cell_w = ((ui.available_width() - cell_gap * (cols as f32 - 1.0))
                        / cols as f32)
                        .max(60.0);
                    // 4:3 横框 —屏幕本身是横的缩略图跟它一个比例才不别扭
                    let thumb_h = cell_w * 3.0 / 4.0;

                    let mut clicked_apply: Option<usize> = None;
                    let mut clicked_del: Option<usize> = None;

                    for row in 0..5usize {
                        let row_start = start + row * cols;
                        if row_start >= end {
                            break;
                        }
                        ui.horizontal(|ui| {
                            // 间距自己排add_space技egui 默认的项间距关掉
                            // 否则三列会多出两殀8px整排撑出卡片右边
                            let old_gap = ui.spacing().item_spacing.x;
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for col in 0..cols {
                                let i = row_start + col;
                                if i >= end {
                                    break;
                                }
                                let rd = CornerRadius::same(8);
                                // ---------- 多出来的那一格空框 ----------
                                if i >= total {
                                    let (r, _) = ui.allocate_exact_size(
                                        egui::vec2(cell_w, thumb_h),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        r,
                                        rd,
                                        pal().card_hi.gamma_multiply(0.45),
                                    );
                                    let mut x = r.left() + 8.0;
                                    while x < r.right() - 8.0 {
                                        let x2 = (x + 6.0).min(r.right() - 8.0);
                                        for y in [r.top() + 0.5, r.bottom() - 0.5] {
                                            ui.painter().line_segment(
                                                [egui::pos2(x, y), egui::pos2(x2, y)],
                                                Stroke::new(1.0, pal().line),
                                            );
                                        }
                                        x += 12.0;
                                    }
                                    if col + 1 < cols && i + 1 < end {
                                        ui.add_space(cell_gap);
                                    }
                                    continue;
                                }
                                // ---------- 已存的样开----------
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(cell_w, thumb_h),
                                    egui::Sense::click(),
                                );
                                ui.painter().rect_filled(rect, rd, pal().card_hi);
                                if let Some(tex) = self.style_thumbs.get(&cfg.styles[i].thumb) {
                                    ui.painter().image(
                                        tex.id(),
                                        rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        Color32::WHITE,
                                    );
                                }
                                let stroke_c = if resp.hovered() {
                                    pal().accent
                                } else {
                                    pal().line
                                };
                                if resp.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        rd,
                                        Color32::from_rgba_unmultiplied(
                                            pal().accent.r(),
                                            pal().accent.g(),
                                            pal().accent.b(),
                                            20,
                                        ),
                                    );
                                }
                                ui.painter().rect_stroke(
                                    rect,
                                    rd,
                                    Stroke::new(1.0, stroke_c),
                                    egui::StrokeKind::Inside,
                                );
                                // 删除按钮右上角一个圆庀画出来的叀
                                let del_sz = 18.0;
                                let del_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.right() - del_sz - 5.0,
                                        rect.top() + 5.0,
                                    ),
                                    egui::vec2(del_sz, del_sz),
                                );
                                let del_resp = ui.interact(
                                    del_rect,
                                    ui.id().with(("style_del", i)),
                                    egui::Sense::click(),
                                );
                                let dc = if del_resp.hovered() {
                                    pal().bad
                                } else {
                                    Color32::from_rgba_unmultiplied(20, 24, 32, 170)
                                };
                                ui.painter().rect_filled(del_rect, CornerRadius::same(9), dc);
                                let c = del_rect.center();
                                let r = 4.0;
                                let xc = Color32::WHITE;
                                let sw = 1.6;
                                ui.painter().line_segment(
                                    [c + egui::vec2(-r, -r), c + egui::vec2(r, r)],
                                    Stroke::new(sw, xc),
                                );
                                ui.painter().line_segment(
                                    [c + egui::vec2(r, -r), c + egui::vec2(-r, r)],
                                    Stroke::new(sw, xc),
                                );
                                // 只有「按下位置不在删除按钮上」才算点缩略图套用
                                // 单看 del_resp.clicked() / resp.clicked() 不够稳
                                // 两个 interact 区域相邻时同一帧里可能都被判成 clicked
                                let hit_del = del_resp.clicked();
                                let hit_cell = resp.clicked()
                                    && resp
                                        .interact_pointer_pos()
                                        .map_or(true, |p| !del_rect.contains(p));
                                if hit_del {
                                    clicked_del = Some(i);
                                } else if hit_cell {
                                    clicked_apply = Some(i);
                                }
                                if col + 1 < cols && i + 1 < end {
                                    ui.add_space(cell_gap);
                                }
                            }
                            ui.spacing_mut().item_spacing.x = old_gap;
                        });
                        ui.add_space(cell_gap);
                    }

                    if total == 0 {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("还没有存过样式。调好布局后点上面那个按钮存一份。")
                                .size(11.0)
                                .color(pal().muted),
                        );
                    }

                    // 翻页
                    if pages > 1 {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui.small_button("‹").clicked() && self.style_page > 0 {
                                self.style_page -= 1;
                            }
                            ui.label(
                                RichText::new(format!("{}/{}", self.style_page + 1, pages))
                                    .size(11.0)
                                    .color(pal().muted),
                            );
                            if ui.small_button("›").clicked() && self.style_page + 1 < pages {
                                self.style_page += 1;
                            }
                        });
                    }

                    if let Some(i) = clicked_del {
                        self.style_pending_del = Some(i);
                    }
                    if let Some(i) = clicked_apply {
                        if let Some(st) = cfg.styles.get(i).cloned() {
                            st.apply_to(cfg);
                            theme::set_current(&cfg.theme);
                            theme::apply(ui.ctx(), &cfg.theme);
                        }
                    }
                });
            });
        });

        self.confirm_delete_style(ui, cfg);
    }

    /// 删除确认框。挂在窗口正中点「删除」才真删
    fn confirm_delete_style(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        let Some(idx) = self.style_pending_del else {
            return;
        };
        if idx >= cfg.styles.len() {
            self.style_pending_del = None;
            return;
        }
        let mut do_del = false;
        let mut cancel = false;

        // 用自己的卡片画确认框不甀egui 默认皀Window —默认那套昀
        // 浅灰标题栀+ 系统风按钮跟我们这三套配色完全不搭
        let frame = egui::Frame::new()
            .fill(pal().panel)
            .corner_radius(CornerRadius::same(14))
            .inner_margin(Margin::symmetric(22, 18))
            .stroke(Stroke::new(1.0, pal().line))
            .shadow(egui::epaint::Shadow {
                offset: [0, 8],
                blur: 28,
                spread: 0,
                color: Color32::from_black_alpha(58),
            });

        let resp = egui::Modal::new(egui::Id::new("style_del_modal"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(38))
            .show(ui.ctx(), |ui| {
                ui.set_width(292.0);
                ui.label(
                    RichText::new("删除这个样式？")
                        .size(15.0)
                        .strong()
                        .color(pal().text),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("缩略图文件也会一起删掉，没法恢复。")
                        .size(11.5)
                        .color(pal().muted),
                );
                ui.add_space(18.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if danger_btn(ui, "删除").clicked() {
                        do_del = true;
                    }
                    ui.add_space(8.0);
                    if ghost_btn(ui, "取消").clicked() {
                        cancel = true;
                    }
                });
            });

        // 点遮罩、按 Esc 都算取消 —确认框不该只能从按钮出去
        if resp.should_close() {
            cancel = true;
        }
        if do_del {
            let path = cfg.styles[idx].thumb.clone();
            cfg.styles.remove(idx);
            if !path.is_empty() {
                self.style_thumbs.remove(&path);
                let _ = std::fs::remove_file(&path);
            }
            self.style_pending_del = None;
        } else if cancel {
            self.style_pending_del = None;
        }
    }

    /// 把当前画面存成一张缩略图连带整套布局塞进 `styles`
    fn save_style(&mut self, cfg: &mut Settings) -> Result<(), String> {
        let px = self
            .preview
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| "画面还没渲染出来，等一秒再试".to_string())?;
        let (buf, w, h) = render::rotate_rgba(&px, DESIGN_W as usize, DESIGN_H as usize, cfg.rotation);
        let dir = settings::Settings::path()
            .parent()
            .map(|p| p.join("styles"))
            .unwrap_or_else(|| std::path::PathBuf::from("styles"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut n = cfg.styles.len() + 1;
        let mut path = dir.join(format!("style_{n}.png"));
        while path.exists() {
            n += 1;
            path = dir.join(format!("style_{n}.png"));
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf)
            .ok_or_else(|| "画面数据不对".to_string())?;
        img.save(&path).map_err(|e| e.to_string())?;
        let st = settings::SavedStyle::capture(cfg, &path.to_string_lossy());
        cfg.styles.push(st);
        self.style_page = (cfg.styles.len() - 1) / 15;
        Ok(())
    }

    /// 系统信息页上面四块实时数据下面「上屏图形组件」—
    /// 每一个开关对应屏幕上的一个圆环勾上就出现在水冷屏上还能调大小和字号
    fn ui_fields(&mut self, ui: &mut egui::Ui, cfg: &mut Settings, info: &InfoSnapshot) {
        let snap = self.snap.lock().map(|s| s.clone()).unwrap_or_default();
        let accent = pal().accent;

        let gap = 14.0;
        // 卡片高度挀两行 + 下面那块组件面板"分配别再吃满整页
        // 否则组件面板会被挤到滚动区外面去
        let avail_h = ui.available_height();
        let card_h = ((avail_h - gap * 2.0 - 190.0) * 0.5).clamp(140.0, 300.0);

        ui.horizontal_top(|ui| {
            // 两列等宽。之前左刀55.2% 是为了塞一CPU 那一行大数字
            // 但两列不一样宽四张卡凑在一起就不成方阵了看着散
            let avail = ui.available_width();
            let col_w = ((avail - gap) * 0.5).max(160.0);
            let (left_w, right_w) = (col_w, col_w);

            // 左列CPU / GPU
            ui.vertical(|ui| {
                ui.set_width(left_w);
                big_card_min(ui, card_h, |ui| {
                    stat_header(ui, "CPU", &info.cpu_name_short(), pal().text);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(fmt_temp(snap.cpu_temp))
                                .size(36.0)
                                .strong()
                                .color(pal().text),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| ring(ui, snap.cpu_usage, accent, 52.0),
                        );
                    });
                    ui.add_space(2.0);
                    metric_row(
                        ui,
                        &[
                            ("使用率", format!("{:.0}%", snap.cpu_usage)),
                            ("时钟", fmt_mhz(snap.cpu_clock)),
                            ("温度源", short_src(&info.temp_source)),
                        ],
                    );
                    ui.add_space(8.0);
                    bar(ui, snap.cpu_usage, accent);
                });
                ui.add_space(gap);
                big_card_min(ui, card_h, |ui| {
                    stat_header(ui, "GPU", &info.gpu_name_short(), pal().text);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(fmt_temp(snap.gpu_temp))
                                .size(36.0)
                                .strong()
                                .color(pal().text),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| ring(ui, snap.gpu_usage, accent, 52.0),
                        );
                    });
                    ui.add_space(2.0);
                    metric_row(
                        ui,
                        &[
                            ("使用率", format!("{:.0}%", snap.gpu_usage)),
                            ("时钟", fmt_mhz(snap.gpu_clock)),
                            ("功耗", format!("{:.0}W", snap.gpu_power)),
                        ],
                    );
                    ui.add_space(8.0);
                    bar(ui, snap.gpu_usage, accent);
                });
            });

            ui.add_space(gap);

            // 右列内孀/ 存储
            ui.vertical(|ui| {
                ui.set_width(right_w);
                big_card_min(ui, card_h, |ui| {
                    stat_header(ui, "内存", "RAM", pal().text);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", snap.ram_usage))
                                .size(36.0)
                                .strong()
                                .color(pal().text),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| ring(ui, snap.ram_usage, accent, 52.0),
                        );
                    });
                    ui.add_space(2.0);
                    metric_row(
                        ui,
                        &[
                            ("已用", format!("{:.1} GB", snap.ram_used_gb)),
                            ("总量", format!("{:.0} GB", snap.ram_total_gb)),
                        ],
                    );
                    ui.add_space(8.0);
                    bar(ui, snap.ram_usage, accent);
                });
                ui.add_space(gap);
                big_card_min(ui, card_h, |ui| {
                    stat_header(ui, "存储", "系统盘", pal().text);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", snap.disk_usage))
                                .size(36.0)
                                .strong()
                                .color(pal().text),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| ring(ui, snap.disk_usage, accent, 52.0),
                        );
                    });
                    ui.add_space(2.0);
                    metric_row(
                        ui,
                        &[
                            ("已用", format!("{:.0} GB", snap.disk_used_gb)),
                            (
                                "可用",
                                format!(
                                    "{:.0} GB",
                                    (snap.disk_total_gb - snap.disk_used_gb).max(0.0)
                                ),
                            ),
                        ],
                    );
                    ui.add_space(8.0);
                    bar(ui, snap.disk_usage, accent);
                });
            });
        });

        // ---------- 上屏图形组件 ----------
        // 用户要的昀图二那种环形表能放到水冷屏上"所以这里不是预览
        // 是真正的上屏列表勾上就画到屏上删掉就从屏上消失
        ui.add_space(gap);
        self.ui_widgets(ui, cfg);
    }

    /// 「上屏图形组件」面板一行一个组件左边开关中间名称右边参数
    ///
    /// 数据源跟「显示列表」同一套`settings::CATALOG`所什CPU 温度
    /// GPU 占用率、风扇转速都能做成环不用另起一份枚举
    fn ui_widgets(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        let accent = pal().accent;
        big_card(ui, |ui| {
            ui.horizontal(|ui| {
                section_title(ui, "上屏图形组件");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("add_widget")
                        .selected_text("＋ 添加环形表")
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for e in settings::CATALOG {
                                if ui.selectable_label(false, e.name).clicked() {
                                    // 新环放在画布中偏下的位置一个挨一个排开
                                    // 免得新加的跟已有的完全重叠、看不见
                                    let n = cfg.widgets.len() as f32;
                                    let x = 60.0 + (n * 62.0) % 200.0;
                                    let y = 60.0 + (n * 40.0) % 140.0;
                                    cfg.widgets.push(settings::Widget::new(e.id, x, y));
                                    self.selected_widget = Some(cfg.widgets.len() - 1);
                                }
                            }
                        });
                });
            });
            ui.label(
                RichText::new("勾上就画到水冷屏上；在「屏幕设备」的预览里可以直接拖位置")
                    .size(10.5)
                    .color(pal().muted),
            );
            ui.add_space(8.0);

            if cfg.widgets.is_empty() {
                ui.label(
                    RichText::new("还没有图形组件。右上角「＋ 添加环形表」加一个试试。")
                        .size(11.5)
                        .color(pal().muted),
                );
                return;
            }

            let mut to_delete: Option<usize> = None;
            let mut rows = Vec::new();
            for (i, w) in cfg.widgets.iter().enumerate() {
                rows.push((i, w.id.clone(), w.visible, w.size, w.thickness, w.text_size));
            }
            for (i, id, mut visible, mut size, mut thickness, mut text_size) in rows {
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut visible, "").changed() {
                        cfg.widgets[i].visible = visible;
                    }
                    let sel = self.selected_widget == Some(i);
                    let name = settings::catalog_name(&id);
                    let text = RichText::new(name)
                        .size(12.0)
                        .color(if sel { accent } else { pal().text });
                    if ui.selectable_label(sel, text).clicked() {
                        self.selected_widget = Some(i);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("×").size(13.0).color(pal().muted))
                                .frame(false))
                            .clicked()
                        {
                            to_delete = Some(i);
                        }
                        // 字号
                        if ui
                            .add(
                                egui::Slider::new(&mut text_size, 8.0..=40.0)
                                    .show_value(false)
                                    .clamping(egui::SliderClamping::Always),
                            )
                            .changed()
                        {
                            cfg.widgets[i].text_size = text_size;
                        }
                        ui.label(RichText::new("字").size(9.5).color(pal().muted));
                        // 环宽
                        if ui
                            .add(
                                egui::Slider::new(&mut thickness, 1.0..=16.0)
                                    .show_value(false)
                                    .clamping(egui::SliderClamping::Always),
                            )
                            .changed()
                        {
                            cfg.widgets[i].thickness = thickness;
                        }
                        ui.label(RichText::new("环").size(9.5).color(pal().muted));
                        // 直径
                        if ui
                            .add(
                                egui::Slider::new(&mut size, 16.0..=200.0)
                                    .show_value(false)
                                    .clamping(egui::SliderClamping::Always),
                            )
                            .changed()
                        {
                            cfg.widgets[i].size = size;
                        }
                        ui.label(RichText::new("径").size(9.5).color(pal().muted));
                    });
                });
                // 选中的那个再展开一排细调坐标 + 颜色
                if self.selected_widget == Some(i) {
                    if let Some(w) = cfg.widgets.get_mut(i) {
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            ui.add(
                                egui::DragValue::new(&mut w.x)
                                    .speed(1.0)
                                    .prefix("X ")
                                    .range(-40.0..=360.0),
                            );
                            ui.add(
                                egui::DragValue::new(&mut w.y)
                                    .speed(1.0)
                                    .prefix("Y ")
                                    .range(-40.0..=280.0),
                            );
                            let c = settings::parse_color(&w.color);
                            let mut rgb = [c.r(), c.g(), c.b()];
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                w.color = settings::format_color(Color32::from_rgb(
                                    rgb[0], rgb[1], rgb[2],
                                ));
                            }
                            ui.checkbox(&mut w.show_text, "显示数字");
                        });
                    }
                }
                ui.add_space(2.0);
            }
            if let Some(i) = to_delete {
                cfg.widgets.remove(i);
                self.selected_widget = if cfg.widgets.is_empty() {
                    None
                } else {
                    Some(self.selected_widget.map_or(0, |s| s.min(cfg.widgets.len() - 1)))
                };
            }
        });
    }

    /// 属性显示页左边「显示列表」哪些字段上屏右边「文字属性」长什么样
    /// 两张卡等高底部留给「CPU 温度取哪一路」
    fn ui_attrs(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        let info = self.info.lock().map(|i| InfoSnapshot::from(&*i)).unwrap_or_default();
        let pair_h = (ui.available_height() - 172.0).clamp(340.0, 520.0);
        ui.columns(2, |cols| {
            // 显示列表
            big_card_min(&mut cols[0], pair_h, |ui| {
                section_title(ui, "显示列表");
                let mut to_delete: Option<usize> = None;
                egui::ScrollArea::vertical()
                    .max_height((pair_h - 122.0).max(150.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, it) in cfg.items.iter_mut().enumerate() {
                            let sel = self.selected == i;
                            let name = settings::catalog_name(&it.id);
                            let label = if it.label.is_empty() {
                                name.to_string()
                            } else {
                                format!("{} · {}", it.label, name)
                            };
                            let (resp, del) = list_row(ui, sel, &label, &mut it.visible);
                            if resp.clicked() {
                                self.selected = i;
                            }
                            if del {
                                to_delete = Some(i);
                            }
                            ui.add_space(2.0);
                        }
                    });
                if let Some(i) = to_delete {
                    if cfg.items.len() > 1 {
                        cfg.items.remove(i);
                        self.selected = self.selected.min(cfg.items.len() - 1);
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("add_field")
                        .selected_text("＋ 添加字段")
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for e in settings::CATALOG {
                                if ui.selectable_label(false, e.name).clicked() {
                                    let y = 6.0 + cfg.items.len() as f32 * 24.0;
                                    cfg.items.push(settings::InfoItem::new(
                                        e.id, "", 8.0, y, 16.0, "#FFFFFFFF", false,
                                    ));
                                    self.selected = cfg.items.len() - 1;
                                }
                            }
                        });
                    if ui.button("保存设置").clicked() {
                        let c = cfg.clone();
                        c.save();
                    }
                });
            });

            // 文字属怀
            big_card_min(&mut cols[1], pair_h, |ui| {
                section_title(ui, "文字属性");
                if self.selected >= cfg.items.len() {
                    self.selected = 0;
                }
                let Some(item) = cfg.items.get_mut(self.selected) else {
                    ui.label(RichText::new("没有字段").color(pal().muted));
                    return;
                };

                label(ui, "字段类型");
                egui::ComboBox::from_id_salt("field_kind")
                    .selected_text(settings::catalog_name(&item.id))
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for e in settings::CATALOG {
                            if ui
                                .selectable_label(item.id == e.id, e.name)
                                .clicked()
                            {
                                item.id = e.id.to_string();
                            }
                        }
                    });

                ui.add_space(4.0);
                label(ui, "前缀文字（留空则不显示）");
                ui.add(egui::TextEdit::singleline(&mut item.label).desired_width(190.0));

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    label(ui, "字号");
                    ui.add(egui::Slider::new(&mut item.font_size, 8.0..=48.0).integer());
                });
                ui.horizontal(|ui| {
                    label(ui, "位置");
                    ui.add(
                        egui::DragValue::new(&mut item.x)
                            .speed(1.0)
                            .prefix("X ")
                            .range(-400.0..=400.0),
                    );
                    ui.add(
                        egui::DragValue::new(&mut item.y)
                            .speed(1.0)
                            .prefix("Y ")
                            .range(-400.0..=400.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut item.bold, "加粗");
                    ui.checkbox(&mut item.show_unit, "显示单位");
                    ui.checkbox(&mut item.show_label, "显示前缀");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut item.outline, "描边");
                    let oc = settings::parse_color(&item.outline_color);
                    let mut orgb = [oc.r(), oc.g(), oc.b()];
                    if ui.color_edit_button_srgb(&mut orgb).changed() {
                        item.outline_color = settings::format_color(Color32::from_rgb(
                            orgb[0], orgb[1], orgb[2],
                        ));
                    }
                    if item.outline {
                        ui.add(
                            egui::Slider::new(&mut item.outline_strength, 0.0..=3.0)
                                .fixed_decimals(1),
                        );
                        ui.add_space(2.0);
                    } else {
                        ui.label(RichText::new("关着的时候不描边").size(10.5).color(pal().muted));
                    }
                });
                ui.horizontal(|ui| {
                    label(ui, "对齐");
                    egui::ComboBox::from_id_salt("align")
                        .selected_text(&item.align)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for a in ["Left", "Center", "Right"] {
                                if ui.selectable_label(item.align == a, a).clicked() {
                                    item.align = a.to_string();
                                }
                            }
                        });
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    label(ui, "文字颜色");
                    let c = settings::parse_color(&item.color);
                    let mut rgb = [c.r(), c.g(), c.b()];
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        item.color = settings::format_color(Color32::from_rgb(
                            rgb[0], rgb[1], rgb[2],
                        ));
                    }
                    ui.label(RichText::new(&item.color).size(10.5).color(pal().muted));
                });
                ui.horizontal(|ui| {
                    label(ui, "底色");
                    let c = settings::parse_color(&item.back_color);
                    let mut rgba = [c.r(), c.g(), c.b(), c.a()];
                    if ui.color_edit_button_srgba_unmultiplied(&mut rgba).changed() {
                        item.back_color = settings::format_color(Color32::from_rgba_unmultiplied(
                            rgba[0], rgba[1], rgba[2], rgba[3],
                        ));
                    }
                    if ui.small_button("透明").clicked() {
                        item.back_color = "#00000000".into();
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ghost_btn(ui, "重置到左下").clicked() {
                        item.x = 8.0;
                        item.y = DESIGN_H as f32 - (item.font_size * 1.6).max(20.0);
                    }
                    if ghost_btn(ui, "归零偏移").clicked() {
                        cfg.offset_x = 0.0;
                        cfg.offset_y = 0.0;
                    }
                });
            });
        });

        ui.add_space(10.0);
        card(ui, |ui| {
            section_title(ui, "CPU 温度取哪一路");
            ui.horizontal(|ui| {
                let current = if cfg.cpu_temp_choice.is_empty() {
                    format!(
                        "自动（现在用：{}）",
                        if info.temp_source.is_empty() {
                            "未知"
                        } else {
                            &info.temp_source
                        }
                    )
                } else {
                    cfg.cpu_temp_choice.clone()
                };
                egui::ComboBox::from_id_salt("temp_src")
                    .selected_text(current)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(cfg.cpu_temp_choice.is_empty(), "自动（挑最像的那个）")
                            .clicked()
                        {
                            cfg.cpu_temp_choice.clear();
                        }
                        for (name, val) in &info.temp_candidates {
                            if ui
                                .selectable_label(
                                    cfg.cpu_temp_choice == *name,
                                    format!("{name}   {val:.1}°C"),
                                )
                                .clicked()
                            {
                                cfg.cpu_temp_choice = name.clone();
                            }
                        }
                    });
            });
            ui.add_space(4.0);
            let suspect = ["DTS", "PECI", "Enhanced", "LHM"]
                .iter()
                .any(|k| info.temp_source.to_lowercase().contains(&k.to_lowercase()));
            if suspect {
                ui.label(
                    RichText::new(
                        "⚠ 这一路在这台机器上实测是死值：压测 45 秒温度不涨，同时 CPU 频率全程 107% 没降。看看就好。",
                    )
                    .size(11.0)
                    .color(pal().warn),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "CPU {}   GPU {}",
                        info.cpu_name, info.gpu_name
                    ))
                    .size(11.0)
                    .color(pal().muted),
                );
            }
        });
    }

    /// 设置页外观主题、背景、出图、启动方式
    /// 卡片宽度和其他页保持一致不单独收窀—四页之间左右边缘对齐整窗才像一个东西
    fn ui_settings(&mut self, ui: &mut egui::Ui, cfg: &mut Settings, info: &InfoSnapshot) {
        // 设置页以前是一长串全宽卡片一路往下排滚起来没完。改成左右两栏
        // 左栏是「我看到的画面」右栏是「程序怎么跑」—两类事分开摆
        let gap = 14.0;
        ui.horizontal_top(|ui| {
            let col_w = ((ui.available_width() - gap) * 0.5).max(300.0);
            ui.vertical(|ui| {
                ui.set_width(col_w);
                self.ui_theme_card(ui, cfg);
                ui.add_space(10.0);
                self.ui_output_card(ui, cfg, info);
                ui.add_space(10.0);
                self.ui_service_card(ui, info);
            });
            ui.add_space(gap);
            ui.vertical(|ui| {
                ui.set_width(col_w);
                self.ui_start_card(ui, cfg);
                ui.add_space(10.0);
                self.ui_sensor_card(ui, cfg, info);
                ui.add_space(10.0);
                self.ui_about(ui, info);
            });
        });
    }

    /// 主题外观只有一排色块色块本身就是按钮
    /// 以前上面一排名字按钮、下面一排色块只昀预览"同一个信息出现两遍
    /// 还让人以为下面那排能点
    fn ui_theme_card(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        card(ui, |ui| {
            section_title(ui, "主题外观");
            ui.horizontal(|ui| {
                for (id, name) in theme::THEME_NAMES {
                    let p = theme::of(id);
                    let sel = cfg.theme == id;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(112.0, 34.0), egui::Sense::click());
                    let rd = CornerRadius::same(8);
                    ui.painter().rect_filled(rect, rd, p.bg);
                    let border = if sel {
                        Stroke::new(2.0, p.accent)
                    } else if resp.hovered() {
                        Stroke::new(1.0, p.accent)
                    } else {
                        Stroke::new(1.0, p.line)
                    };
                    ui.painter()
                        .rect_stroke(rect, rd, border, egui::StrokeKind::Inside);
                    ui.painter().circle_filled(
                        rect.left_center() + egui::vec2(16.0, 0.0),
                        5.5,
                        p.accent,
                    );
                    ui.painter().text(
                        rect.left_center() + egui::vec2(30.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        name,
                        egui::FontId::proportional(12.0),
                        p.text,
                    );
                    if resp.clicked() && cfg.theme != id {
                        cfg.theme = id.to_string();
                        theme::set_current(id);
                        theme::apply(ui.ctx(), id);
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new("点一下整套配色立刻换。百日橘那套会带出左侧的角色立绘。")
                    .size(10.5)
                    .color(pal().muted),
            );
        });
    }

    /// 出图帧率、错误、视频解码那几条
    fn ui_output_card(&mut self, ui: &mut egui::Ui, cfg: &mut Settings, info: &InfoSnapshot) {
        card(ui, |ui| {
            section_title(ui, "出图");
            ui.horizontal(|ui| {
                let running = self.running.load(Ordering::Relaxed);
                if primary_btn(ui, if running { "停止出图" } else { "开始出图" }).clicked() {
                    self.running.store(!running, Ordering::SeqCst);
                }
                ui.add_space(12.0);
                label(ui, "目标帧率");
                ui.add(egui::Slider::new(&mut cfg.target_fps, 1..=30).suffix(" fps"));
            });
            // 原来这里显示"已推 N 帀· 实际 X fps"现在已经去掉
            // 每帧查一欀ACK 要自旋等屏幕服务回写白烧一个核
            if !info.screen_error.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&info.screen_error).size(11.0).color(pal().warn));
            }
            // 视频解码相关的几条也放这块它们是出图链路的一部分
            // 原来贴在背景设置里跀选哪张图"混在一起反而不好找
            if cfg.background_kind == "Video" {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("视频会先整段解码进内存，再把尾巴和开头交叉淡化，所以循环是首尾相接的。")
                        .size(10.5)
                        .color(pal().muted),
                );
                if !info.loop_note.is_empty() {
                    ui.label(
                        RichText::new(format!("当前：{}", info.loop_note))
                            .size(10.5)
                            .color(pal().ok),
                    );
                }
                if !info.video_error.is_empty() {
                    ui.label(
                        RichText::new(&info.video_error)
                            .size(10.5)
                            .color(pal().warn),
                    );
                }
                ui.add(
                    egui::TextEdit::singleline(&mut cfg.ffmpeg_path)
                        .hint_text("ffmpeg.exe 路径（留空自动找）")
                        .desired_width(320.0),
                );
            }
        });
    }

    /// 屏幕服务USBLCD 那个进程的状态与两个急救按钮
    fn ui_service_card(&mut self, ui: &mut egui::Ui, info: &InfoSnapshot) {
        card(ui, |ui| {
            section_title(ui, "屏幕服务");
            ui.label(
                RichText::new(if info.helper_ok {
                    "USBLCD.exe 运行中"
                } else {
                    "USBLCD.exe 没在跑"
                })
                .size(12.0)
                .color(if info.helper_ok { pal().ok } else { pal().warn }),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ghost_btn(ui, "重启屏幕服务").clicked() {
                    sensors::kill_process("USBLCD");
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    let _ = sensors::start_helper();
                }
                if ghost_btn(ui, "关闭 BOOMSTAR").clicked() {
                    let n = sensors::kill_process("BOOMSTAR");
                    if n > 0 {
                        self.log.push(format!("已关闭 {n} 个 BOOMSTAR 进程"));
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("BOOMSTAR 会以 4.6 fps 独占共享内存，开着就会跟这里抢画面。")
                    .size(10.5)
                    .color(pal().muted),
            );
        });
    }

    /// 启动三个开关
    fn ui_start_card(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        card(ui, |ui| {
            section_title(ui, "启动");
            // 开关一律用滑钮样式勾选框在这一排里显得又小又寒酸
            toggle(ui, &mut cfg.start_minimized, "启动后最小化，只留托盘图标");
            ui.add_space(6.0);
            toggle(ui, &mut cfg.auto_kill_boomstar, "启动时关掉 BOOMSTAR");
            ui.add_space(6.0);
            let mut auto = self.auto_start;
            if toggle(ui, &mut auto, "开机自动启动").clicked() {
                let ok = autostart::set(auto);
                self.auto_start = if ok { auto } else { autostart::enabled() };
                self.log.push(if ok {
                    format!("开机自启已{}", if auto { "打开" } else { "关闭" })
                } else {
                    "写注册表失败，可能被安全软件拦了".to_string()
                });
            }
        });
    }

    /// 传感器硬盘扫描开兀+ 提权状态
    fn ui_sensor_card(&mut self, ui: &mut egui::Ui, cfg: &mut Settings, info: &InfoSnapshot) {
        card(ui, |ui| {
            section_title(ui, "传感器");
            toggle(ui, &mut cfg.disk_scan, "扫描硬盘（使用率与温度）");
            ui.add_space(4.0);
            ui.label(
                RichText::new("关掉之后完全不碰硬盘传感器。有些盘一查 SMART 就「咔」一下，嫌吵就关掉。")
                    .size(10.5)
                    .color(pal().muted),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(if info.elevated {
                    "已提权：内核级传感器都读得到"
                } else {
                    "未提权：没关系，HWINFO 会单独以管理员身份拉起，数据照样有"
                })
                .size(10.5)
                .color(if info.elevated { pal().ok } else { pal().warn }),
            );
        });
    }

    /// 背景来源 + 文件 + 铺满 + 纯色底色
    ///
    /// 这几项原来在「设置」页但它们直接改的就是屏上画面本身
    /// 摆在画面旁边才顺所—调一下看一眼不用来回翻页
    fn ui_bg_controls(&mut self, ui: &mut egui::Ui, cfg: &mut Settings) {
        ui.horizontal(|ui| {
            for (kind, text) in [("None", "纯色"), ("Image", "图片"), ("Video", "视频")] {
                let sel = cfg.background_kind == kind;
                if choice_btn(ui, sel, text).clicked() {
                    cfg.background_kind = kind.to_string();
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut cfg.background_path)
                    .hint_text("选择图片或视频文件")
                    .desired_width(ui.available_width() - 118.0),
            );
            if ui.button("浏览…").clicked() {
                let filter = if cfg.background_kind == "Video" {
                    vec!["mp4", "mov", "webm", "mkv", "avi"]
                } else {
                    vec!["png", "jpg", "jpeg", "bmp", "webp"]
                };
                if let Some(p) = rfd_pick(&filter) {
                    cfg.background_path = p;
                }
            }
            if ui.small_button("清除").clicked() {
                cfg.background_path.clear();
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut cfg.background_fill, "铺满屏幕");
            ui.add_space(10.0);
            label(ui, "纯色底色");
            let c = settings::parse_color(&cfg.fallback_color);
            let mut rgb = [c.r(), c.g(), c.b()];
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                cfg.fallback_color =
                    settings::format_color(Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            }
        });
    }

    /// 设置页最底下的「关于」小卡不放 FAQ / 邮箱 / 检查更新这些
    fn ui_about(&mut self, ui: &mut egui::Ui, info: &InfoSnapshot) {
        card(ui, |ui| {
            section_title(ui, "关于");
            ui.label(
                RichText::new(format!("{APP_NAME} {} · Rust", APP_VERSION))
                    .size(13.0)
                    .strong()
                    .color(pal().text),
            );
            ui.label(
                RichText::new(format!("{} · 320×240 图片/视频背景 · 系统信息叠加", APP_SUB))
                    .size(11.0)
                    .color(pal().muted),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "数据源：{}    CPU 温度取：{}",
                    info.source_label,
                    if info.temp_source.is_empty() {
                        "未知"
                    } else {
                        &info.temp_source
                    }
                ))
                .size(11.0)
                .color(pal().muted),
            );
            if !info.video_error.is_empty() {
                ui.label(RichText::new(&info.video_error).size(11.0).color(pal().warn));
            }
        });
        if !self.log.is_empty() {
            ui.add_space(10.0);
            card(ui, |ui| {
                section_title(ui, "日志");
                for l in &self.log {
                    ui.label(RichText::new(l).size(11.0).color(pal().muted));
                }
            });
        }
    }
}

// ---------- 小部什----------

/// 左侧栏的一项导航左边一条竖线标出选中右侧小字写它管什么
fn nav_item(
    ui: &mut egui::Ui,
    cur: &mut Tab,
    target: Tab,
    text: &str,
    hint: &str,
) {
    let selected = *cur == target;
    let h = 46.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), h),
        egui::Sense::click(),
    );
    if selected {
        // 选中态的浅色底左边缘留一点圆角会显得脏干脆画成直角
        ui.painter().rect_filled(
            rect,
            CornerRadius {
                nw: 0,
                sw: 0,
                ne: 10,
                se: 10,
            },
            pal().card,
        );
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, h)),
            CornerRadius::same(0),
            pal().accent,
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(8),
            pal().card_hi.gamma_multiply(0.6),
        );
    }
    let fg = if selected { pal().text } else { pal().muted };
    ui.painter().text(
        rect.left_center() + egui::vec2(22.0, -7.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(13.5),
        fg,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(22.0, 10.0),
        egui::Align2::LEFT_CENTER,
        hint,
        egui::FontId::proportional(9.5),
        if selected { pal().muted } else { pal().muted.gamma_multiply(0.7) },
    );
    if resp.clicked() {
        *cur = target;
    }
}

/// 每套主题配一张立绘返回素材的原始尺寸用来算缩放
fn hero_native(kind: &str) -> (f32, f32) {
    match kind {
        theme::THEME_LIGHT => (477.0, 730.0),
        theme::THEME_DARK => (468.0, 730.0),
        _ => (464.0, 730.0),
    }
}

/// 左侧栏底部的角色立绘。图片编进二进制里运行时不用带文件
///
/// 按主题取不同的图明昼是白发精灵、百日橘是橙发的、黑夜是黑发的
/// 三张都裁成头顶到脚、高度统一 730所以换主题时版面高度不会跳
fn hero(
    ui: &mut egui::Ui,
    cache: &mut std::collections::HashMap<&'static str, egui::TextureHandle>,
    ctx: &egui::Context,
    kind: &str,
) {
    // 主题 id 昀&'static strTHEME_* 常量可以直接当缓存键
    let key: &'static str = if kind == theme::THEME_LIGHT {
        theme::THEME_LIGHT
    } else if kind == theme::THEME_DARK {
        theme::THEME_DARK
    } else {
        theme::THEME_SUNSET
    };
    let bytes: &[u8] = match key {
        theme::THEME_LIGHT => include_bytes!("../assets/hero_light.png"),
        theme::THEME_DARK => include_bytes!("../assets/hero_dark.png"),
        _ => include_bytes!("../assets/hero.png"),
    };
    if !cache.contains_key(key) {
        if let Ok(img) = image::load_from_memory(bytes) {
            let rgba = img.to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            let tex = ctx.load_texture(key.to_string(), ci, egui::TextureOptions::LINEAR);
            cache.insert(key, tex);
        }
    }
    let Some(t) = cache.get(key) else { return };
    let src = t.size_vec2();
    let avail_w = ui.available_width();
    let avail_h = ui.available_height().max(0.0);
    if avail_h < 60.0 {
        return;
    }
    // 等比放进剩下的空间靠底对齐、水平居中
    let scale = (avail_w / src.x).min(avail_h / src.y);
    let size = egui::vec2(src.x * scale, src.y * scale);
    let (slot, _) = ui.allocate_exact_size(egui::vec2(avail_w, size.y), egui::Sense::hover());
    let rect = egui::Rect::from_center_size(slot.center(), size);
    ui.painter().image(
        t.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// 预览矩形里的一个点 ↀ设计画布20×240里的坐标
///
/// 预览显示的是旋转过的画面所以换算里必须帀`rot`否则转迀90° 之后
/// 拖动会朝着垂直方向跑
fn preview_to_design(p: egui::Pos2, img: egui::Rect, rot: u32) -> (f32, f32) {
    let u = (p.x - img.left()) / img.width().max(1.0);
    let v = (p.y - img.top()) / img.height().max(1.0);
    let (tw, th) = if rot % 180 == 90 {
        (DESIGN_H as f32, DESIGN_W as f32)
    } else {
        (DESIGN_W as f32, DESIGN_H as f32)
    };
    let px = u * tw;
    let py = v * th;
    match rot % 360 {
        90 => (py, DESIGN_H as f32 - px),
        180 => (DESIGN_W as f32 - px, DESIGN_H as f32 - py),
        270 => (DESIGN_W as f32 - py, px),
        _ => (px, py),
    }
}

/// 上面那个的逆运算设计画布坐标 ↀ预览矩形里的屏幕坐标
fn design_to_preview(dx: f32, dy: f32, img: egui::Rect, rot: u32) -> egui::Pos2 {
    let (px, py) = match rot % 360 {
        90 => (DESIGN_H as f32 - dy, dx),
        180 => (DESIGN_W as f32 - dx, DESIGN_H as f32 - dy),
        270 => (dy, DESIGN_W as f32 - dx),
        _ => (dx, dy),
    };
    let (tw, th) = if rot % 180 == 90 {
        (DESIGN_H as f32, DESIGN_W as f32)
    } else {
        (DESIGN_W as f32, DESIGN_H as f32)
    };
    egui::pos2(
        img.left() + px / tw * img.width(),
        img.top() + py / th * img.height(),
    )
}

/// 数据卡靀比底亮一桀浮起来不加描边。描边一加就回到模板味了
fn big_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    big_card_min(ui, 0.0, add);
}

/// 同上但给一个最小高庀—同一页的卡片对齐用别让它们各长各的
fn big_card_min(ui: &mut egui::Ui, min_h: f32, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(pal().card)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(18, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if min_h > 0.0 {
                // 减掉上下内边距让卡片外框最终正好是 min_h 髀
                ui.set_min_height((min_h - 32.0).max(0.0));
            }
            add(ui);
        });
}

fn stat_header(ui: &mut egui::Ui, name: &str, sub: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).size(15.0).strong().color(color));
        if !sub.is_empty() {
            ui.label(RichText::new(sub).size(10.0).color(pal().muted));
        }
    });
}

/// 右侧那个圆环中间写百分比
fn ring(ui: &mut egui::Ui, pct: f32, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let c = rect.center();
    let r = size * 0.42;
    let p = ui.painter();
    p.circle_stroke(c, r, Stroke::new(4.0, pal().line));
    let frac = (pct / 100.0).clamp(0.0, 1.0);
    if frac > 0.001 {
        let steps = ((frac * 64.0).ceil() as usize).max(2);
        let start = -std::f32::consts::FRAC_PI_2;
        let mut pts = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let a = start + frac * std::f32::consts::TAU * (i as f32 / steps as f32);
            pts.push(c + egui::vec2(a.cos(), a.sin()) * r);
        }
        p.add(egui::Shape::line(pts, Stroke::new(4.0, color)));
    }
    p.text(
        c,
        egui::Align2::CENTER_CENTER,
        format!("{:.0}%", pct),
        egui::FontId::proportional(11.0),
        pal().text,
    );
}

/// 卡片底部那条横进度条
fn bar(ui: &mut egui::Ui, pct: f32, color: Color32) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 6.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(3), pal().line);
    let frac = (pct / 100.0).clamp(0.0, 1.0);
    if frac > 0.0 {
        let fw = (w * frac).max(3.0);
        let r = egui::Rect::from_min_size(rect.min, egui::vec2(fw, 6.0));
        let rr = CornerRadius::same(if fw >= 6.0 { 3 } else { 2 });
        ui.painter().rect_filled(r, rr, color);
    }
}

/// 卡片里那一行「标筀/ 值」的小格子
fn metric_row(ui: &mut egui::Ui, items: &[(&str, String)]) {
    ui.horizontal(|ui| {
        for (i, (k, v)) in items.iter().enumerate() {
            if i > 0 {
                ui.add_space(18.0);
            }
            ui.vertical(|ui| {
                ui.label(RichText::new(*k).size(9.5).color(pal().muted));
                ui.label(RichText::new(v).size(12.5).strong().color(pal().text));
            });
        }
    });
}

fn fmt_temp(v: f32) -> String {
    if v <= 0.0 {
        "—".to_string()
    } else {
        format!("{:.0}°C", v)
    }
}

fn fmt_mhz(v: f32) -> String {
    if v <= 0.0 {
        "—".to_string()
    } else {
        format!("{:.0} MHz", v)
    }
}

fn short_src(s: &str) -> String {
    if s.is_empty() {
        "—".to_string()
    } else {
        s.chars().take(10).collect()
    }
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(pal().card)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .outer_margin(Margin::same(0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(10.0);
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(13.0).strong().color(pal().text));
    ui.add_space(8.0);
}

fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.5).color(pal().muted));
}

/// 标题条右边那三个按钮形状自己画系统的符号字体在中文环境里不保险
#[derive(Clone, Copy)]
enum WinBtn {
    Min,
    Max,
    Close,
}

fn win_btn(ui: &mut egui::Ui, rect: egui::Rect, kind: WinBtn) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(("winbtn", matches!(kind, WinBtn::Min) as u8
        + 2 * matches!(kind, WinBtn::Max) as u8)), egui::Sense::click());
    let is_close = matches!(kind, WinBtn::Close);
    if resp.hovered() {
        let bg = if is_close {
            // 关闭键用主题里的警示色跟别处的错误色统一
            pal().bad
        } else {
            pal().card_hi
        };
        ui.painter().rect_filled(rect, CornerRadius::same(0), bg);
    }
    let c = rect.center();
    let fg = if resp.hovered() && is_close {
        Color32::WHITE
    } else {
        pal().muted
    };
    let p = ui.painter();
    let st = Stroke::new(1.2, fg);
    match kind {
        WinBtn::Min => {
            p.line_segment(
                [
                    egui::pos2(c.x - 5.0, c.y + 4.0),
                    egui::pos2(c.x + 5.0, c.y + 4.0),
                ],
                st,
            );
        }
        WinBtn::Max => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
                CornerRadius::same(0),
                st,
                egui::StrokeKind::Inside,
            );
        }
        WinBtn::Close => {
            let d = 4.5;
            p.line_segment(
                [
                    egui::pos2(c.x - d, c.y - d),
                    egui::pos2(c.x + d, c.y + d),
                ],
                st,
            );
            p.line_segment(
                [
                    egui::pos2(c.x + d, c.y - d),
                    egui::pos2(c.x - d, c.y + d),
                ],
                st,
            );
        }
    }
    resp
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            26,
        ))
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 90)))
        .corner_radius(CornerRadius::same(20))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).color(color));
        });
}

fn list_row(
    ui: &mut egui::Ui,
    selected: bool,
    text: &str,
    visible: &mut bool,
) -> (egui::Response, bool) {
    let h = 30.0;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    let bg = if selected {
        pal().accent_dim
    } else if resp.hovered() {
        pal().card_hi
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            CornerRadius::same(2),
            pal().accent,
        );
    }
    let color = if *visible { pal().text } else { pal().muted };
    ui.painter().text(
        rect.left_center() + egui::vec2(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(12.0),
        color,
    );

    // 右侧两个小开关显示/隐藏、删陀
    let eye_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 54.0, rect.center().y - 10.0),
        egui::vec2(24.0, 20.0),
    );
    let del_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 28.0, rect.center().y - 10.0),
        egui::vec2(20.0, 20.0),
    );
    let eye = ui.interact(eye_rect, ui.id().with(("eye", text)), egui::Sense::click());
    if eye.clicked() {
        *visible = !*visible;
    }
    ui.painter().text(
        eye_rect.center(),
        egui::Align2::CENTER_CENTER,
        if *visible { "◉" } else { "○" },
        egui::FontId::proportional(12.0),
        if eye.hovered() { pal().accent } else { pal().muted },
    );
    let del = ui.interact(del_rect, ui.id().with(("del", text)), egui::Sense::click());
    ui.painter().text(
        del_rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        egui::FontId::proportional(13.0),
        if del.hovered() { pal().bad } else { pal().muted },
    );
    (resp, del.clicked())
}

fn big_btn(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let size = egui::vec2(46.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let bg = if resp.hovered() { pal().card_hi } else { pal().card };
    let border = if resp.hovered() { pal().accent } else { pal().line };
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(7), Stroke::new(1.0, border), egui::StrokeKind::Inside);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(16.0),
        pal().text,
    );
    resp
}

fn primary_btn(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).size(12.5).color(Color32::WHITE))
        .fill(pal().accent)
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(96.0, 30.0));
    ui.add(b)
}

/// 危险动作的按钮删除这类。平常就是红的hover 再压深一炀—
/// 用主色画"删除"会让人误以为是普通确认这里必须一眼看出是破坏性的
fn danger_btn(ui: &mut egui::Ui, text: &str) -> egui::Response {
    // 自己画egui 皀Button 只有一一fill管不了 hover 态
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(84.0, 30.0), egui::Sense::click());
    let fill = if resp.hovered() {
        pal().bad.gamma_multiply(0.86)
    } else {
        pal().bad
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.5),
        Color32::WHITE,
    );
    resp
}

/// 开关滑钮那种。egui 自带皀checkbox 是个小方框跟这套面板的圆角
/// 和主色都不搭所以自己画一个底槽 + 滑钮开的时候整块换成主色
///
/// 点哪都算不需要正好点到那个小白点 —开关这种东西本来就该整块可点
fn toggle(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    let h = 22.0;
    let track_w = 38.0;
    let knob_r = 8.0;
    let text_w = if text.is_empty() {
        0.0
    } else {
        ui.fonts(|f| {
            f.layout_no_wrap(
                text.to_owned(),
                egui::FontId::proportional(12.0),
                pal().text,
            )
            .size()
            .x
        }) + 10.0
    };
    let total = egui::vec2(track_w + text_w, h);
    let (rect, resp) = ui.allocate_exact_size(total, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
    }

    let track = egui::Rect::from_min_size(
        rect.left_center() - egui::vec2(0.0, h * 0.5),
        egui::vec2(track_w, h),
    );
    let (fill, knob) = if *on {
        (pal().accent, Color32::WHITE)
    } else {
        (pal().line, pal().card)
    };
    ui.painter()
        .rect_filled(track, CornerRadius::same((h * 0.5) as u8), fill);
    let kx = if *on {
        track.right() - knob_r - 3.0
    } else {
        track.left() + knob_r + 3.0
    };
    ui.painter()
        .circle_filled(egui::pos2(kx, track.center().y), knob_r, knob);
    if !text.is_empty() {
        ui.painter().text(
            egui::pos2(track.right() + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(12.0),
            pal().text,
        );
    }
    resp
}

fn ghost_btn(ui: &mut egui::Ui, text: &str) -> egui::Response {    let b = egui::Button::new(RichText::new(text).size(12.0).color(pal().text))
        .fill(pal().card_hi)
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(0.0, 28.0));
    ui.add(b)
}

fn choice_btn(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let (fill, fg, stroke) = if selected {
        (pal().accent_dim, pal().accent, Stroke::new(1.0, pal().accent))
    } else {
        (pal().card_hi, pal().muted, Stroke::new(1.0, pal().line))
    };
    let b = egui::Button::new(RichText::new(text).size(12.0).color(fg))
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(72.0, 28.0));
    ui.add(b)
}

fn tab_btn(ui: &mut egui::Ui, cur: &mut Tab, target: Tab, text: &str) {
    let selected = *cur == target;
    let (fill, fg) = if selected {
        (pal().card_hi, pal().accent)
    } else {
        (Color32::TRANSPARENT, pal().muted)
    };
    let b = egui::Button::new(RichText::new(text).size(12.5).color(fg))
        .fill(fill)
        .stroke(Stroke::new(
            1.0,
            if selected { pal().line } else { Color32::TRANSPARENT },
        ))
        .corner_radius(CornerRadius::same(7))
        .min_size(egui::vec2(84.0, 30.0));
    if ui.add(b).clicked() {
        *cur = target;
    }
}

fn install_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();

    // 数字甀BahnschriftWin10 自带皀DIN 系它是窄体、等宽感强
    // 摆一排读数时不会像雅黑那样忽宽忽窄。没有西文字形可用的中文孀
    // 会自动落到后面的中文字体上所以顺序不能反
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\bahnschrift.ttf") {
        let fd = egui::FontData {
            font: std::borrow::Cow::Owned(bytes),
            index: 0,
            tweak: Default::default(),
        };
        defs.font_data.insert("bahnschrift".to_string(), Arc::new(fd));
        defs.families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "bahnschrift".to_string());
    }

    // 中文用「等线」Deng而不是微软雅黀—这一条是为了修中英文错位
    //
    // epaint 排版时会给每个字形补一一0.5 * (行高 - 该字体行髀 的垂直偏移
    // 想让不同字体在同一行里"居中对齐"。可雅黑的行高是 1.33emBahnschrift
    // 只有 1.01em差 0.32em一半就昀0.16em2 号字上中文被整个往下压亀
    // 5 个像素跟旁边的数字明显不在一条基线上。等线的行高昀1.05em跟
    // Bahnschrift 只差 0.04em偏移剩零点几像素肉眼看不出来
    //
    // 顺带技tweak 皀y_offset_factor 补上邀0.02em让基线严丝合缝
    for (path, name, tweak) in [
        (
            r"C:\Windows\Fonts\Deng.ttf",
            "deng",
            egui::FontTweak {
                y_offset_factor: 0.02,
                ..Default::default()
            },
        ),
        // 等线偶尔缺字生僻字、特殊符号雅黑垫在后面兜底
        (r"C:\Windows\Fonts\msyh.ttc", "msyh", Default::default()),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            let fd = egui::FontData {
                font: std::borrow::Cow::Owned(bytes),
                index: 0,
                tweak,
            };
            defs.font_data.insert(name.to_string(), Arc::new(fd));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                defs.families
                    .entry(fam)
                    .or_default()
                    .push(name.to_string());
            }
        }
    }
    ctx.set_fonts(defs);
}

fn bg_label(cfg: &Settings) -> String {
    if cfg.background_path.is_empty() {
        return String::new();
    }
    let p = std::path::Path::new(&cfg.background_path);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| cfg.background_path.clone());
    let kind = match cfg.background_kind.as_str() {
        "Video" => "视频",
        "Image" => "图片",
        _ => return String::new(),
    };
    format!("{kind} · {name}")
}

fn load_image(path: &str) -> Option<Img> {
    let data = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&data).ok()?;
    let rgba = img.to_rgba8();
    Some(Img {
        w: rgba.width(),
        h: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// 原生文件选择用 Windows 通用对话框的简易实现赀powershell 不可靠这里甀tinyfiledialogs 太重的替代
fn rfd_pick(exts: &[&str]) -> Option<String> {
    // 甀Windows 皀GetOpenFileNameW
    pick_file(exts)
}

fn pick_file(exts: &[&str]) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let filter: String = {
        let mut f = String::new();
        let patterns: Vec<String> = exts.iter().map(|e| format!("*.{e}")).collect();
        f.push_str("媒体文件\0");
        f.push_str(&patterns.join(";"));
        f.push('\0');
        f.push_str("所有文件\0*.*\0\0");
        f
    };
    let filter_wide: Vec<u16> = std::ffi::OsStr::new(&filter).encode_wide().collect();

    let mut buf = vec![0u16; 1024];
    let mut ofn = OPENFILENAMEW::default();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.lpstrFilter = PCWSTR(filter_wide.as_ptr());
    ofn.lpstrFile = windows::core::PWSTR(buf.as_mut_ptr());
    ofn.nMaxFile = buf.len() as u32;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;

    unsafe {
        if GetOpenFileNameW(&mut ofn).as_bool() {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            return Some(String::from_utf16_lossy(&buf[..len]));
        }
    }
    None
}

fn main() -> eframe::Result<()> {
    // 单实例开机自启HKCU 皀Run 项和用户手动双击可能撞在一起
    // 两个实例会互相抢 shareMemory_Image屏上就会来回跳
    // 拿不到命名互斥量就说明已经有一个在跑了 —把它的窗口叫到前台然后退出
    {
        use windows::core::w;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        unsafe {
            let h = CreateMutexW(None, false, w!("CoolerSTAR_SingleInstance"));
            if let Ok(h) = h {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if let Some(existing) = tray::find_main_window_by_pid_of_other() {
                        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                            existing,
                            windows::Win32::UI::WindowsAndMessaging::SW_RESTORE,
                        );
                        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(
                            existing,
                        );
                    }
                    // 句柄故意不关进程退出时系统会回攀
                    let _ = h;
                    return Ok(());
                }
                // HANDLE 没有 Drop不厀CloseHandle 就等于泄漏到进程结束
                // 互斥量一直有效正是我们要的
                let _ = h;
            }
        }
    }

    let title = format!("{APP_NAME} {}", APP_VERSION);

    // 开机自吀/ 「启动后最小化」时窗口一露面就得是看不见的
    //
    // 光靠 with_visible(false) 不够eframe 画完第一帧会自己 set_visible(true)
    // 见宀native/epi_integration.rs 皀post_rendering窗口会在 0.19s 左右
    // 露一下面那时它还没被我们缩起来用户看到的就是一闪而过的黑框
    //
    // 真正拦住它的昀App::new 里的 set_blank_region —窗口可见区域一清空
    // 那一下什么也画不出来。位置照旧恢复上次的不用挪到屏幕外去
    let start_hidden = Settings::load().start_minimized
        || std::env::args().any(|a| {
            a.eq_ignore_ascii_case("-tray") || a.eq_ignore_ascii_case("--tray")
        });

    // 不要系统标题栏那一圈非客户区是深灰的跟这套浅色面板完全不是一个东西
    // 而且会把左栏立绘压掉一截。窗口的拖动、缩放、三个按钮都由界面自己画
    // title_bar / edge_resizeDWM 只负责那一杀1px 外框
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1220.0, 800.0])
        .with_min_inner_size([1000.0, 680.0])
        .with_decorations(false)
        .with_title(title.clone());
    if start_hidden {
        // visible(false) 留着能让第一帧之前就不显示真正兜底的是空区域
        viewport = viewport.with_visible(false);
    }

    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        &title,
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
