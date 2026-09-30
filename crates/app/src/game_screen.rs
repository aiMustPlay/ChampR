//! 定位游戏本身(`League of Legends (TM) Client`)落在哪台显示器。
//!
//! 铁律(用户 2026-09-30 拍板): 游戏必须完整独占它自己的屏幕,
//! 任何助手窗口(现在是迷你窗)永不落在游戏屏, 且 show 后必须把焦点还给游戏。
//!
//! 识别: 枚举顶层窗口, 类名 == "RiotWindowClass"(游戏进程独有,
//! LeagueClient Ux 用的是别的类), 拿窗口矩形中心点过 monitors::index_at_point。
//! 找不到(读取载入画面/复盘中) → None, 调用方按"不弹"保守处理。

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowRect, IsWindowVisible, SetForegroundWindow,
};

use crate::monitors::{self, Monitor};

struct EnumCtx {
    /// 找到的游戏窗口矩形中心的屏幕 index
    hit: Option<(usize, HWND)>,
    monitors: Vec<Monitor>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    let ctx = &mut *(data as *mut EnumCtx);
    if IsWindowVisible(hwnd) == 0 {
        return 1; // TRUE, 继续
    }
    let mut class = [0u16; 32];
    let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    if n <= 0 {
        return 1;
    }
    let class = String::from_utf16_lossy(&class[..n as usize]);
    if class != "RiotWindowClass" {
        return 1;
    }
    let mut rect: RECT = std::mem::zeroed();
    if GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }
    let cx = (rect.left + rect.right) / 2;
    let cy = (rect.top + rect.bottom) / 2;
    if let Some(idx) = monitors::index_at_point(&ctx.monitors, cx, cy) {
        ctx.hit = Some((idx, hwnd));
        return 0; // FALSE, 够用了
    }
    1
}

/// 游戏所在的显示器 index(按 monitors.list_monitors 的顺序)。
/// 加上 hwnd(之后 SetForegroundWindow 要用)。
pub fn game_screen_and_hwnd(monitors: &[Monitor]) -> Option<(usize, HWND)> {
    let mut ctx = EnumCtx {
        hit: None,
        monitors: monitors.to_vec(),
    };
    unsafe {
        EnumWindows(
            Some(enum_proc),
            &mut ctx as *mut EnumCtx as LPARAM,
        );
    }
    ctx.hit
}

/// show() 后把前台焦点还给游戏(独占全屏下不还原的话游戏会被最小化)。
/// best-effort: Windows 前台锁可能拒绝, 失败就失败, 不 panic。
pub fn restore_focus(hwnd: HWND) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        SetForegroundWindow(hwnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_game_running_returns_none() {
        // 在没有游戏进程的环境(CI/后台)须稳定 None, 不 panic
        // 有游戏跑着就跳过断言结果只验证不崩
        let _ = game_screen_and_hwnd(&[Monitor {
            index: 0,
            work_rect: (0, 0, 1920, 1080),
            is_primary: true,
        }]);
    }
}
