//! 系统托盘（直接调 Win32，不引第三方 crate）
//!
//! 起一个独立线程建一个隐藏窗口，挂 NOTIFYICONDATAW，右键弹原生菜单。
//!
//! **平时这个模块不碰主窗口**。托盘线程只把"用户想干什么"记成标志位，由 egui 的
//! `update()` 翻译成 `ViewportCommand`，让 winit 自己去改窗口状态。以前这里
//! 直接 `ShowWindow(SW_MINIMIZE)` 绕过框架，等于两个线程 + 第三方 Dock 三方
//! 抢窗口状态，谁后跑谁赢 —— 在 MyDockFinder 环境下就乱了。
//!
//! 只有一个例外：主窗口被 `ShowWindow(SW_HIDE)` 藏起来之后，Windows 不再给它
//! 发 `WM_PAINT`，winit 也就不再回调 `update()` —— 标志位永远没人读，点托盘
//! 叫不回来。所以那一步必须由托盘线程自己把窗口露出来（见 `wake_main_window`），
//! 露出来之后 `update()` 恢复运行，再按标志位把状态接上。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, EnumWindows, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowTextW, GetWindowThreadProcessId, LoadIconW, PostMessageW, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, GWL_STYLE, HICON, IDI_APPLICATION,
    MF_SEPARATOR, MF_STRING, MSG, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_CAPTION,
    WS_EX_TOOLWINDOW, WS_POPUP, SW_RESTORE, SW_SHOW,
};

const TRAY_CLASS: &str = "CoolerScreenTrayWnd";

/// 找「别的进程」里标题以 CoolerSTAR 开头的主窗口。
/// 单实例检查用：新实例发现已经有一个在跑，就把那个窗口叫出来。
pub fn find_main_window_by_pid_of_other() -> Option<HWND> {
    struct Ctx {
        me: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let c = &mut *(l.0 as *mut Ctx);
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == c.me {
            return windows::core::BOOL(1);
        }
        let style = GetWindowLongPtrW(h, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == 0 {
            return windows::core::BOOL(1);
        }
        // 只要标题像 CoolerSTAR 的，别把别的程序的窗口捞出来
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf);
        if n > 0 {
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            if title.starts_with("CoolerSTAR") {
                c.found = Some(h);
                return windows::core::BOOL(0);
            }
        }
        windows::core::BOOL(1)
    }
    let mut c = Ctx {
        me: unsafe { GetCurrentProcessId() },
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut c as *mut Ctx as isize));
    }
    c.found
}

/// 托盘这边**只管发请求**，不碰窗口。
///
/// 以前这里直接 `ShowWindow(SW_MINIMIZE)` 绕过 winit 改窗口状态，结果要跟
/// eframe 自己的窗口管理、还有 MyDockFinder 这类接管任务栏的软件三方抢
/// 控制权 —— 每帧补一刀最小化，谁后跑谁赢，状态就乱了。
///
/// 现在按普通程序的做法：托盘线程把"用户想干什么"记成标志位，egui 的
/// `update()` 里读标志位，用 `ViewportCommand` 让 winit 自己去改窗口。
/// 只有一个线程动窗口，不存在打架。
const WM_TRAY: u32 = WM_APP + 1;
const CMD_SHOW: usize = 1;
const CMD_TOGGLE: usize = 2;
const CMD_EXIT: usize = 3;

/// 用户点了托盘左键 / 双击，想把窗口叫出来
static SHOW_REQ: AtomicBool = AtomicBool::new(false);
/// 用户点了「开始/停止出图」
static TOGGLE_REQ: AtomicBool = AtomicBool::new(false);
/// 用户点了「退出」
static EXIT_REQ: AtomicBool = AtomicBool::new(false);
/// 允许窗口真的关掉（退出流程用，不然关窗会被当成"收进托盘"）
static ALLOW_CLOSE: AtomicBool = AtomicBool::new(false);

static CTX: Mutex<Option<egui::Context>> = Mutex::new(None);
static RUNNING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

