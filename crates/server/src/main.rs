use server::{Config, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 启动器把 server 输出重定向到 .cache/server.log; 那里没有终端, 带上 ANSI 颜色码
    // 会让日志变成 [2m[32mINFO[0m 这种噪声(还会出现在失败弹窗里), 所以默认关掉,
    // 想在自己终端看颜色可以设 CHAMPR_SERVER_ANSI=1。
    let ansi = std::env::var("CHAMPR_SERVER_ANSI").is_ok();
    tracing_subscriber::fmt()
        .with_ansi(ansi)
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "server=info,tower_http=info".to_string()),
        )
        .init();

    run(Config::from_env()).await
}
