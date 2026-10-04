//! 游戏/客户端窗口操作(Win32)。
//!
//! 2026-10-04: 悬浮迷你窗按用户要求移除, 因此"找游戏窗口在哪块屏"那套
//! (game_screen_and_hwnd / restore_focus / monitors::mini_target) 一并删除 ——
//! 对局信息统一在主窗「对局数据」Tab 输出, 不再有任何窗口在游戏旁边出现。
//! 这里只剩托盘菜单要用的"把 LoL 客户端窗口唤到前台"。

use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// 把 LoL 客户端(LeagueClientUx)窗口唤到前台: 已最小化的还原, 然后置前。
/// 客户端类名 "RCLIENT"; 找不到(只开着游戏进程/客户端挂了)返回 false。
/// best-effort: SetForegroundWindow 可能被 Windows 前台锁拒绝。
pub fn activate_lol_client_window() -> bool {
    // "RCLIENT\0" 宽字符
    let class: Vec<u16> = "RCLIENT".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), std::ptr::null());
        if hwnd == 0 {
            return false;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    true
}
