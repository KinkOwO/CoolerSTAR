//! 传感器聚合：HWiNFO 为主源，系统 API 兜底。

use crate::hwinfo::Hwinfo;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::core::w;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

#[derive(Default, Clone, Debug)]
pub struct Snapshot {
    pub cpu_temp: f32,
    pub cpu_usage: f32,
    pub cpu_clock: f32,
    pub gpu_temp: f32,
    pub gpu_usage: f32,
    pub gpu_clock: f32,
    pub gpu_power: f32,
    pub fan_rpm: f32,
    pub disk_usage: f32,
    pub disk_temp: f32,
    pub ram_usage: f32,
    /// 内存总量 / 已用（GB），只给启动器界面看，不上屏
    pub ram_total_gb: f32,
    pub ram_used_gb: f32,
    /// 系统盘已用 / 总量（GB）
    pub disk_used_gb: f32,
    pub disk_total_gb: f32,
}

pub fn now_hm() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local = secs as i64 + 8 * 3600; // 东八区
    let day = local % 86400;
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

fn ft_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

pub struct Provider {
    pub hw: Hwinfo,
    pub snap: Snapshot,
    pub source: String,
    pub cpu_name: String,
    pub gpu_name: String,
    /// 自动挑选出来的那一路 CPU 温度源名字
    pub temp_source: String,
    pub temp_choice: String,
    /// 关掉之后连系统 API 都不问，硬盘相关的一律留空。
    /// 有些机器上一查 SMART 硬盘就「咔」一下，能关掉比什么都强。
    pub disk_scan: bool,
    pub temp_candidates: Vec<(String, f32)>,
    pub last_cpu_time: Option<(u64, u64, u64)>,
    pub last_poll: Option<Instant>,
    pub disks: Vec<String>,
    pub hw_ok: bool,
    pub error: String,
}

impl Provider {
    pub fn new() -> Self {
        let mut hw = Hwinfo::new();
        hw.open();
        Self {
            hw,
            snap: Snapshot::default(),
            source: "未连接".into(),
            cpu_name: "—".into(),
            gpu_name: "—".into(),
            temp_source: String::new(),
            temp_choice: String::new(),
            disk_scan: true,
            temp_candidates: Vec::new(),
            last_cpu_time: None,
            last_poll: None,
            disks: Vec::new(),
            hw_ok: false,
            error: String::new(),
        }
    }

    pub fn poll(&mut self) {
        let mut s = Snapshot::default();
        self.hw_ok = self.hw.refresh();
        if self.hw_ok {
            self.source = "HWiNFO 共享内存".into();
            self.error.clear();
            self.read_hwinfo(&mut s);
        } else {
            self.source = "系统 API（HWiNFO 不可用）".into();
            self.error = self.hw.last_error.clone();
        }
        if s.cpu_usage <= 0.0 {
            s.cpu_usage = self.sys_cpu_usage();
        } else {
            let _ = self.sys_cpu_usage(); // 保持差分基准连续
        }
        if s.ram_usage <= 0.0 {
            s.ram_usage = self.sys_ram_usage();
        }
        if self.disk_scan && s.disk_usage <= 0.0 {
            s.disk_usage = self.sys_disk_usage();
        }
        // 这两组容量不进 HWiNFO，直接问系统，反正一次调用都是微秒级
        let (ram_total, ram_used) = self.sys_ram_sizes();
        s.ram_total_gb = ram_total;
        s.ram_used_gb = ram_used;
        if self.disk_scan {
            let (disk_total, disk_used, disk_pct) = self.sys_disk_sizes();
            s.disk_total_gb = disk_total;
            s.disk_used_gb = disk_used;
            if s.disk_usage <= 0.0 {
                s.disk_usage = disk_pct;
            }
        }
        if s.cpu_clock <= 0.0 {
            s.cpu_clock = self.hw.get_like("cpu [#0]", "p-core 0 clock").unwrap_or(0.0);
        }
        self.snap = s;
        self.last_poll = Some(Instant::now());
        self.learn_names();
    }

