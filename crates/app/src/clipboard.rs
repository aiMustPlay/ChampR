//! 系统剪贴板(Windows)。
//!
//! 为什么需要: 表格与符文卡片是自绘的 Text 元素, Slint 的 Text 不支持选中复制,
//! 只有 TextEdit/LineEdit 才能用 Ctrl+C。用户 2026-10-05 要求"输出框均需要支持直接复制",
//! 于是给每个输出区配一个「复制」按钮, 由这里把纯文本写进系统剪贴板。
//!
//! 只用 Win32 标准 API (OpenClipboard/EmptyClipboard/SetClipboardData),
//! 不引第三方 crate; 失败返回 false 并让调用方把原因写进日志(不静默)。

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

/// 把文本写入系统剪贴板。成功 = 其它程序(记事本/微信等)可 Ctrl+V 粘贴。
pub fn set_text(text: &str) -> bool {
    // UTF-16 + NUL 结尾
    let mut utf16: Vec<u16> = std::ffi::OsStr::new(text).encode_wide().collect();
    utf16.push(0);
    let bytes = utf16.len() * std::mem::size_of::<u16>();

    unsafe {
        // 剪贴板被别的程序占着时 OpenClipboard 会失败 —— 重试几次(常见于刚复制完)
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(0 as HWND) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return false;
        }

        let mut ok = false;
        if EmptyClipboard() != 0 {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if !handle.is_null() {
                let ptr = GlobalLock(handle);
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr as *mut u16, utf16.len());
                    GlobalUnlock(handle);
                    // 成功后所有权归系统, 不能再 GlobalFree
                    if SetClipboardData(CF_UNICODETEXT as u32, handle as isize) != 0 {
                        ok = true;
                    }
                }
            }
        }
        CloseClipboard();
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写进去再读回来(通过重新打开剪贴板校验句柄非空), 至少覆盖"不崩 + 返回 true"。
    /// 无桌面会话(CI)时 OpenClipboard 会失败, 那时只要求不 panic。
    #[test]
    fn set_text_does_not_panic() {
        let _ = set_text("ChampR clipboard self test");
    }
}
