use std::process::Command;

fn main() {
    slint_build::compile("ui/app.slint").unwrap();
    // 把应用图标嵌进 exe(任务栏/Alt-Tab/资源管理器显示金色 L)
    let _ = embed_resource::compile("champr.rc", embed_resource::NONE);

    // 构建戳: 把 git 短 hash 编进二进制, 主窗底部显示。
    // 起因(2026-10-03): 用户两次在提交前几分钟重启, 看到旧界面以为"改了没生效",
    // 排查全靠猜时间线。有了这个戳, 一眼就知道跑的是哪一版。
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CHAMPR_BUILD_HASH={hash}");

    // 工作区有未提交改动时明确标出来(避免拿脏树的产物当"已提交版本")
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if dirty {
        println!("cargo:rustc-env=CHAMPR_BUILD_DIRTY=+");
    } else {
        println!("cargo:rustc-env=CHAMPR_BUILD_DIRTY=");
    }

    // HEAD 变动就重编, 让戳保持真实
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
