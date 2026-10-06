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
            let row = |cells: Vec<&str>, section: &str, mine_team: bool, mine: bool, opp: bool| TableRow {
                section: slint::SharedString::from(section),
                cells: slint::ModelRc::new(slint::VecModel::from(
                    cells
                        .into_iter()
                        .map(slint::SharedString::from)
                        .collect::<Vec<_>>(),
                )),
                mine_team,
                mine,
                opponent: opp,
            };
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
// 自动禁人/选人(默认开)的预览状态
            ui.set_auto_ban_enabled(true);
            ui.set_auto_pick_enabled(true);
            ui.set_auto_ban_list_text(slint::SharedString::from("暗裔剑魔,影流之主"));
            ui.set_auto_select_status(slint::SharedString::from(
                "自动禁人 开 | 自动选人 开 | 锁定阈值 3s",
            ));
            prepare!(ui, get_win_main_w, get_win_main_h);
        }
        "settings" => {
            let ui = TtsSettingsWindow::new().unwrap();
            prepare!(ui, get_win_settings_w, get_win_settings_h);
        }
        // 选人阶段: 与对局同一个 DataTable, 只是列不同(段位/分路胜率/场次)
        "champselect" => {
            let ui = SourcesWindow::new().unwrap();
            ui.set_build_stamp(slint::SharedString::from("preview"));
            ui.set_lcu_status(slint::SharedString::from("connected"));
            ui.set_lcu_summoner(slint::SharedString::from("测试召唤师#1234"));
            ui.set_match_session_status(slint::SharedString::from("英雄选择中"));
            ui.set_table_summary(slint::SharedString::from("英雄选择中"));
            ui.set_table_sub_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                slint::SharedString::from("ban 我方[亚索,劫]  敌方[盲僧,洛]"),
            ])));
            ui.set_table_columns(preview_unified_columns());
            ui.set_table_rows(slint::ModelRc::new(slint::VecModel::from(vec![
                preview_row(vec![], "我方", true, false, false),
                preview_row(vec!["上", "杰斯", "对手上单#1234", "铂金 III", "53%", "51.2%", "-", "-", "-", "攻187", "1240 场"], "", true, false, false),
                preview_row(vec!["野", "盲僧", "打野爸爸#8888", "钻石 IV", "56%", "52.8%", "-", "-", "-", "法240", "2310 场"], "", true, false, false),
                preview_row(vec!["中", "阿卡丽", "测试召唤师#1234", "黄金 II", "50%", "50.4%", "-", "-", "-", "攻213", "1870 场"], "", true, true, false),
                preview_row(vec!["下", "卡莎", "ADC#6666", "铂金 I", "52%", "51.9%", "-", "-", "-", "法165", "3020 场"], "", true, false, false),
                preview_row(vec!["辅", "洛", "辅助#2333", "白银 I", "54%", "52.1%", "-", "-", "-", "攻98", "980 场"], "", true, false, false),
                preview_row(vec![], "敌方", false, false, false),
                preview_row(vec!["上", "剑魔", "敌方上单#1111", "铂金 II", "49%", "49.6%", "-", "-", "-", "法450", "1120 场"], "", false, false, false),
                preview_row(vec!["野", "豹女", "敌方打野#2222", "钻石 III", "57%", "53.4%", "-", "-", "-", "攻231", "2760 场"], "", false, false, false),
                preview_row(vec!["中", "劫", "敌方中单#3333", "黄金 I", "48%", "48.9%", "-", "-", "-", "攻305", "2050 场"], "", false, false, true),
                preview_row(vec!["下", "厄斐琉斯", "敌方ADC#4444", "铂金 IV", "51%", "51.6%", "-", "-", "-", "法120", "1580 场"], "", false, false, false),
                preview_row(vec!["辅", "牛头", "敌方辅助#5555", "黄金 III", "53%", "52.4%", "-", "-", "-", "攻156", "890 场"], "", false, false, false),
            ])));
            ui.set_table_notes(slint::ModelRc::new(slint::VecModel::from(vec![
                slint::SharedString::from("本机位置 中 | 对位 敌方中 劫"),
                slint::SharedString::from("ban 建议: 亚索(胜率 47.1%)、劫(48.3%)、洛(48.8%)"),
                slint::SharedString::from("对位提示: 你对敌方劫胜率 48.9%(2050 场), 略劣势, 建议先手控线"),
            ])));
            ui.set_table_footnote(slint::SharedString::from(""));
            ui.set_output_tab(1);
            prepare!(ui, get_win_main_w, get_win_main_h);
        }
        _ => {
            let ui = SourcesWindow::new().unwrap();
            ui.set_build_stamp(slint::SharedString::from("preview"));
            ui.set_lcu_status(slint::SharedString::from("connected"));
            ui.set_lcu_summoner(slint::SharedString::from("测试召唤师#1234"));
            ui.set_match_session_status(slint::SharedString::from("对局中 18:32"));
            ui.set_live_match_text(slint::SharedString::from(
                "比分 12:9 | 小龙 2:1 | 先锋 1:0",
            ));


            // 对局中(默认): 与原表格同样的列
            ui.set_table_summary(slint::SharedString::from("对局中 18:32 · 比分 12:9"));
            ui.set_table_sub_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                slint::SharedString::from("我方  龙 火,风 · 巢虫 1 · 先锋 0 · 男爵 0 · 塔 3"),
                slint::SharedString::from("敌方  龙 土 · 巢虫 0 · 先锋 1 · 男爵 0 · 塔 2"),
            ])));
            ui.set_table_columns(preview_unified_columns());
            ui.set_table_rows(slint::ModelRc::new(slint::VecModel::from(vec![
                preview_row(vec![], "我方", true, false, false),
                preview_row(vec!["上", "杰斯", "对手上单#1234", "铂金 III", "53%", "51.2%", "2/3/1", "145", "-", "攻187", "三相之力 · 铁板靴"], "", true, false, false),
                preview_row(vec!["野", "盲僧", "打野爸爸#8888", "钻石 IV", "56%", "52.8%", "4/1/6", "182", "-", "法240", "渴血战斧 · 铁板靴 · 长者之誓"], "", true, false, false),
                preview_row(vec!["中", "阿卡丽", "测试召唤师#1234", "黄金 II", "50%", "50.4%", "8/2/3", "196", "-", "攻213", "暗影阔剑 · 法师之靴"], "", true, true, false),
                preview_row(vec!["下", "卡莎", "ADC#6666", "铂金 I", "52%", "51.9%", "5/4/2", "210", "-", "法165", "无穷之刃 · 狂徒铠甲"], "", true, false, false),
                preview_row(vec!["辅", "洛", "辅助#2333", "白银 I", "54%", "52.1%", "1/5/9", "32", "-", "攻98", "骑士之誓 · 圣物之盾 (阵亡 7s)"], "", true, false, false),
                preview_row(vec![], "敌方", false, false, false),
                preview_row(vec!["上", "剑魔", "敌方上单#1111", "铂金 II", "49%", "49.6%", "3/2/0", "160", "-", "法450", "斯特拉克的挑战护手"], "", false, false, false),
                preview_row(vec!["野", "豹女", "敌方打野#2222", "钻石 III", "57%", "53.4%", "2/4/5", "150", "-", "攻231", "冰霜之牙"], "", false, false, false),
                preview_row(vec!["中", "劫", "敌方中单#3333", "黄金 I", "48%", "48.9%", "6/1/2", "188", "-", "攻305", "幽梦之灵 · 法师之靴"], "", false, false, true),
                preview_row(vec!["下", "厄斐琉斯", "敌方ADC#4444", "铂金 IV", "51%", "51.6%", "4/3/1", "205", "-", "法120", "无尽之刃 · 幻影之舞"], "", false, false, false),
                preview_row(vec!["辅", "牛头", "敌方辅助#5555", "黄金 III", "53%", "52.4%", "0/6/7", "28", "-", "攻156", "骑士之誓 · 山脉之戒"], "", false, false, false),
            ])));
            ui.set_table_notes(slint::ModelRc::new(slint::VecModel::from(vec![
                slint::SharedString::from("我的金币 8420 | 加点 Q5W3E2R1 | 符文 电刑,猛然冲击,眼球收集器,贪欲猎手"),
                slint::SharedString::from("近期击杀 17:42 阿卡丽→劫; 18:05 盲僧→豹女; 18:21 劫→洛"),
            ])));
            ui.set_table_footnote(slint::SharedString::from(
                "我 阿卡丽 闪现/引燃 Lv12 8/2/3 196刀 · 对位 劫 闪现/引燃 Lv12 6/1/2 188刀 · 补刀 +8 · 等级 +0 · 净击杀 +1",
            ));
            ui.set_output_tab(1);
            ui.set_copy_status(slint::SharedString::from("已复制 对局数据 14 行"));
            prepare!(ui, get_win_main_w, get_win_main_h);
        }
    }
}

