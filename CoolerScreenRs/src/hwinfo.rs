//! HWiNFO 共享内存读取（映射名 shareMemory_SysInfo）。
//!
//! 格式（实测）：
//! ```text
//! HWi32_GetNumberOfDetectedSensors:
//! system: maxsun ms-icraft b760m cross
//! ...
//! {"acpi: ...":[{"TZ.TZ00":{"type":"temperature","value":"27.850000"}}],
//!  "cpu [#0]: intel core i5-14600kf":[{"P-core 0 Clock":{"type":"clock","value":"4089.996000"}}, ...]}
//! ```
//! 即：一堆天头行 + 一个 JSON 对象；JSON 的顶层是 `设备名 -> [ {传感器名: {type, value}} ]`。

use std::collections::HashMap;
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    FILE_MAP_READ, PAGE_READWRITE,
};

const MAP_NAME: &str = "shareMemory_SysInfo";
const READ_BYTES: usize = 256 * 1024;
/// HWINFO.exe 要写的整块大小（实测它只用前 30KB 左右，但映射得给足）。
const SECTION_BYTES: usize = 2 * 1024 * 1024;

pub struct Hwinfo {
    map: windows::Win32::Foundation::HANDLE,
    view: Option<windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS>,
    /// 这块 section 是不是我们自己建的（是的话要一直持有，不能关）
    pub created: bool,
    pub devices: HashMap<String, HashMap<String, f32>>,
    pub last_error: String,
    pub ok: bool,
}

impl Drop for Hwinfo {
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

impl Hwinfo {
    pub fn new() -> Self {
        Self {
            map: windows::Win32::Foundation::HANDLE::default(),
            view: None,
            created: false,
            devices: HashMap::new(),
            last_error: String::new(),
            ok: false,
        }
    }

    pub fn open(&mut self) -> bool {
        unsafe {
            // 先试着打开别人（BOOMSTAR / 上一次的我们）已经建好的。
            if let Ok(h) = OpenFileMappingW(FILE_MAP_READ.0, false, w!("shareMemory_SysInfo")) {
                self.map = h;
                let v = MapViewOfFile(h, FILE_MAP_READ, 0, 0, READ_BYTES);
                if v.Value.is_null() {
                    self.last_error = "MapViewOfFile 失败".into();
                    self.ok = false;
                    return false;
                }
                self.view = Some(v);
                self.ok = true;
                self.created = false;
                return true;
            }

            // 没有就自己建一块。HWINFO.exe 只"打开"这块内存，不会创建，
            // 所以没有 BOOMSTAR 的时候必须由我们先把 section 建出来，
            // 它才能往里写数据。
            let h = CreateFileMappingW(
                HANDLE::default(),
                None,
                PAGE_READWRITE,
                (SECTION_BYTES >> 32) as u32,
                (SECTION_BYTES & 0xffff_ffff) as u32,
                w!("shareMemory_SysInfo"),
            );
            let Ok(h) = h else {
                self.last_error = format!("创建 {MAP_NAME} 失败：{:?}", h.err());
                self.ok = false;
                return false;
            };
            let v = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, SECTION_BYTES);
            if v.Value.is_null() {
                let _ = CloseHandle(h);
                self.last_error = "MapViewOfFile（自建）失败".into();
                self.ok = false;
                return false;
            }
            self.map = h;
            self.view = Some(v);
            self.ok = true;
            self.created = true;
            true
        }
    }

    /// 重新读一次。返回是否成功拿到数据。
    pub fn refresh(&mut self) -> bool {
        let Some(view) = self.view else {
            if !self.open() {
                return false;
            }
            return self.refresh();
        };
        unsafe {
            let p = view.Value as *const u8;
            let bytes = std::slice::from_raw_parts(p, READ_BYTES);
            // 别在第一个 0 字节就截断：这块内存开头是 4 字节版本号（0x00000001），
            // 后面跟一串 0 填充，真正的文本从 offset 33 才开始。截到第一个 0 会
            // 得到 1 字节，然后被当成"内容为空"全丢掉。
            let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
            if end < 64 {
                self.last_error = "共享内存内容为空，HWiNFO 可能没在运行".into();
                return false;
            }
            let text = String::from_utf8_lossy(&bytes[..end]);
            match text.find("{\"") {
                Some(idx) => {
                    let tail = &text[idx..];
                    let cut = tail.rfind('}').map(|k| k + 1).unwrap_or(tail.len());
                    match serde_json::from_str::<serde_json::Value>(&tail[..cut]) {
                        Ok(v) => {
                            self.parse(&v);
                            self.ok = true;
                            self.last_error.clear();
                            true
                        }
                        Err(e) => {
                            self.last_error = format!("JSON 解析失败：{e}");
                            false
                        }
                    }
                }
                None => {
                    self.last_error = "共享内存里没有 JSON 段".into();
                    false
                }
            }
        }
    }

    fn parse(&mut self, root: &serde_json::Value) {
        self.devices.clear();
        let Some(obj) = root.as_object() else { return };
        for (dev, list) in obj {
            let Some(arr) = list.as_array() else { continue };
            let mut map: HashMap<String, f32> = HashMap::new();
            for entry in arr {
                let Some(kv) = entry.as_object() else { continue };
                for (sensor, info) in kv {
                    let Some(inner) = info.as_object() else { continue };
                    let val = inner.get("value").and_then(loose_number);
                    if let Some(v) = val {
                        map.insert(sensor.to_lowercase(), v);
                    }
                }
            }
            if !map.is_empty() {
                self.devices.insert(dev.to_lowercase(), map);
            }
        }
    }

    /// 设备名包含 dev_part、传感器名完全等于 sensor（均忽略大小写）。
    pub fn get(&self, dev_part: &str, sensor: &str) -> Option<f32> {
        let sensor = sensor.to_lowercase();
        self.devices
            .iter()
            .filter(|(d, _)| d.contains(dev_part))
            .filter_map(|(_, s)| s.get(&sensor).copied())
            .next()
    }

    /// 设备名包含 dev_part、传感器名包含 sensor_part。
    pub fn get_like(&self, dev_part: &str, sensor_part: &str) -> Option<f32> {
        let part = sensor_part.to_lowercase();
        self.devices
            .iter()
            .filter(|(d, _)| d.contains(dev_part))
            .filter_map(|(_, s)| s.iter().find(|(k, _)| k.contains(&part)).map(|(_, v)| *v))
            .next()
    }

    /// 遍历所有传感器，找设备名包含 dev_part、传感器名包含 sensor_part 的最大值。
    pub fn max_like(&self, dev_part: &str, sensor_part: &str) -> Option<f32> {
        let part = sensor_part.to_lowercase();
        self.devices
            .iter()
            .filter(|(d, _)| d.contains(dev_part))
            .flat_map(|(_, s)| s.iter())
            .filter(|(k, _)| k.contains(&part))
            .map(|(_, v)| *v)
            .fold(None, |acc: Option<f32>, v| Some(acc.map_or(v, |a| a.max(v))))
    }

    /// 列出设备名（原始大小写已丢失，返回小写）。
    pub fn device_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.devices.keys().cloned().collect();
        v.sort();
        v
    }
}

fn loose_number(v: &serde_json::Value) -> Option<f32> {
    if let Some(n) = v.as_f64() {
        return Some(n as f32);
    }
    let s = v.as_str()?;
    let mut out = String::new();
    let mut seen_digit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' {
            out.push(ch);
            if ch.is_ascii_digit() {
                seen_digit = true;
            }
        } else if seen_digit {
            break;
        }
    }
    out.parse::<f32>().ok()
}