    fn read_hwinfo(&mut self, s: &mut Snapshot) {
        // CPU
        if let Some(v) = self.hw.get_like("cpu [#0]", "total cpu usage") {
            s.cpu_usage = v;
        }
        for name in ["p-core 0 clock", "core 0 clock", "p-core 0 ratio"] {
            if let Some(v) = self.hw.get_like("cpu [#0]", name) {
                if name.ends_with("ratio") {
                    s.cpu_clock = v * 100.0;
                } else {
                    s.cpu_clock = v;
                }
                break;
            }
        }

        // CPU 温度候选
        let mut cands: Vec<(String, f32)> = Vec::new();
        let snap_devices: Vec<(String, Vec<(String, f32)>)> = self
            .hw
            .devices
            .iter()
            .map(|(d, m)| (d.clone(), m.iter().map(|(k, v)| (k.clone(), *v)).collect()))
            .collect();
        for (dev, sensors) in &snap_devices {
            let tag = if dev.contains("nuvoton") {
                "SuperIO"
            } else if dev.contains("enhanced") {
                "Enhanced"
            } else if dev.contains("dts") {
                "DTS"
            } else if dev.contains("acpi") {
                "ACPI"
            } else if dev.contains("cpu [#0]") {
                "CPU"
            } else {
                continue;
            };
            for (key, val) in sensors {
                let label = match (tag, key.as_str()) {
                    ("DTS", "cpu package") => Some("DTS CPU Package".to_string()),
                    ("DTS", "core max") => Some("DTS Core Max".to_string()),
                    ("Enhanced", "cpu package") => Some("Enhanced CPU Package".to_string()),
                    ("SuperIO", "cpu") => Some("SuperIO CPU".to_string()),
                    ("SuperIO", "cpu (peci)") => Some("SuperIO PECI".to_string()),
                    ("ACPI", k) if k.starts_with("tz") => Some(format!("ACPI {}", upper_first(k))),
                    _ => None,
                };
                if let Some(l) = label {
                    if *val > 0.0 {
                        cands.push((l, *val));
                    }
                }
            }
        }
        cands.sort_by(|a, b| a.0.cmp(&b.0));
        cands.dedup_by(|a, b| a.0 == b.0);
        self.temp_candidates = cands.clone();

        let picked = if !self.temp_choice.is_empty() {
            cands.iter().find(|(n, _)| n == &self.temp_choice).cloned()
        } else {
            let exact = ["superio cpu", "superio system"];
            let order = ["superio cpu", "superio system", "cpu package", "core max", "cpu"];
            let mut found = None;
            for want in exact {
                if let Some(c) = cands.iter().find(|(n, _)| n.to_lowercase() == want) {
                    found = Some(c.clone());
                    break;
                }
            }
            if found.is_none() {
                for want in order {
                    if let Some(c) = cands.iter().find(|(n, _)| n.to_lowercase().contains(want)) {
                        found = Some(c.clone());
                        break;
                    }
                }
            }
            found
        };
        if let Some((name, val)) = picked {
            s.cpu_temp = val;
            self.temp_source = name;
        } else if let Some(c) = cands.first() {
            s.cpu_temp = c.1;
            self.temp_source = c.0.clone();
        } else {
            self.temp_source.clear();
        }

        // GPU
        if let Some(v) = self.hw.get_like("gpu [#0]", "gpu temperature") {
            s.gpu_temp = v;
        }
        for name in ["gpu core load", "gpu utilization", "gpu d3d usage"] {
            if let Some(v) = self.hw.get_like("gpu [#0]", name) {
                s.gpu_usage = v;
                break;
            }
        }
        if let Some(v) = self.hw.get_like("gpu [#0]", "gpu clock") {
            s.gpu_clock = v;
        }
        if let Some(v) = self.hw.get_like("gpu [#0]", "gpu power") {
            s.gpu_power = v;
        } else {
            s.gpu_power = self.hw.get_like("gpu [#0]", "board power").unwrap_or(0.0);
        }

        // 风扇：nuvoton 里名字含 FAN 的，优先 CPU 那一路
        let mut best_cpu: Option<f32> = None;
        let mut best_any: Option<f32> = None;
        for (dev, sensors) in &snap_devices {
            if !dev.contains("nuvoton") {
                continue;
            }
            for (k, v) in sensors {
                if !k.contains("fan") || *v <= 0.0 {
                    continue;
                }
                if k.contains("cpu") && best_cpu.map_or(true, |b| *v > b) {
                    best_cpu = Some(*v);
                }
                if best_any.map_or(true, |b| *v > b) {
                    best_any = Some(*v);
                }
            }
        }
        s.fan_rpm = best_cpu.or(best_any).unwrap_or(0.0);

        // 内存
        if let Some(v) = self.hw.get_like("system", "physical memory load") {
            s.ram_usage = v;
        }
        // 硬盘温度（关掉磁盘扫描就连提都不提，免得上层再拿着旧值算）
        if self.disk_scan {
            s.disk_temp = self.hw.max_like("s.m.a.r.t.", "drive temperature").unwrap_or(0.0);
        }
    }

