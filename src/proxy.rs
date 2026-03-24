use std::path::Path;
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
use tracing::{error, info};

use crate::cli::ResolvedConfig;
use crate::logger::DualLogger;

/// 后端连接信息，从 CLI 参数解析
#[derive(Clone, Debug)]
pub struct BackendInfo {
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    pub base_path: String,
}

impl BackendInfo {
    /// 从 backend_url 解析出连接信息
    pub fn from_url(backend_url: &str) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(backend_url)
            .map_err(|e| anyhow::anyhow!("无效的 backend-url: {e}"))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("backend-url 缺少 host"))?
            .to_string();

        let use_tls = parsed.scheme() == "https";
        let port = parsed.port().unwrap_or(if use_tls { 443 } else { 80 });

        let base_path = parsed.path().trim_end_matches('/').to_string();

        Ok(Self {
            host,
            port,
            use_tls,
            base_path,
        })
    }
}

/// 每个请求的上下文
pub struct ProxyContext {
    /// 请求开始时间
    pub start_time: Instant,
    /// 收集的请求 body chunks
    pub request_body: Vec<u8>,
}

impl ProxyContext {
    fn new() -> Self {
        Self {
            start_time: Instant::now(),
            request_body: Vec::new(),
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
    pub backend: BackendInfo,
    pub api_key: String,
    pub use_anthropic_auth: bool,
    pub logger: Arc<DualLogger>,
}

#[async_trait]
impl ProxyHttp for ApiProxy {
    type CTX = ProxyContext;

    fn new_ctx(&self) -> Self::CTX {
        ProxyContext::new()
    }

    /// 请求过滤 - 每个请求都会触发，用于记录请求头
    async fn request_filter(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> PingoraResult<bool>
    where
        Self::CTX: Send + Sync,
    {
        let method = session.req_header().method.as_str().to_string();
        let uri = session.req_header().uri.to_string();
        let headers = collect_request_headers(session);

        self.logger.log_request_header(&method, &uri, &headers);

        // 返回 false 表示继续处理
        Ok(false)
    }

    /// 选择上游服务器
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> PingoraResult<Box<HttpPeer>> {
        let mut peer = HttpPeer::new(
            (self.backend.host.as_str(), self.backend.port),
            self.backend.use_tls,
            self.backend.host.clone(),
        );

        // HTTP/2 优先，回退到 HTTP/1.1
        peer.options.alpn = ALPN::H2H1;

        // 连接超时和 keepalive 设置
        peer.options.connection_timeout = Some(Duration::from_secs(10));
        peer.options.total_connection_timeout = Some(Duration::from_secs(30));
        peer.options.idle_timeout = Some(Duration::from_secs(90));
        peer.options.tcp_keepalive = Some(TcpKeepalive {
            idle: Duration::from_secs(60),
            interval: Duration::from_secs(5),
            count: 5,
        });

        if self.backend.use_tls {
            peer.options.h2_ping_interval = Some(Duration::from_secs(30));
        }

        Ok(Box::new(peer))
    }

    /// 修改发往上游的请求
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let original_uri = session.req_header().uri.clone();
        let original_path = original_uri.path();
        let query = original_uri.query();

        // 1. 重写 URI: 拼接 base_path + 原始路径
        let new_path = if self.backend.base_path.is_empty() {
            original_path.to_string()
        } else {
            format!("{}{}", self.backend.base_path, original_path)
        };

        let new_uri_str = if let Some(q) = query {
            format!("{new_path}?{q}")
        } else {
            new_path
        };

        let new_uri: http::Uri = new_uri_str.parse().map_err(|e| {
            error!("URI 重写失败: {e}");
            pingora_core::Error::new_str("URI rewrite failed")
        })?;
        upstream_request.set_uri(new_uri);

        // 2. 移除客户端的认证头
        upstream_request.remove_header("x-api-key");
        upstream_request.remove_header("authorization");

        // 3. 注入真实 API Key
        if self.use_anthropic_auth {
            upstream_request
                .insert_header("x-api-key", &self.api_key)
                .map_err(|e| {
                    error!("注入 x-api-key 失败: {e}");
                    pingora_core::Error::new_str("Header injection failed")
                })?;
        } else {
            upstream_request
                .insert_header("authorization", &format!("Bearer {}", self.api_key))
                .map_err(|e| {
                    error!("注入 authorization 失败: {e}");
                    pingora_core::Error::new_str("Header injection failed")
                })?;
        }

        // 4. 设置 Host 头
        upstream_request
            .insert_header("host", &self.backend.host)
            .map_err(|e| {
                error!("设置 Host 头失败: {e}");
                pingora_core::Error::new_str("Host header failed")
            })?;

        // 5. 记录实际发往上游的请求 (URI 重写 + API Key 注入后)
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
        self.logger.log_upstream_request(
            upstream_request.method.as_str(),
            &upstream_request.uri.to_string(),
            &upstream_headers,
        );

        Ok(())
    }

    /// 捕获请求 body (仅有 body 的请求会触发)
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
        // 收集 body chunks
        if let Some(b) = body {
            ctx.request_body.extend_from_slice(b);
        }

        // body 收集完毕，打印 body 日志
        if end_of_stream && !ctx.request_body.is_empty() {
            self.logger.log_request_body(&ctx.request_body);
        }

        Ok(())
    }

    /// 处理上游响应头
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let status = upstream_response.status.as_u16();

        // 收集响应 headers
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

        self.logger.log_response_start(status, &headers);

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
        // 逐 chunk 打印响应 body
        if let Some(b) = body {
            self.logger.log_response_chunk(b);
        }

        // 响应结束，打印耗时
        if end_of_body {
            let duration = ctx.start_time.elapsed().as_millis() as u64;
            self.logger.log_request_end(duration);
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
        _ctx: &mut Self::CTX,
    ) -> PingoraResult<()> {
        let tls_version = digest
            .and_then(|d| d.ssl_digest.as_ref())
            .map(|ssl| ssl.version.to_string())
            .unwrap_or_else(|| "none".to_string());

        self.logger.log_connection(
            &peer.sni().to_string(),
            &peer.address().to_string(),
            self.backend.use_tls,
            reused,
            &tls_version,
        );

        Ok(())
    }

    /// 代理过程中发生错误的回调
    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        e: Box<pingora_core::Error>,
        _ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        self.logger.log_error(&format!(
            "代理错误 [{}]: {}",
            peer.address(),
            e
        ));
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
        _ctx: &mut Self::CTX,
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
        self.logger.log_error(&format!(
            "请求失败 [{method} {uri}] -> {code}: {e}"
        ));

        // 尝试向下游写入错误响应
        let body = format!("{{\"error\": \"{e}\"}}");
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
    /// 从合并后的配置创建代理服务
    pub fn from_config(config: &ResolvedConfig) -> anyhow::Result<Self> {
        let backend = BackendInfo::from_url(&config.backend_url)?;
        let log_dir = config.log_dir.as_deref().map(Path::new);

        let logger = match log_dir {
            Some(dir) => Arc::new(
                DualLogger::new(config.log_body(), config.log_headers(), dir)
                    .map_err(|e| anyhow::anyhow!("初始化日志目录失败: {e}"))?,
            ),
            None => {
                return Err(anyhow::anyhow!("log_dir 是必填项，请通过 --log-dir 或配置文件指定"));
            }
        };

        info!(
            "后端地址: {}:{} (TLS={})",
            backend.host, backend.port, backend.use_tls
        );
        info!("后端路径前缀: {}", backend.base_path);
        info!(
            "认证方式: {}",
            if config.use_anthropic_auth() {
                "Anthropic (x-api-key)"
            } else {
                "OpenAI (Authorization: Bearer)"
            }
        );

        Ok(Self {
            backend,
            api_key: config.api_key.clone(),
            use_anthropic_auth: config.use_anthropic_auth(),
            logger,
        })
    }
}
