//! 显示器枚举(Win32 EnumDisplayMonitors)。
//! 用途: 让窗口手动固定在指定显示器上, 避免与游戏主屏抢占。
//! 坐标均为**物理像素**的工作区(已扣任务栏), 与 slint::WindowPosition::Physical 配套。

use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};

/// MONITORINFOF_PRIMARY (windows-sys 0.52 未单独导出该常量)
const MONITORINFOF_PRIMARY: u32 = 1;

/// 单个显示器的工作区信息(物理像素, 不含任务栏区域)。
#[derive(Debug, Clone)]
pub struct Monitor {
    /// 枚举顺序索引(0 起), 跟设置里的下拉项对应。
    pub index: usize,
    /// 工作区 Rect: (left, top, right, bottom)
    pub work_rect: (i32, i32, i32, i32),
    /// 是否主显示器
    pub is_primary: bool,
}

/// 用户可见的下拉标签, 例如 "0 - 主显示器 2560x1440 @(0, 0)".
pub fn label(m: &Monitor) -> String {
    let (l, t, r, b) = m.work_rect;
    let role = if m.is_primary { "主显示器" } else { "副屏" };
    format!("{} - {} {}x{} @({}, {})", m.index, role, r - l, b - t, l, t)
}

struct EnumCtx {
    list: Vec<((i32, i32, i32, i32), bool)>,
}

unsafe extern "system" fn enum_monitor_proc(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let ctx = &mut *(data as *mut EnumCtx);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(hmonitor, &mut info) != 0 {
        let r = info.rcWork;
        ctx.list.push((
            (r.left, r.top, r.right, r.bottom),
            (info.dwFlags & MONITORINFOF_PRIMARY) != 0,
        ));
    }
    1 // TRUE
}

/// 点(物理像素)落在哪一台显示器上。多屏边界重叠时取第一个命中。
pub fn index_at_point(monitors: &[Monitor], x: i32, y: i32) -> Option<usize> {
    monitors.iter().find_map(|m| {
        let (l, t, r, b) = m.work_rect;
        (x >= l && x < r && y >= t && y < b).then_some(m.index)
    })
}

/// 迷你窗目标屏求解(纯函数, 测试覆盖)。
/// 铁律: 游戏必须完整独占它自己的屏, 迷你窗永不落在游戏屏。
/// - 明确知道游戏屏 → 任一其它屏(优先主屏, 再按枚举序)
/// - 游戏屏未知(游戏进程没找到), 多屏 → 保守不弹(None)
/// - 单屏 → 恒 None(迷你窗不存在, 信息只靠 TTS)
pub fn mini_target(monitors: &[Monitor], game_screen: Option<usize>) -> Option<usize> {
    if monitors.len() < 2 {
        return None;
    }
    let game = game_screen?;
    monitors
        .iter()
        .filter(|m| m.index != game)
        .map(|m| (m.index, m.is_primary))
        .max_by_key(|(_, primary)| *primary)
        .map(|(index, _)| index)
}

/// 枚举当前系统上的显示器(索引即枚举顺序, 主显示器通常在前面)。
/// 失败/无显示器时返回空 Vec —— 调用方按无固定处理。
pub fn list_monitors() -> Vec<Monitor> {
    let mut ctx = EnumCtx { list: Vec::new() };
    unsafe {
        let ok = EnumDisplayMonitors(
            0 as HDC,
            std::ptr::null_mut(),
            Some(enum_monitor_proc),
            &mut ctx as *mut EnumCtx as LPARAM,
        );
        if ok == 0 {
            return Vec::new();
        }
    }
    ctx.list
        .into_iter()
        .enumerate()
        .map(|(index, (work_rect, is_primary))| Monitor {
            index,
            work_rect,
            is_primary,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 桌面环境至少应有 1 个显示器; 同时保证 label 不 panic。
    #[test]
    fn enumerates_at_least_one_monitor() {
        let list = list_monitors();
        assert!(!list.is_empty(), "应能枚举到至少一个显示器");
        let first = label(&list[0]);
        assert!(first.contains('0'));
    }

    fn fake(screen: usize, x0: i32) -> Monitor {
        Monitor {
            index: screen,
            work_rect: (x0, 0, x0 + 1920, 1080),
            is_primary: screen == 0,
        }
    }

    #[test]
    fn point_lookup_hits_containing_monitor() {
        let mons = vec![fake(0, 0), fake(1, 1920)];
        assert_eq!(index_at_point(&mons, 100, 100), Some(0));
        assert_eq!(index_at_point(&mons, 2000, 100), Some(1));
        assert_eq!(index_at_point(&mons, -50, 100), None);
    }

    #[test]
    fn mini_never_lands_on_game_screen() {
        let mons = vec![fake(0, 0), fake(1, 1920)];
        // 游戏在 1 号屏 → 迷你只能去 0 号屏
        assert_eq!(mini_target(&mons, Some(1)), Some(0));
        assert_eq!(mini_target(&mons, Some(0)), Some(1));
        // 游戏屏未知 → 不弹
        assert_eq!(mini_target(&mons, None), None);
        // 单屏 → 恒不弹
        assert_eq!(mini_target(&mons[..1], Some(0)), None);
        // 三屏: 游戏在外屏, 优先主屏
        let mons3 = vec![fake(0, 0), fake(1, 1920), fake(2, 3840)];
        assert_eq!(mini_target(&mons3, Some(2)), Some(0));
    }
}