    fn learn_names(&mut self) {
        for dev in self.hw.devices.keys() {
            if dev.starts_with("cpu [#0]:") && self.cpu_name == "—" {
                if let Some(rest) = dev.split(':').nth(1) {
                    self.cpu_name = rest.trim().to_uppercase();
                }
            }
            if dev.starts_with("gpu [#0]:") && self.gpu_name == "—" {
                if let Some(rest) = dev.split(':').nth(1) {
                    self.gpu_name = rest.trim().to_uppercase();
                }
            }
        }
        if self.cpu_name == "—" {
            std::env::var("PROCESSOR_IDENTIFIER")
                .ok()
                .inspect(|v| self.cpu_name = v.clone());
        }
        self.disks = self
            .hw
            .devices
            .keys()
            .filter(|d| d.starts_with("s.m.a.r.t."))
            .map(|d| {
                d.split('[')
                    .last()
                    .map(|x| x.trim_end_matches(']').to_string())
                    .unwrap_or_default()
            })
            .collect();
    }

    fn sys_cpu_usage(&mut self) -> f32 {
        unsafe {
            let mut idle = FILETIME::default();
            let mut kern = FILETIME::default();
            let mut user = FILETIME::default();
            if GetSystemTimes(Some(&mut idle), Some(&mut kern), Some(&mut user)).is_err() {
                return 0.0;
            }
            let i = ft_to_u64(idle);
            let k = ft_to_u64(kern);
            let u = ft_to_u64(user);
            let total = k + u;
            if let Some((li, lt, _)) = self.last_cpu_time {
                let dt = total.saturating_sub(lt) as f64;
                let di = i.saturating_sub(li) as f64;
                self.last_cpu_time = Some((i, total, total));
                if dt > 0.0 {
                    return (((dt - di) / dt) * 100.0).clamp(0.0, 100.0) as f32;
                }
            } else {
                self.last_cpu_time = Some((i, total, total));
            }
            0.0
        }
    }

    fn sys_ram_usage(&self) -> f32 {
        unsafe {
            let mut ms = MEMORYSTATUSEX::default();
            ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                ms.dwMemoryLoad as f32
            } else {
                0.0
            }
        }
    }

    /// 内存总量 / 已用（GB）。GlobalMemoryStatusEx 给的是字节数。
    fn sys_ram_sizes(&self) -> (f32, f32) {
        unsafe {
            let mut ms = MEMORYSTATUSEX::default();
            ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                const GB: f64 = 1024.0 * 1024.0 * 1024.0;
                let total = (ms.ullTotalPhys as f64 / GB) as f32;
                let used = ((ms.ullTotalPhys - ms.ullAvailPhys) as f64 / GB) as f32;
                (total, used)
            } else {
                (0.0, 0.0)
            }
        }
    }

    fn sys_disk_usage(&self) -> f32 {
        self.sys_disk_sizes().2
    }

    /// 系统盘 (总量 GB, 已用 GB, 占用率 %)
    fn sys_disk_sizes(&self) -> (f32, f32, f32) {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let mut root = drive.clone();
        root.push('\\');
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let mut free = 0u64;
            let mut total = 0u64;
            let mut avail = 0u64;
            if GetDiskFreeSpaceExW(
                windows::core::PCWSTR(wide.as_ptr()),
                Some(&mut avail),
                Some(&mut total),
                Some(&mut free),
            )
            .is_ok()
                && total > 0
            {
                const GB: f64 = 1024.0 * 1024.0 * 1024.0;
                let total_gb = (total as f64 / GB) as f32;
                let used_gb = ((total - avail) as f64 / GB) as f32;
                let pct = ((total - avail) as f64 / total as f64 * 100.0) as f32;
                return (total_gb, used_gb, pct);
            }
        }
        (0.0, 0.0, 0.0)
    }
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// 进程检查（USBLCD 是否在跑）
pub fn process_running(name_part: &str) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut pe = PROCESSENTRY32W::default();
        pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let want = name_part.to_lowercase();
        let mut found = false;
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let end = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let n = String::from_utf16_lossy(&pe.szExeFile[..end]).to_lowercase();
                if n.contains(&want) {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

pub fn kill_process(name_part: &str) -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    let mut killed = 0;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return 0;
        };
        let mut pe = PROCESSENTRY32W::default();
        pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let want = name_part.to_lowercase();
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let end = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let n = String::from_utf16_lossy(&pe.szExeFile[..end]).to_lowercase();
                if n.contains(&want) {
                    if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pe.th32ProcessID) {
                        if TerminateProcess(h, 1).is_ok() {
                            killed += 1;
                        }
                        let _ = CloseHandle(h);
                    }
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    killed
}

/// 放 HWINFO.exe / USBLCD.exe 的目录。
///
/// 优先用程序自己旁边的 `bin` —— 这样把 `发布` 整个文件夹拷走就能用，
/// 不用装 BOOMSTAR、也不用碰注册表。找不到再退回 BOOMSTAR 的安装目录。
pub fn helper_dir() -> Option<String> {
    // 1. 便携模式：<exe 所在目录>\bin
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let local = dir.join("bin");
            if local.join("USBLCD.exe").exists() {
                return Some(local.display().to_string());
            }
            // 也有人会把两个 exe 直接跟主程序放一起
            if dir.join("USBLCD.exe").exists() {
                return Some(dir.display().to_string());
            }
        }
    }
    // 2. 退回 BOOMSTAR 安装目录
    boomstar_dir()
}

