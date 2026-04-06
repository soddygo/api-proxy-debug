//! 代理核心模块
//!
//! 实现 Pingora ProxyHttp trait，处理请求转发、日志记录、统计等

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use pingora_core::Result as PingoraResult;
use pingora_core::protocols::Digest;
use pingora_core::protocols::TcpKeepalive;
use pingora_core::upstreams::peer::{ALPN, HttpPeer, Peer};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use tracing::{error, info, warn};

use crate::backend::{BackendInfo, BackendRouter};
use crate::cli::ResolvedConfig;
use crate::logger::RequestLogger;
use crate::stats::{RequestRecord, RequestStats, TokenUsage};

/// 每个请求的上下文
pub struct ProxyContext {
    /// 请求开始时间
    pub start_time: Instant,
    /// 收集的请求 body chunks
    pub request_body: Vec<u8>,
    /// 从请求 body 中解析出的模型名称
    pub model: Option<String>,
    /// 选中的后端
    pub selected_backend: Option<BackendInfo>,
    /// 重写后的路径
    pub rewritten_path: Option<String>,
    /// 请求日志记录器
    pub logger: Option<RequestLogger>,
    /// 收集的响应 body（用于解析 Token）
    pub response_body: Vec<u8>,
    /// Token 使用量
    pub token_usage: Option<TokenUsage>,
    /// 响应状态码
    pub status: u16,
}

impl ProxyContext {
    fn new() -> Self {
        Self {
            start_time: Instant::now(),
            request_body: Vec::new(),
            model: None,
            selected_backend: None,
            rewritten_path: None,
            logger: None,
            response_body: Vec::new(),
            token_usage: None,
            status: 0,
        }
    }
}

/// 收集 session 中的请求 headers
fn collect_request_headers(session: &Session) -> Vec<(String, String)> {
    session
        .req_header()
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect()
}

/// API 代理服务 - 实现 Pingora ProxyHttp trait
pub struct ApiProxy {
    pub router: Arc<BackendRouter>,
    pub stats: Option<Arc<RequestStats>>,
}

#[async_trait]
impl ProxyHttp for ApiProxy {
    type CTX = ProxyContext;

    fn new_ctx(&self) -> Self::CTX {
        ProxyContext::new()
    }

    /// 请求过滤 - 每个请求都会触发
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<bool>
    where
        Self::CTX: Send + Sync,
    {
        let method = session.req_header().method.as_str().to_string();
        let uri = session.req_header().uri.to_string();
        let path = session.req_header().uri.path().to_string();
        let headers = collect_request_headers(session);

        // 选择后端
        let (backend, rewritten_path) = match self.router.select_and_rewrite(&path, &headers) {
            Some(result) => result,
            None => {
                warn!("没有匹配的后端: {}", path);
                return Err(pingora_core::Error::new_str("No matching backend"));
            }
        };

        ctx.selected_backend = Some(backend.clone());
        ctx.rewritten_path = Some(rewritten_path);

        // 创建请求日志记录器
        let mut logger = RequestLogger::new(&method, &uri, &backend.name);
        logger.log_request_headers(&headers);
        ctx.logger = Some(logger);

        // 返回 false 表示继续处理
        Ok(false)
    }

    /// 选择上游服务器
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<Box<HttpPeer>> {
        let backend = ctx.selected_backend.as_ref()
            .ok_or_else(|| pingora_core::Error::new_str("No backend selected"))?;

        let mut peer = HttpPeer::new(
            (backend.host.as_str(), backend.port),
            backend.use_tls,
            backend.host.clone(),
        );

        // HTTP/2 优先
        peer.options.alpn = ALPN::H2H1;

        // 连接配置
        peer.options.connection_timeout = Some(Duration::from_secs(10));
        peer.options.total_connection_timeout = Some(Duration::from_secs(30));
        peer.options.idle_timeout = Some(Duration::from_secs(90));
        peer.options.tcp_keepalive = Some(TcpKeepalive {
            idle: Duration::from_secs(60),
            interval: Duration::from_secs(5),
            count: 5,
        });

        if backend.use_tls {
            peer.options.h2_ping_interval = Some(Duration::from_secs(30));
        }

        Ok(Box::new(peer))
    }

