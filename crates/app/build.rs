fn main() {
    slint_build::compile("ui/app.slint").unwrap();
    // 把应用图标嵌进 exe(任务栏/Alt-Tab/资源管理器显示金色 L)
    let _ = embed_resource::compile("champr.rc", embed_resource::NONE);
}
