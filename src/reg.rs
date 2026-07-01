//! 注册表读写助手（windows-rs，locale 无关，避免解析 reg.exe 本地化输出）。
//! Windows 专用模块。

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, KEY_READ,
    KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
};

pub use windows::Win32::System::Registry::{
    HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
};

/// NUL 结尾宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读取字符串值（默认值传 `value=""`）。键/值不存在或类型不符返回 None。
pub fn read_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
    let sub = wide(subkey);
    let val = wide(value);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_READ, &mut hkey).is_err() {
            return None;
        }
        // 先问字节数。
        let mut cb: u32 = 0;
        let r = RegQueryValueExW(hkey, PCWSTR(val.as_ptr()), None, None, None, Some(&mut cb));
        if r.is_err() || cb == 0 {
            let _ = RegCloseKey(hkey);
            return None;
        }
        let mut buf = vec![0u8; cb as usize];
        let mut cb2 = cb;
        let mut ty = REG_VALUE_TYPE::default();
        let r = RegQueryValueExW(
            hkey,
            PCWSTR(val.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut cb2),
        );
        let _ = RegCloseKey(hkey);
        if r.is_err() {
            return None;
        }
        // 兑现"类型不符返回 None"契约：仅接受字符串类型，避免把 DWORD/二进制按 UTF-16 误读。
        if ty != REG_SZ && ty != REG_EXPAND_SZ {
            return None;
        }
        // 奇数字节数时丢弃末尾半个 u16（n*2 <= cb2，切片必在界内）。
        let n = (cb2 as usize) / 2;
        let u16s: Vec<u16> = buf[..n * 2]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16_lossy(&u16s);
        Some(s.trim_end_matches('\0').to_string())
    }
}

/// 写字符串值（REG_SZ）。打开已存在的 subkey（不创建）；成功返回 true。
pub fn set_string(root: HKEY, subkey: &str, value: &str, data: &str) -> bool {
    let sub = wide(subkey);
    let val = wide(value);
    let data_w = wide(data);
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2) };
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE, &mut hkey).is_err() {
            return false;
        }
        let r = RegSetValueExW(hkey, PCWSTR(val.as_ptr()), None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(hkey);
        r.is_ok()
    }
}

/// 删除值（值不存在也视为成功——幂等清理）。
pub fn delete_value(root: HKEY, subkey: &str, value: &str) -> bool {
    let sub = wide(subkey);
    let val = wide(value);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_SET_VALUE, &mut hkey).is_err() {
            return false;
        }
        let r = RegDeleteValueW(hkey, PCWSTR(val.as_ptr()));
        let _ = RegCloseKey(hkey);
        r.is_ok()
    }
}