    /// 修改发往上游的请求
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let backend = ctx.selected_backend.as_ref()
            .ok_or_else(|| pingora_core::Error::new_str("No backend selected"))?;
        let rewritten_path = ctx.rewritten_path.as_ref()
            .ok_or_else(|| pingora_core::Error::new_str("No rewritten path"))?;

        let original_uri = session.req_header().uri.clone();
        let query = original_uri.query();

        // 构建新的 URI
        let new_uri_str = if let Some(q) = query {
            format!("{}?{}", rewritten_path, q)
        } else {
            rewritten_path.clone()
        };

        let new_uri: http::Uri = new_uri_str.parse().map_err(|e| {
            error!("URI 重写失败: {}", e);
            pingora_core::Error::new_str("URI rewrite failed")
        })?;
        upstream_request.set_uri(new_uri);

        // 移除客户端的认证头
        upstream_request.remove_header("x-api-key");
        upstream_request.remove_header("authorization");

        // 注入 API Key
        if backend.use_anthropic_auth() {
            upstream_request
                .insert_header("x-api-key", &backend.api_key)
                .map_err(|e| {
                    error!("注入 x-api-key 失败: {}", e);
                    pingora_core::Error::new_str("Header injection failed")
                })?;
        } else {
            upstream_request
                .insert_header("authorization", &format!("Bearer {}", backend.api_key))
                .map_err(|e| {
                    error!("注入 authorization 失败: {}", e);
                    pingora_core::Error::new_str("Header injection failed")
                })?;
        }

        // 设置 Host 头
        upstream_request
            .insert_header("host", &backend.host)
            .map_err(|e| {
                error!("设置 Host 头失败: {}", e);
                pingora_core::Error::new_str("Host header failed")
            })?;

        // 记录上游请求
        let upstream_headers: Vec<(String, String)> = upstream_request
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();

        if let Some(ref logger) = ctx.logger {
            logger.log_upstream_request(
                upstream_request.method.as_str(),
                &upstream_request.uri.to_string(),
                &upstream_headers,
            );
        }

