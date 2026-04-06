//! 日志模块 - 基于 tracing 生态
//!
//! 使用 tracing-appender 实现异步文件日志，支持：
//! - 多层日志输出（timeline + detail）
//! - 按天滚动
//! - 敏感信息脱敏
//! - 结构化日志字段

use std::path::Path;

use tracing::{debug, error, info};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter, Layer, Registry,
};


/// 初始化日志系统
///
/// 配置多层日志输出：
/// - stdout: 终端输出（timeline 级别）
/// - detail_file: 详细日志文件（所有级别）
/// - timeline_file: 时间线日志文件
pub fn init_logging(
    log_dir: &Path,
    log_body: bool,
    log_headers: bool,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(log_dir)?;

    // 创建文件 appender - 详细日志
    let detail_appender = tracing_appender::rolling::daily(log_dir, "proxy-detail.log");
    let detail_layer = fmt::layer()
        .with_writer(detail_appender)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug")));

    // 创建文件 appender - 时间线日志
    let timeline_appender = tracing_appender::rolling::daily(log_dir, "proxy-timeline.log");
    let timeline_file_layer = fmt::layer()
        .with_writer(timeline_appender)
        .with_target(false)
        .with_thread_ids(false)
        .with_ansi(false)
        .with_filter(create_timeline_filter());

    // 终端输出层
    let stdout_layer = fmt::layer()
        .with_writer(std::io::stdout)
        .with_target(false)
        .with_thread_ids(false)
        .with_ansi(true)
        .with_filter(create_timeline_filter());

    // 初始化订阅者
    Registry::default()
        .with(detail_layer)
        .with(timeline_file_layer)
        .with(stdout_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("初始化日志系统失败: {}", e))?;

    // 存储全局配置
    LOG_CONFIG.get_or_init(|| LogConfig {
        log_body,
        log_headers,
    });

    info!("日志系统初始化完成: {}", log_dir.display());
    Ok(())
}