// ---------------------------------------------------------------------------
//  预览用的表格辅助: 与 Rust 侧 DataTable 一致(列宽/主列标记/行标记)
// ---------------------------------------------------------------------------
fn preview_column(title: &str, width: f32, emphasis: bool) -> TableColumn {
    TableColumn {
        title: slint::SharedString::from(title),
        width,
        emphasis,
    }
}

fn preview_row(
    cells: Vec<&str>,
    section: &str,
    mine_team: bool,
    mine: bool,
    opponent: bool,
) -> TableRow {
    TableRow {
        section: slint::SharedString::from(section),
        cells: slint::ModelRc::new(slint::VecModel::from(
            cells
                .into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        )),
        mine_team,
        mine,
        opponent,
    }
}

/// 统一状态表格的列(选人与对局共用): 与 lcu/advisor.rs::MATCH_COLUMNS 保持一致
fn preview_unified_columns() -> slint::ModelRc<TableColumn> {
    slint::ModelRc::new(slint::VecModel::from(vec![
        preview_column("位", 30.0, false),
        preview_column("英雄", 80.0, true),
        preview_column("召唤师", 86.0, false),
        preview_column("段位", 56.0, false),
        preview_column("个人胜率", 56.0, true),
        preview_column("英雄胜率", 56.0, false),
        preview_column("KDA", 58.0, true),
        preview_column("补刀", 40.0, true),
        preview_column("等级", 32.0, true),
        preview_column("攻/法", 54.0, true),
        preview_column("装备", 73.0, false),
    ]))
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