        Ok(())
    }

    /// 捕获请求 body
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<()>
    where
        Self::CTX: Send + Sync,
    {
        // 收集 body 到 context
        if let Some(b) = body {
            ctx.request_body.extend_from_slice(b);
        }

        // body 收集完毕
        if end_of_stream && !ctx.request_body.is_empty() {
            // 先将 body 传递给 logger
            if let Some(ref mut logger) = ctx.logger {
                logger.collect_body(&ctx.request_body);
                logger.log_request_body();
            }

            // 解析模型名称
            if ctx.model.is_none() {
                if let Ok(text) = std::str::from_utf8(&ctx.request_body) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                        if let Some(model) = json.get("model").and_then(|v| v.as_str()) {
                            ctx.model = Some(model.to_string());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// 处理上游响应头
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let status = upstream_response.status.as_u16();
        ctx.status = status;

        let headers: Vec<(String, String)> = upstream_response
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();

        if let Some(ref logger) = ctx.logger {
            logger.log_response_start(status, &headers);
        }

        Ok(())
    }

    /// 捕获响应 body (支持 SSE streaming)
    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_body: bool,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<Option<Duration>>
    where
        Self::CTX: Send + Sync,
    {
        // 记录响应 chunk
        if let Some(b) = body {
            if let Some(ref logger) = ctx.logger {
                logger.log_response_chunk(b);
            }

            // 收集响应用于解析 Token
            ctx.response_body.extend_from_slice(b);

            // 尝试解析 Token 使用量
            if let Ok(text) = std::str::from_utf8(b) {
                if let Some(usage) = TokenUsage::parse_from_sse(text) {
                    ctx.token_usage = Some(usage);
                }
            }
        }

        // 响应结束
        if end_of_body {
            let duration_ms = ctx.start_time.elapsed().as_millis() as u64;

            if let Some(ref logger) = ctx.logger {
                logger.log_request_end(duration_ms, ctx.status);
            }

            // 记录统计
            if let Some(ref stats) = self.stats {
                let usage = ctx.token_usage.as_ref();
                stats.record_request(RequestRecord {
                    timestamp: chrono::Utc::now(),
                    method: "POST".to_string(),
                    uri: String::new(),
                    backend: ctx.selected_backend.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
                    model: ctx.model.clone(),
                    status: ctx.status,
                    duration_ms,
                    input_tokens: usage.map(|u| u.input_tokens),
                    output_tokens: usage.map(|u| u.output_tokens),
                    cache_read_tokens: usage.map(|u| u.cache_read_tokens),
                    cache_creation_tokens: usage.map(|u| u.cache_creation_tokens),
                    error: None,
                });
            }
        }

        Ok(None)
    }

    /// 连接到上游后的回调
    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        reused: bool,
        peer: &HttpPeer,
        #[cfg(unix)] _fd: std::os::unix::io::RawFd,
        #[cfg(windows)] _sock: std::os::windows::io::RawSocket,
        digest: Option<&Digest>,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let tls_version = digest
            .and_then(|d| d.ssl_digest.as_ref())
            .map(|ssl| ssl.version.to_string())
            .unwrap_or_else(|| "none".to_string());

        let backend = ctx.selected_backend.as_ref();
        let use_tls = backend.map(|b| b.use_tls).unwrap_or(false);

        if let Some(ref logger) = ctx.logger {
            logger.log_connection(
                &peer.sni().to_string(),
                &peer.address().to_string(),
                use_tls,
                reused,
                &tls_version,
            );
        }

        Ok(())
    }

    /// 代理过程中发生错误的回调
    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<pingora_core::Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        if let Some(ref logger) = ctx.logger {
            logger.log_error(&format!("代理错误 [{}]: {}", peer.address(), e));
        }

        // 记录错误统计
        if let Some(ref stats) = self.stats {
            stats.record_request(RequestRecord {
                timestamp: chrono::Utc::now(),
                method: session.req_header().method.as_str().to_string(),
                uri: session.req_header().uri.to_string(),
                backend: ctx.selected_backend.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
                model: ctx.model.clone(),
                status: 502,
                duration_ms: ctx.start_time.elapsed().as_millis() as u64,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                error: Some(e.to_string()),
            });
        }

        let mut e = e.more_context(format!("Peer: {}", peer));
        e.retry
            .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());
        e
    }

    /// 请求遇到致命错误的回调
    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        e: &pingora_core::Error,
        ctx: &mut Self::CTX,
    ) -> pingora_proxy::FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        let code = match e.etype() {
            &pingora_core::ErrorType::ConnectTimedout => 504,
            &pingora_core::ErrorType::ConnectRefused => 502,
            &pingora_core::ErrorType::TLSHandshakeFailure => 502,
            _ => 502,
        };

        let method = session.req_header().method.as_str();
        let uri = &session.req_header().uri;

        if let Some(ref logger) = ctx.logger {
            logger.log_error(&format!("请求失败 [{} {}] -> {}: {}", method, uri, code, e));
        }

        // 返回错误响应
        let body = format!(r#"{{"error": "{}"}}"#, e);
        if let Ok(mut resp) = pingora_http::ResponseHeader::build(code, None) {
            let _ = resp.insert_header("content-type", "application/json");
            let _ = resp.insert_header("content-length", &body.len().to_string());
            let _ = session.write_response_header(Box::new(resp), false).await;
            let _ = session
                .write_response_body(Some(bytes::Bytes::from(body)), true)
                .await;
        }

        pingora_proxy::FailToProxy {
            error_code: code,
            can_reuse_downstream: false,
        }
    }
}

impl ApiProxy {
    /// 从配置创建代理服务
    pub fn from_config(config: &ResolvedConfig, stats: Option<Arc<RequestStats>>) -> anyhow::Result<Self> {
        let router = Arc::new(BackendRouter::new(config.backends.clone())?);

        info!("代理服务创建成功");
        info!("后端列表: {:?}", router.backend_names());

        Ok(Self { router, stats })
    }
}
