mod cli;
mod logger;
mod proxy;
mod server;

use clap::Parser;
use cli::{CliArgs, ResolvedConfig};
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() {
    // 解析 CLI 参数
    let args = CliArgs::parse();

    // 初始化日志
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // 合并 CLI 参数和配置文件
    let config = match ResolvedConfig::resolve(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置错误: {e}");
            std::process::exit(1);
        }
    };

    info!("API Proxy Debug Tool 启动中...");
    info!("后端地址: {}", config.backend_url);
    info!(
        "协议: {} | 日志Body: {} | 日志Headers: {}",
        config.api_protocol,
        config.log_body(),
        config.log_headers()
    );
    match &config.log_dir {
        Some(dir) => info!("日志目录: {dir}"),
        None => info!("日志目录: 无 (仅终端输出)"),
    }

    // 启动代理服务器
    if let Err(e) = server::start_proxy_server(&config) {
        eprintln!("启动失败: {e}");
        std::process::exit(1);
    }
}
