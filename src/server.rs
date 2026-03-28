use anyhow::Result;
use pingora_core::server::Server;
use pingora_core::server::configuration::Opt;
use tracing::info;

use crate::cli::ResolvedConfig;
use crate::proxy::ApiProxy;

/// 启动 Pingora 代理服务器
pub fn start_proxy_server(config: &ResolvedConfig) -> Result<()> {
    // 创建代理服务
    let proxy = ApiProxy::from_config(config)?;
    let (_, port) = config.listen_addr();

    // 创建 Pingora 服务器
    let opt = Opt::default();
    let mut server = Server::new(Some(opt))?;
    server.bootstrap();

    // 创建 HTTP 代理服务
    let mut http_proxy = pingora_proxy::http_proxy_service(&server.configuration, proxy);

    // 添加 TCP 监听
    let listen_addr = &config.listen;
    http_proxy.add_tcp(listen_addr);

    // 注册服务
    server.add_service(http_proxy);

    info!("代理服务器启动成功");
    info!("监听地址: {listen_addr}");

    let base_url_var = if config.use_anthropic_auth() {
        "ANTHROPIC_BASE_URL"
    } else {
        "OPENAI_BASE_URL"
    };
    info!("使用方式: {base_url_var}=http://127.0.0.1:{port} <your-client>");

    // 阻塞运行
    server.run_forever();
}