pub fn show_requested() -> bool {
    SHOW_REQ.swap(false, Ordering::SeqCst)
}
pub fn toggle_requested() -> bool {
    TOGGLE_REQ.swap(false, Ordering::SeqCst)
}
pub fn exit_requested() -> bool {
    EXIT_REQ.load(Ordering::SeqCst)
}
pub fn allow_close() -> bool {
    ALLOW_CLOSE.load(Ordering::SeqCst)
}
fn wake() {
    if let Ok(g) = CTX.lock() {
        if let Some(c) = g.as_ref() {
            c.request_repaint();
        }
    }
}

/// 找自己进程里那个主窗口。
///
/// 主窗口被 `SW_HIDE` 之后 winit 收不到 `WM_PAINT`，`update()` 整个停摆，
/// 标志位没人读 —— 所以"叫醒"这一步不能等它，托盘线程自己动手。
fn main_hwnd_own() -> Option<HWND> {
    struct Ctx {
        me: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let c = &mut *(l.0 as *mut Ctx);
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(h, Some(&mut pid));
        if pid != c.me {
            return windows::core::BOOL(1);
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf);
        if n > 0 {
            let title = String::from_utf16_lossy(&buf[..n as usize]);
            if title.starts_with("CoolerSTAR") {
                c.found = Some(h);
                return windows::core::BOOL(0);
            }
        }
        windows::core::BOOL(1)
    }
    let mut c = Ctx {
        me: unsafe { GetCurrentProcessId() },
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut c as *mut Ctx as isize));
    }
    c.found
}

/// 主窗口如果还藏着就把它露出来。
///
/// 这是整个托盘模块里**唯一**直接动主窗口的地方，理由见模块头：藏起来的窗口
/// 拿不到重绘消息，只能从外面撬开。露出来之后 `update()` 立刻恢复，那边会看到
/// `SHOW_REQ` 并把任务栏按钮、预览刷新一并接上。
fn wake_main_window() {
    let Some(h) = main_hwnd_own() else { return };
    unsafe {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = ShowWindow(h, SW_RESTORE);
        force_foreground(h);
    }
    wake();
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 运行时画一个 32×32 图标：渐变圆底 + 一道浪 + 左上高光，不依赖外部资源文件
unsafe fn build_icon() -> Option<HICON> {
    const S: usize = 32;
    let mut xor = vec![0u8; S * S * 4];
    let mut and = vec![0u8; S * S / 8];
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let d = (dx * dx + dy * dy).sqrt();
            if d > 14.2 {
                // AND 位为 1 = 透明
                and[y * (S / 8) + x / 8] |= 1 << (7 - (x % 8));
                continue;
            }
            let t = ((dy + 14.0) / 28.0).clamp(0.0, 1.0);
            let mut r = 0x6e as f32 * (1.0 - t) + 0x1b as f32 * t;
            let mut g = 0xc4 as f32 * (1.0 - t) + 0x52 as f32 * t;
            let mut b = 0xff as f32 * (1.0 - t) + 0xa8 as f32 * t;
            let wave = (x as f32 * 0.30).sin() * 0.5 + 0.5;
            let wy = 20.0 + wave * 4.0;
            if (y as f32 - wy).abs() < 1.6 && x > 5 && x < 27 {
                r = 0xea as f32;
                g = 0xf6 as f32;
                b = 0xff as f32;
            }
            let hl = ((x as f32 - 11.0).powi(2) + (y as f32 - 10.0).powi(2)).sqrt();
            if hl < 4.2 {
                let k = (1.0 - hl / 4.2) * 0.55;
                r += (255.0 - r) * k;
                g += (255.0 - g) * k;
                b += (255.0 - b) * k;
            }
            let i = (y * S + x) * 4;
            xor[i] = b as u8;
            xor[i + 1] = g as u8;
            xor[i + 2] = r as u8;
            xor[i + 3] = 0xff;
        }
    }
    CreateIcon(None, S as i32, S as i32, 1, 32, and.as_ptr(), xor.as_ptr()).ok()
}

