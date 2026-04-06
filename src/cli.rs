//! CLI 参数和配置文件解析
//!
//! 支持多层配置合并：CLI 参数 > 配置文件 > 默认值

use std::path::Path;

use clap::Parser;
use serde::Deserialize;
use tracing::info;

use crate::backend::BackendConfig;
use crate::stats::RequestStats;
use crate::telemetry::TelemetryConfig;

/// JSON 配置文件结构
#[derive(Deserialize, Debug, Default)]
pub struct ConfigFile {
    /// 代理监听地址
    pub listen: Option<String>,
    /// 多后端配置
    pub backends: Option<Vec<BackendConfig>>,
    /// 日志目录
    pub log_dir: Option<String>,
    /// 不记录 body
    pub no_log_body: Option<bool>,
    /// 不记录 headers
    pub no_log_headers: Option<bool>,
    /// 统计配置
    pub stats: Option<StatsConfig>,
    /// OpenTelemetry 配置
    pub telemetry: Option<TelemetryConfig>,
}

/// 统计配置
#[derive(Deserialize, Debug, Clone)]
pub struct StatsConfig {
    /// 是否启用统计
    #[serde(default = "default_stats_enabled")]
    pub enabled: bool,
    /// 最大保存记录数
    #[serde(default = "default_max_recent")]
    pub max_recent: usize,
}

fn default_stats_enabled() -> bool {
    true
}

fn default_max_recent() -> usize {
    1000
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: default_stats_enabled(),
            max_recent: default_max_recent(),
        }
    }
}

impl ConfigFile {
    /// 从 JSON 文件加载配置
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "配置文件 '{}' 不存在\n\n请先创建配置文件，例如:\n  cp config.example.json {}",
                path.display(),
                path.display()
            ));
        }

        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置文件 {} 失败: {}", path.display(), e))?;
        let config: Self = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("解析配置文件 {} 失败: {}", path.display(), e))?;
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

    /// 关闭请求/响应 body 日志
    #[arg(long, default_value_t = false)]
    pub no_log_body: bool,

    /// 关闭请求/响应 headers 日志
    #[arg(long, default_value_t = false)]
    pub no_log_headers: bool,

    /// 日志输出目录
    #[arg(long)]
    pub log_dir: Option<String>,

    /// 启用 OpenTelemetry 导出
    #[arg(long)]
    pub enable_telemetry: bool,

    /// OpenTelemetry OTLP 端点
    #[arg(long)]
    pub otlp_endpoint: Option<String>,
}

/// 合并后的最终配置
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub listen: String,
    pub backends: Vec<BackendConfig>,
    pub log_dir: String,
    pub no_log_body: bool,
    pub no_log_headers: bool,
    pub stats: StatsConfig,
    pub telemetry: TelemetryConfig,
}

impl ResolvedConfig {
    /// 从 CLI 参数和配置文件合并，CLI 优先
    pub fn resolve(args: &CliArgs) -> anyhow::Result<Self> {
        let file_config = match &args.config {
            Some(path) => ConfigFile::load(Path::new(path))?,
            None => ConfigFile::default(),
        };

        // 解析后端配置
        let backends = file_config.backends.clone().unwrap_or_default();

        if backends.is_empty() {
            return Err(anyhow::anyhow!(
                "缺少后端配置，请在配置文件中指定 backends"
            ));
        }

        // 解析监听地址
        let listen = args
            .listen
            .clone()
            .or(file_config.listen)
            .unwrap_or_else(|| "0.0.0.0:8989".to_string());

        // 解析日志配置
        let log_dir = args
            .log_dir
            .clone()
            .or(file_config.log_dir)
            .unwrap_or_else(|| "logs".to_string());

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

        // 解析统计配置
        let stats = file_config.stats.unwrap_or_default();

        // 解析 OpenTelemetry 配置
        let telemetry = TelemetryConfig {
            enabled: args.enable_telemetry
                || file_config.telemetry.as_ref().map(|t| t.enabled).unwrap_or(false),
            otlp_endpoint: args
                .otlp_endpoint
                .clone()
                .or(file_config.telemetry.as_ref().map(|t| t.otlp_endpoint.clone()))
                .unwrap_or_else(|| "http://localhost:4317".to_string()),
            service_name: file_config
                .telemetry
                .as_ref()
                .map(|t| t.service_name.clone())
                .unwrap_or_else(|| "api-proxy-debug".to_string()),
            sample_rate: file_config
                .telemetry
                .as_ref()
                .map(|t| t.sample_rate)
                .unwrap_or(1.0),
            enable_metrics: file_config
                .telemetry
                .as_ref()
                .map(|t| t.enable_metrics)
                .unwrap_or(true),
        };

        // 打印配置摘要
        info!("监听地址: {}", listen);
        info!("后端数量: {}", backends.len());
        for backend in &backends {
            let suffix = if backend.match_rules.default {
                " (默认)".to_string()
            } else if let Some(ref prefix) = backend.match_rules.path_prefix {
                format!(" (前缀: {})", prefix)
            } else {
                String::new()
            };
            info!(
                "  - {} [{}]: {}{}",
                backend.name,
                backend.protocol,
                backend.url,
                suffix
            );
        }
        info!("日志目录: {}", log_dir);
        info!("统计: {}", if stats.enabled { "启用" } else { "禁用" });
        info!(
            "OpenTelemetry: {}",
            if telemetry.enabled {
                &telemetry.otlp_endpoint
            } else {
                "禁用"
            }
        );

        Ok(Self {
            listen,
            backends,
            log_dir,
            no_log_body,
            no_log_headers,
            stats,
            telemetry,
        })
    }

    /// 解析监听地址
    pub fn listen_addr(&self) -> (&str, u16) {
        if let Some((host, port_str)) = self.listen.rsplit_once(':') {
            let port = port_str.parse::<u16>().unwrap_or(8989);
            (host, port)
        } else {
            ("0.0.0.0", 8989)
        }
    }

    /// 是否记录 body
    pub fn log_body(&self) -> bool {
        !self.no_log_body
    }

    /// 是否记录 headers
    pub fn log_headers(&self) -> bool {
        !self.no_log_headers
    }

    /// 创建统计实例
    pub fn create_stats(&self) -> Option<std::sync::Arc<RequestStats>> {
        if self.stats.enabled {
            Some(std::sync::Arc::new(RequestStats::new(self.stats.max_recent)))
        } else {
            None
        }
    }
}