/// BOOMSTAR 的安装目录。注册表里写着实际位置（这台机器在 D 盘），
/// 读不到再按常见路径挨个试。
pub fn boomstar_dir() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };
    unsafe {
        for sub in [
            w!("SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\BOOMSTAR_is1"),
            w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\BOOMSTAR_is1"),
        ] {
            let mut buf = [0u16; 512];
            let mut len = (buf.len() * 2) as u32;
            let rc = RegGetValueW(
                HKEY_LOCAL_MACHINE,
                sub,
                w!("InstallLocation"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut len),
            );
            if rc.is_ok() {
                let n = (len as usize / 2).saturating_sub(1);
                let s = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
                let s = s.trim().trim_end_matches('\\').to_string();
                if !s.is_empty() && std::path::Path::new(&s).is_dir() {
                    return Some(s);
                }
            }
        }
    }
    // 注册表里没有就扫一遍常见位置
    let mut cands = vec![
        r"C:\Program Files (x86)\BOOMSTARCAP".to_string(),
        r"C:\Program Files\BOOMSTARCAP".to_string(),
    ];
    for drive in ["C", "D", "E", "F"] {
        cands.push(format!(r"{drive}:\Program Files (x86)\BOOMSTARCAP"));
        cands.push(format!(r"{drive}:\Program Files\BOOMSTARCAP"));
    }
    cands.into_iter().find(|p| std::path::Path::new(p).is_dir())
}

pub fn start_helper() -> Result<(), String> {
    let base = helper_dir().ok_or_else(|| "找不到 USBLCD.exe".to_string())?;
    for exe in ["USBLCD.exe", "USBLCDNEW.exe"] {
        let path = format!("{base}\\{exe}");
        if std::path::Path::new(&path).exists() {
            let mut cmd = std::process::Command::new(&path);
            cmd.current_dir(&base);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            return match cmd.spawn() {
                Ok(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    Ok(())
                }
                Err(e) => Err(format!("启动 {exe} 失败：{e}")),
            };
        }
    }
    Err(format!("{base} 里没有 USBLCD.exe"))
}

/// 拉起 HWINFO.exe（HWiNFO 的数据采集壳）。它必须以管理员身份跑，
/// 否则 `HWi32_Init` 返回 5（ACCESS_DENIED）然后立刻退出。
///
/// 注意：它只「打开」已存在的 `shareMemory_SysInfo`，不会自己创建。
/// 调用前请先确保 `Hwinfo::open()` 已经把 section 建好了。
pub fn start_hwinfo() -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let base = helper_dir().ok_or_else(|| "找不到 HWINFO.exe".to_string())?;
    let exe = format!("{base}\\HWINFO.exe");
    if !std::path::Path::new(&exe).exists() {
        return Err(format!("{exe} 不存在"));
    }

    let verb = w!("runas");
    let file: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
    let dir: Vec<u16> = base.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let r = ShellExecuteW(
            None,
            verb,
            windows::core::PCWSTR(file.as_ptr()),
            None,
            windows::core::PCWSTR(dir.as_ptr()),
            // HWINFO.exe 是控制台程序，正常启动会顶出一个黑框。
            // SW_HIDE 让它带着隐藏的控制台起来，功能不受影响。
            SW_HIDE,
        );
        // ShellExecuteW 返回值 > 32 才算成功
        if r.0 as isize > 32 {
            Ok(())
        } else {
            Err(format!("拉起 HWINFO.exe 失败（ShellExecute 返回 {}）", r.0 as isize))
        }
    }
}

/// HWINFO.exe 在不在跑
pub fn hwinfo_running() -> bool {
    process_running("HWINFO")
}

pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

pub fn find_ffmpeg() -> Option<String> {
    if let Ok(p) = std::env::var("FFMPEG_PATH") {
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    // 便携模式：<exe 目录>\bin\ffmpeg.exe
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for sub in ["bin", "."] {
                let p = dir.join(sub).join("ffmpeg.exe");
                if p.exists() {
                    return Some(p.display().to_string());
                }
            }
        }
    }
    let candidates = [
        r"C:\Users\Tomeu\AppData\Local\Microsoft\WinGet\Packages\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg-9.0.2-full_build\bin\ffmpeg.exe",
        r"C:\ffmpeg\bin\ffmpeg.exe",
    ];
    for c in candidates {
        if std::path::Path::new(c).exists() {
            return Some(c.to_string());
        }
    }
    // PATH 里找
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let p = format!("{}\\ffmpeg.exe", dir.trim_end_matches('\\'));
            if std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
    }
    let _ = w!("");
    None
}
