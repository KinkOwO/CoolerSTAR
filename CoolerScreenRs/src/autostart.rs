//! 开机自启（写 HKCU\Software\Microsoft\Windows\CurrentVersion\Run）

use windows::core::PCWSTR;
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "CoolerSTAR";
/// 改名之前用的值名，顺手清掉，免开机跑两个
const OLD_VALUE_NAME: &str = "CoolerScreenRs";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

pub fn enabled() -> bool {
    let sub = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    unsafe {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let rc: WIN32_ERROR = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        rc.0 == 0 && size > 2
    }
}

pub fn set(on: bool) -> bool {
    let sub = wide(RUN_KEY);
    let name = wide(VALUE_NAME);
    unsafe {
        if !on {
            let mut key = HKEY::default();
            let rc = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            );
            if rc.0 != 0 {
                return false;
            }
            let rc2 = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
            let _ = RegCloseKey(key);
            return rc2.0 == 0 || rc2.0 == 2; // 2 = ERROR_FILE_NOT_FOUND，本来就没有
        }

        let mut key = HKEY::default();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            None,
            &mut key,
            None,
        );
        if rc.0 != 0 {
            return false;
        }
        let cmd = format!("\"{}\" -tray", exe_path());
        let data = wide(&cmd);
        let bytes = unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2)
        };
        let rc2 = RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes));
        // 顺手把改名前的旧值删掉，不然开机会起两个
        let old = wide(OLD_VALUE_NAME);
        let _ = RegDeleteValueW(key, PCWSTR(old.as_ptr()));
        let _ = RegCloseKey(key);
        rc2.0 == 0
    }
}
