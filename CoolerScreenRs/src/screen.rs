//! 共享内存推帧：映射名 shareMemory_Image。
//!
//! 协议（实测）：
//! - 总大小 250 × 153600 = 38,400,000 字节
//! - slot0 前 4 字节是控制字：`00 00 00 1E` = 准备接（pending）、`00 01 01 1E` = 提交（submit）
//! - 帧数据从偏移 153600 开始，153600 字节 = 240×320×2（RGB565 LE）
//! - 写入后 slot0[2] 会在屏幕服务消化完变回 0；这里**不轮询等待它**，
//!   只在下一帧写入前顺手看一眼，没消化掉就丢帧（见 `send`）。
//!
//! **USBLCD.exe 是「打开已存在的共享区」**，所以必须先有 section 再拉它，否则它启动即崩。

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    PAGE_READWRITE,
};

pub const FRAME_BYTES: usize = 240 * 320 * 2;
pub const TOTAL_BYTES: usize = 250 * FRAME_BYTES;
pub const FRAME_SLOT: usize = FRAME_BYTES;

const CONTROL_PENDING: [u8; 4] = [0x00, 0x00, 0x00, 0x1E];
const CONTROL_SUBMIT: [u8; 4] = [0x00, 0x01, 0x01, 0x1E];

pub struct Screen {
    map: HANDLE,
    view: Option<windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS>,
    pub created: bool,
    pub is_open: bool,
    pub error: String,
    pub sent: u64,
    pub skipped: u64,
}

impl Drop for Screen {
    fn drop(&mut self) {
        unsafe {
            if let Some(v) = self.view.take() {
                let _ = UnmapViewOfFile(v);
            }
            if !self.map.is_invalid() {
                let _ = CloseHandle(self.map);
            }
        }
    }
}

impl Screen {
    pub fn new() -> Self {
        Self {
            map: HANDLE::default(),
            view: None,
            created: false,
            is_open: false,
            error: String::new(),
            sent: 0,
            skipped: 0,
        }
    }

    pub fn open(&mut self) -> bool {
        if self.is_open {
            return true;
        }
        unsafe {
            let mut map = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, w!("shareMemory_Image"));
            if map.is_err() {
                match CreateFileMappingW(
                    HANDLE(-1isize as _),
                    None,
                    PAGE_READWRITE,
                    0,
                    TOTAL_BYTES as u32,
                    w!("shareMemory_Image"),
                ) {
                    Ok(h) => {
                        map = Ok(h);
                        self.created = true;
                    }
                    Err(e) => {
                        self.error = format!("创建共享内存失败：{e}");
                        return false;
                    }
                }
            }
            match map {
                Ok(h) => {
                    self.map = h;
                    let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_BYTES);
                    if v.Value.is_null() {
                        self.error = "MapViewOfFile 失败".into();
                        return false;
                    }
                    self.view = Some(v);
                    self.is_open = true;
                    self.error.clear();
                    true
                }
                Err(e) => {
                    self.error = format!("打开共享内存失败：{e}");
                    false
                }
            }
        }
    }

    pub fn close(&mut self) {
        unsafe {
            if let Some(v) = self.view.take() {
                let _ = UnmapViewOfFile(v);
            }
            if !self.map.is_invalid() {
                let _ = CloseHandle(self.map);
            }
        }
        self.map = HANDLE::default();
        self.is_open = false;
    }

    /// 推一帧。**不等待 ACK**——原来那个轮询会白烧 CPU，
    /// 改成「上一帧还没被屏幕服务消化就先丢掉这一帧」，效果一样但零开销。
    pub fn send(&mut self, frame: &[u8]) -> bool {
        if frame.len() != FRAME_BYTES {
            self.error = format!("帧长度不对：{}", frame.len());
            return false;
        }
        let Some(view) = self.view else {
            self.error = "共享内存还没打开".into();
            return false;
        };
        unsafe {
            let p = view.Value as *mut u8;
            // slot0 第 3 字节非 0 = 上一帧还没被读走，再写会撕裂，直接丢。
            if self.sent > 0 && *p.add(2) != 0 {
                self.skipped += 1;
                return false;
            }
            std::ptr::copy_nonoverlapping(CONTROL_PENDING.as_ptr(), p, 4);
            std::ptr::copy_nonoverlapping(frame.as_ptr(), p.add(FRAME_SLOT), FRAME_BYTES);
            std::ptr::copy_nonoverlapping(CONTROL_SUBMIT.as_ptr(), p, 4);
            self.sent += 1;
            if !self.error.is_empty() {
                self.error.clear();
            }
            true
        }
    }
}
