//! 无头 UI 预览: 用软件渲染器把窗口画进 PNG, 供开发时肉眼核对布局。
//!
//! 起因(2026-10-03): 主窗"卡片里空一大块 / Tab 文字被裁"这类布局问题,
//! 靠截图往返给用户核实太慢, 而布局余量语义纯靠读代码推不出来。
//! 这个 bin 让布局改动当场可验证:
//!
//! ```text
//! cargo run -p champr --bin ui_preview             # 主窗
//! cargo run -p champr --bin ui_preview -- runes    # 符文窗
//! cargo run -p champr --bin ui_preview -- settings # 设置窗
//! cargo run -p champr --bin ui_preview -- mini     # 迷你窗
//! ```
//! 产物落在 `.cache/ui-preview-<W>x<H>.png` (` .cache/` 已 gitignore)。
//! 注意: 这里同时设了深色 Palette, 与运行时行为保持一致。

slint::include_modules!();

use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, WindowAdapter,
};
use slint::{PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::rc::Rc;

struct PreviewPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "main".to_string());

    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform {
        window: window.clone(),
    }))
    .expect("set preview platform");

    let dark = slint::language::ColorScheme::Dark;

    // 尺寸直接读 Tokens, 与真实窗口尺寸永远一致(不再各写一份硬编码)
    macro_rules! prepare {
        ($ui:expr, $w:ident, $h:ident) => {{
            let ui = $ui;
            let w = ui.global::<Tokens>().$w() as u32;
            let h = ui.global::<Tokens>().$h() as u32;
            window.set_size(PhysicalSize::new(w, h));
            let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
            ui.global::<Palette>().set_color_scheme(dark);
            ui.show().unwrap();
            render(&window, &mut buffer, w, h);
        }};
    }

    match which.as_str() {
        "runes" => {
            // 符文面板现已并入主窗: 预览 = 主窗 + 切到「符文」Tab(output-tab = 0)
            let ui = SourcesWindow::new().unwrap();
            ui.set_build_stamp(slint::SharedString::from("preview"));
            ui.set_output_tab(0);
            ui.set_lcu_status(slint::SharedString::from("connected"));
            ui.set_has_champion(true);
            ui.set_champion_id(74);
            ui.set_champion_name(slint::SharedString::from("大发明家"));
            ui.set_position_label(slint::SharedString::from("辅助"));
            ui.set_roster_my(slint::SharedString::from(
                "我方 1楼·下路·圣枪游侠 | 2楼·辅助·大发明家(我) | 3楼·中单·虚空先知",
            ));
            ui.set_roster_enemy(slint::SharedString::from(
                "敌方 1楼·待分配·不破之誓 | 2楼·待分配·暮光之眼 | 3楼·待分配·武器大师",
            ));
            ui.set_counter_status(slint::SharedString::from("没有该对位的对局样本"));
            ui.set_counter_cand_ids(slint::ModelRc::new(slint::VecModel::from(vec![
                223, 111, 24,
            ])));
            ui.set_counter_cand_names(slint::ModelRc::new(slint::VecModel::from(vec![
                slint::SharedString::from("不破之誓"),
                slint::SharedString::from("暮光之眼"),
                slint::SharedString::from("武器大师"),
            ])));
            ui.set_opponent_header(slint::SharedString::from("对位心理 · 暮光之眼 慎"));
            ui.set_opponent_intel(slint::SharedString::from(
                "他想 1 [- 密切留意队友们, 并且准备好用你的终极技能拯救他们。]\n他想 2 [- 你的能量是可以持续快速恢复的, 所以可以利用这点在与使用法力的英雄们对线时逐渐积累优势。]",
            ));
            ui.set_war_header(slint::SharedString::from("兵法心战 · 战略: 分推压制"));
            ui.set_war_body(slint::SharedString::from("主计: 围魏救赵 | 战术: 趁虚而入"));
            ui.set_rune_compare_header(slint::SharedString::from("符文对比 · 我(奥术彗星) vs 对位(余震)"));
            ui.set_rune_compare_body(slint::SharedString::from(
                "我方: 巫术系消耗型 · 属性碎片 攻速/适应之力/双抗\n对方: 坚决系耐久型 · 基石余震(控制后双抗爆发)\n提醒: 别让他的控制起手命中; 用射程与技能消耗, 等他余震CD(约 15s)再换血。",
            ));
            ui.set_rune_status(slint::SharedString::from("success"));
            ui.set_runes(slint::ModelRc::new(slint::VecModel::from(vec![
                RuneModel {
                    index: 0,
                    name: slint::SharedString::from("[OP.GG] 奥术彗星 · 对线消耗"),
                    position: slint::SharedString::from("辅助"),
                    pick_count: 3120,
                    win_rate: slint::SharedString::from("53.4%"),
                    primary_style_id: 8200,
                    sub_style_id: 8300,
                },
                RuneModel {
                    index: 1,
                    name: slint::SharedString::from("[OP.GG] 召唤艾黎 · 持续消耗"),
                    position: slint::SharedString::from("辅助"),
                    pick_count: 1880,
                    win_rate: slint::SharedString::from("52.1%"),
                    primary_style_id: 8200,
                    sub_style_id: 8400,
                },
            ])));
            prepare!(ui, get_win_main_w, get_win_main_h);
        }
        "mini" => {
            let ui = MiniMatchWindow::new().unwrap();
            ui.set_match_status(slint::SharedString::from("对局中 18:32"));
            ui.set_match_text(slint::SharedString::from(
                "比分 12:9 | 小龙 2:1 | 先锋 1:0\n我 4/1/6 补刀 182\n对位 1/3/2 补刀 145",
            ));
            prepare!(ui, get_win_mini_w, get_win_mini_h);
        }
        "settings" => {
            let ui = TtsSettingsWindow::new().unwrap();
            prepare!(ui, get_win_settings_w, get_win_settings_h);
        }
        _ => {
            let ui = SourcesWindow::new().unwrap();
            ui.set_build_stamp(slint::SharedString::from("preview"));
            ui.set_lcu_status(slint::SharedString::from("connected"));
            ui.set_lcu_summoner(slint::SharedString::from("测试召唤师#1234"));
            ui.set_match_session_status(slint::SharedString::from("对局中 18:32"));
            ui.set_live_match_text(slint::SharedString::from(
                "比分 12:9 | 小龙 2:1 | 先锋 1:0 | 男爵 0:0\n我 4/1/6 补刀 182 金币 8.2k\n对位 1/3/2 补刀 145",
            ));
            prepare!(ui, get_win_main_w, get_win_main_h);
        }
    }
}

/// 软件渲染一帧到 buffer 并落盘
fn render(
    window: &Rc<MinimalSoftwareWindow>,
    buffer: &mut SharedPixelBuffer<Rgb8Pixel>,
    w: u32,
    h: u32,
) {
    window.request_redraw();
    let mut drew = false;
    while window.draw_if_needed(|r| {
        // 回调给的是 &SoftwareRenderer, 直接渲染到像素缓冲
        r.render(buffer.make_mut_slice(), w as usize);
        drew = true;
    }) {}
    if !drew {
        eprintln!("warning: 没有产生重绘(窗口可能未 show)");
    }

    let img = image::RgbImage::from_raw(w, h, buffer.as_bytes().to_vec()).expect("buffer -> image");
    std::fs::create_dir_all(".cache").ok();
    let out = format!(".cache/ui-preview-{w}x{h}.png");
    img.save(&out).expect("save png");
    println!("wrote {out}");
}
