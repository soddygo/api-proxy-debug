//! API Proxy Debug Tool
//!
//! 本地 HTTP 反向代理工具，用于拦截和记录 AI 模型 API 的所有请求与响应

mod backend;
mod cli;
mod logger;
mod proxy;
mod server;
mod stats;
mod telemetry;

use std::path::Path;

use clap::Parser;
use cli::{CliArgs, ResolvedConfig};
use telemetry::TelemetryInitializer;
use tracing::info;

fn main() {
    // 解析 CLI 参数
    let args = CliArgs::parse();

    // 合并配置
    let config = match ResolvedConfig::resolve(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("配置错误: {e}");
            std::process::exit(1);
        }
    };

    // 初始化统计模块
    let stats = config.create_stats();

    // 初始化 OpenTelemetry（如果启用）
    // 注意：必须在初始化日志系统之前，这样 OpenTelemetry layer 才能正确集成
    let mut telemetry = TelemetryInitializer::new(config.telemetry.clone());
    if config.telemetry.enabled {
        if let Err(e) = telemetry.init() {
            eprintln!("OpenTelemetry 初始化失败（将禁用）: {e}");
        }
    }

    // 初始化日志系统
    if let Err(e) = logger::init_logging(
        Path::new(&config.log_dir),
        config.log_body(),
        config.log_headers(),
    ) {
        eprintln!("初始化日志系统失败: {e}");
        std::process::exit(1);
    }

    info!("API Proxy Debug Tool 启动中...");

    // 启动代理服务器（server.run_forever 会创建 Tokio runtime）
    if let Err(e) = server::start_proxy_server(&config, stats) {
        eprintln!("启动失败: {e}");
        std::process::exit(1);
    }
    
    // 程序退出时关闭 OpenTelemetry
    telemetry.shutdown();
}
