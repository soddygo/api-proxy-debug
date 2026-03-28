use std::path::Path;

use clap::Parser;
use serde::Deserialize;

/// JSON 配置文件结构
#[derive(Deserialize, Debug, Default)]
pub struct ConfigFile {
    pub listen: Option<String>,
    pub backend_url: Option<String>,
    pub api_key: Option<String>,
    pub api_protocol: Option<String>,
    pub no_log_body: Option<bool>,
    pub no_log_headers: Option<bool>,
    pub log_dir: Option<String>,
}

impl ConfigFile {
    /// 从 JSON 文件加载配置
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        // 检查配置文件是否存在
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "配置文件 '{}' 不存在\n\n请先创建配置文件，例如:\n  cp config.example.json {}\n\n或者使用命令行参数直接指定配置:\n  cargo run -- --backend-url https://api.example.com --api-key your-key",
                path.display(),
                path.display()
            ));
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置文件 {} 失败: {e}", path.display()))?;
        let config: Self = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("解析配置文件 {} 失败: {e}", path.display()))?;
        Ok(config)
    }
}

/// API 代理调试工具 - 拦截并记录 AI 模型 API 调用
#[derive(Parser, Debug, Clone)]
#[command(name = "api-proxy-debug")]
#[command(about = "A local proxy for intercepting and logging AI model API requests/responses")]
pub struct CliArgs {
    /// JSON 配置文件路径
    #[arg(short, long)]
    pub config: Option<String>,

    /// 代理监听地址
    #[arg(short, long)]
    pub listen: Option<String>,

    /// 后端 API 地址 (例如: https://api.anthropic.com)
    #[arg(short, long)]
    pub backend_url: Option<String>,

    /// 注入到请求中的 API Key
    #[arg(short = 'k', long)]
    pub api_key: Option<String>,

    /// API 协议类型: anthropic 或 openai
    #[arg(short = 'p', long)]
    pub api_protocol: Option<String>,

    /// 关闭请求/响应 body 日志
    #[arg(long, default_value_t = false)]
    pub no_log_body: bool,

    /// 关闭请求/响应 headers 日志
    #[arg(long, default_value_t = false)]
    pub no_log_headers: bool,

    /// 日志输出目录 (不指定则仅输出到终端)
    #[arg(long)]
    pub log_dir: Option<String>,
}

/// 合并后的最终配置 (所有字段必填)
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub listen: String,
    pub backend_url: String,
    pub api_key: String,
    pub api_protocol: String,
    pub no_log_body: bool,
    pub no_log_headers: bool,
    pub log_dir: Option<String>,
}

impl ResolvedConfig {
    /// 从 CLI 参数和配置文件合并，CLI 优先
    pub fn resolve(args: &CliArgs) -> anyhow::Result<Self> {
        let file_config = match &args.config {
            Some(path) => ConfigFile::load(Path::new(path))?,
            None => ConfigFile::default(),
        };

        let backend_url = args
            .backend_url
            .clone()
            .or(file_config.backend_url)
            .ok_or_else(|| {
                anyhow::anyhow!("缺少 backend_url，请通过 --backend-url 或配置文件指定")
            })?;

        let api_key = args
            .api_key
            .clone()
            .or(file_config.api_key)
            .ok_or_else(|| anyhow::anyhow!("缺少 api_key，请通过 --api-key 或配置文件指定"))?;

        let listen = args
            .listen
            .clone()
            .or(file_config.listen)
            .unwrap_or_else(|| "0.0.0.0:8080".to_string());

        let api_protocol = args
            .api_protocol
            .clone()
            .or(file_config.api_protocol)
            .unwrap_or_else(|| "anthropic".to_string());

        let no_log_body = if args.no_log_body {
            true
        } else {
            file_config.no_log_body.unwrap_or(false)
        };

        let no_log_headers = if args.no_log_headers {
            true
        } else {
            file_config.no_log_headers.unwrap_or(false)
        };

        let log_dir = args.log_dir.clone().or(file_config.log_dir);

        Ok(Self {
            listen,
            backend_url,
            api_key,
            api_protocol,
            no_log_body,
            no_log_headers,
            log_dir,
        })
    }

    pub fn use_anthropic_auth(&self) -> bool {
        self.api_protocol.to_lowercase() != "openai"
    }

    pub fn log_body(&self) -> bool {
        !self.no_log_body
    }

    pub fn log_headers(&self) -> bool {
        !self.no_log_headers
    }

    pub fn listen_addr(&self) -> (&str, u16) {
        if let Some((host, port_str)) = self.listen.rsplit_once(':') {
            let port = port_str.parse::<u16>().unwrap_or(8080);
            (host, port)
        } else {
            ("0.0.0.0", 8080)
        }
    }
}