/// 把自己顶成前台窗口。
///
/// 直接调 SetForegroundWindow 是不够的：Windows 有前台锁，后台进程调它会被
/// 静默忽略。结果是 TrackPopupMenu 的 owner 不是前台窗口 —— 菜单能弹出来，
/// 但点别处不会消失，得等用户点一下菜单项才收起来。
///
/// 绕过的办法是把本线程的输入队列临时挂到当前前台线程上，这时候系统就
/// 认为我们是"同一个线程"，SetForegroundWindow 才生效。挂完必须摘掉，
/// 不然两个线程的输入会一直纠缠。
unsafe fn force_foreground(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{BringWindowToTop, GetForegroundWindow};

    let fg = GetForegroundWindow();
    let mut fg_pid = 0u32;
    let fg_tid = GetWindowThreadProcessId(fg, Some(&mut fg_pid));
    let my_tid = GetCurrentThreadId();

    if fg_tid != 0 && fg_tid != my_tid {
        let _ = AttachThreadInput(fg_tid, my_tid, true);
        let _ = SetForegroundWindow(hwnd);
        let _ = AttachThreadInput(fg_tid, my_tid, false);
    } else {
        let _ = SetForegroundWindow(hwnd);
    }
    // 少数情况下第一下没生效，补一下
    let _ = BringWindowToTop(hwnd);
}

unsafe fn open_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let running = RUNNING
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|f| f.load(Ordering::Relaxed)))
        .unwrap_or(true);
    let s1 = wide("显示窗口");
    let s2 = wide(if running { "停止出图" } else { "开始出图" });
    let s3 = wide("退出");
    let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, PCWSTR(s1.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, CMD_TOGGLE, PCWSTR(s2.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, PCWSTR(s3.as_ptr()));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    // 标准做法：先把自己顶到前台，菜单才会"点外面自动关"。
    // 这一对 SetForegroundWindow + 事后 PostMessage(WM_NULL) 是 MSDN 的老规矩，
    // 少任何一步都会出现"菜单挂着关不掉"。
    force_foreground(hwnd);
    let cmd = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        None,
        hwnd,
        None,
    )
    .0 as usize;
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    match cmd {
        CMD_SHOW => {
            SHOW_REQ.store(true, Ordering::SeqCst);
            wake_main_window();
        }
        CMD_TOGGLE => TOGGLE_REQ.store(true, Ordering::SeqCst),
        CMD_EXIT => {
            ALLOW_CLOSE.store(true, Ordering::SeqCst);
            EXIT_REQ.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
    wake();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        let ev = (lparam.0 as u32) & 0xffff;
        if ev == WM_LBUTTONDBLCLK || ev == WM_LBUTTONUP {
            SHOW_REQ.store(true, Ordering::SeqCst);
            // 窗口要是还藏着，得先从外面撬开，不然 update() 根本不会跑
            wake_main_window();
            return LRESULT(0);
        }
        if ev == WM_RBUTTONUP {
            open_menu(hwnd);
            return LRESULT(0);
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn spawn(ctx: egui::Context, running: Arc<AtomicBool>) {
    if let Ok(mut g) = CTX.lock() {
        *g = Some(ctx);
    }
    if let Ok(mut g) = RUNNING.lock() {
        *g = Some(running);
    }
    std::thread::spawn(|| unsafe {
        let hinst = GetModuleHandleW(None).ok();
        let icon = build_icon().or_else(|| LoadIconW(None, IDI_APPLICATION).ok());

        let class_name = wide(TRAY_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.map(|h| h.into()).unwrap_or_default(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        // 关键：不能用 HWND_MESSAGE 当父窗口。
        // message-only 窗口永远成不了前台窗口，而 TrackPopupMenu 要求 owner
        // 是前台窗口 —— 否则菜单弹出来点不掉、点别处也不消失。
        // 所以这里建一个普通的隐藏窗口，用 WS_EX_TOOLWINDOW 让它不进任务栏和 Alt+Tab。
        //
        // 风格必须写 WS_POPUP（0x80000000）。以前写的 WS_OVERLAPPED 其实等于 0，
        // 传 0 会被 Windows 当成"用默认风格"，自动补上 WS_CAPTION —— 于是这个
        // 空壳窗口长得跟主窗口一样，被 main_window() 认成主窗口最小化掉了，
        // 任务栏左下角就多出个白框。
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinst.map(|h| h.into()),
            None,
        ) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        if let Some(i) = icon {
            nid.hIcon = i;
        }
        let tip = wide("水冷屏控制台");
        for (k, c) in tip.iter().take(127).enumerate() {
            nid.szTip[k] = *c;
        }
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        let _ = DestroyWindow(hwnd);
    });
}