/// 创建时间线日志过滤器（只记录关键事件）
fn create_timeline_filter() -> EnvFilter {
    EnvFilter::builder()
        .parse("api_proxy=info")
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// 全局日志配置
static LOG_CONFIG: std::sync::OnceLock<LogConfig> = std::sync::OnceLock::new();

struct LogConfig {
    log_body: bool,
    log_headers: bool,
}

/// 请求日志记录器
///
/// 使用 tracing 的 span 来记录单个请求的完整生命周期
pub struct RequestLogger {
    span: tracing::Span,
    request_body: Vec<u8>,
    model: Option<String>,
}

impl RequestLogger {
    /// 创建新的请求日志记录器
    pub fn new(method: &str, uri: &str, backend: &str) -> Self {
        let span = tracing::info_span!(
            "request",
            method = %method,
            uri = %uri,
            backend = %backend,
        );

        span.in_scope(|| {
            info!("[REQUEST] {} {}", method, uri);
        });

        Self {
            span,
            request_body: Vec::new(),
            model: None,
        }
    }

    /// 记录请求 Headers
    pub fn log_request_headers(&mut self, headers: &[(String, String)]) {
        let config = LOG_CONFIG.get().unwrap();
        if !config.log_headers {
            return;
        }

        self.span.in_scope(|| {
            debug!("Request Headers:");
            for (name, value) in headers {
                let display_value = if is_sensitive_header(name) {
                    mask_sensitive(value)
                } else {
                    value.clone()
                };
                debug!("  {}: {}", name, display_value);
            }
        });
    }

    /// 收集请求 Body
    pub fn collect_body(&mut self, body: &[u8]) {
        self.request_body.extend_from_slice(body);
    }

    /// 记录请求 Body
    pub fn log_request_body(&self) {
        let config = LOG_CONFIG.get().unwrap();
        if !config.log_body || self.request_body.is_empty() {
            return;
        }

        self.span.in_scope(|| {
            if let Ok(text) = std::str::from_utf8(&self.request_body) {
                // 紧凑输出 JSON（单行）
                let body_text = if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                    serde_json::to_string(&json).unwrap_or_else(|_| text.to_string())
                } else {
                    text.to_string()
                };
                info!("[REQUEST BODY] {}", body_text);
            }
        });
    }

    /// 解析请求 Body 中的模型名称
    pub fn parse_model(&mut self) {
        if self.model.is_some() {
            return;
        }

        if let Ok(text) = std::str::from_utf8(&self.request_body) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                    self.model = Some(model.to_string());
                }
            }
        }
    }

    /// 记录上游请求
    pub fn log_upstream_request(&self, method: &str, uri: &str, headers: &[(String, String)]) {
        self.span.in_scope(|| {
            info!("[UPSTREAM] {} {}", method, uri);

            let config = LOG_CONFIG.get().unwrap();
            if config.log_headers {
                debug!("Upstream Headers:");
                for (name, value) in headers {
                    let display_value = if is_sensitive_header(name) {
                        mask_sensitive(value)
                    } else {
                        value.clone()
                    };
                    debug!("  {}: {}", name, display_value);
                }
            }
        });
    }

    /// 记录连接信息
    pub fn log_connection(&self, sni: &str, address: &str, use_tls: bool, reused: bool, tls_version: &str) {
        self.span.in_scope(|| {
            info!(
                "[CONNECT] {} -> {} (TLS={}, reused={}, tls_version={})",
                sni, address, use_tls, reused, tls_version
            );
        });
    }

    /// 记录响应开始
    pub fn log_response_start(&self, status: u16, headers: &[(String, String)]) {
        // 注意：不在此处 record status，避免重复
        // status 将在 log_request_end 中统一记录

        self.span.in_scope(|| {
            info!("[RESPONSE] Status: {}", status);

            let config = LOG_CONFIG.get().unwrap();
            if config.log_headers {
                debug!("Response Headers:");
                for (name, value) in headers {
                    debug!("  {}: {}", name, value);
                }
            }
        });
    }

    /// 记录响应 chunk
    pub fn log_response_chunk(&self, chunk: &[u8]) {
        let config = LOG_CONFIG.get().unwrap();
        if !config.log_body {
            return;
        }

        if let Ok(text) = std::str::from_utf8(chunk) {
            for line in text.lines() {
                if !line.is_empty() {
                    debug!("  {}", line);
                }
            }
        }
    }

    /// 记录请求结束
    pub fn log_request_end(&self, duration_ms: u64, status: u16) {
        self.span.in_scope(|| {
            info!("[DONE] 耗时: {}ms, 状态: {}", duration_ms, status);
        });
        // 注意：统计记录由 proxy.rs 统一处理，避免重复
    }

    /// 记录错误
    pub fn log_error(&self, message: &str) {
        self.span.in_scope(|| {
            error!("[ERROR] {}", message);
        });
    }

    /// 获取模型名称
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// 判断是否为敏感 header
pub fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "x-api-key"
        || lower == "authorization"
        || lower == "api-key"
        || lower == "x-api-token"
        || lower == "cookie"
        || lower == "set-cookie"
}

/// API Key 脱敏显示
pub fn mask_sensitive(value: &str) -> String {
    if value.len() <= 10 {
        return "***".to_string();
    }
    // 对于 Bearer token
    if value.starts_with("Bearer ") {
        let token = &value[7..];
        if token.len() <= 10 {
            return "Bearer ***".to_string();
        }
        return format!("Bearer {}***{}", &token[..6], &token[token.len() - 4..]);
    }
    format!("{}***{}", &value[..6], &value[value.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_sensitive() {
        assert_eq!(mask_sensitive("short"), "***");
        assert_eq!(
            mask_sensitive("sk-ant-api03-xxxxxxxxxxxx"),
            "sk-ant***xxxx"
        );
        // Bearer token: "Bearer sk-project-12345" -> token 长度 16，前 6 个是 "sk-pro"
        assert_eq!(
            mask_sensitive("Bearer sk-project-12345"),
            "Bearer sk-pro***2345"
        );
    }

    #[test]
    fn test_is_sensitive_header() {
        assert!(is_sensitive_header("x-api-key"));
        assert!(is_sensitive_header("X-API-KEY"));
        assert!(is_sensitive_header("Authorization"));
        assert!(is_sensitive_header("authorization"));
        assert!(!is_sensitive_header("content-type"));
    }
}
