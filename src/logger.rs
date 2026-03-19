use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::Local;

/// 输出写入器，支持仅 stdout 或 stdout + 文件
enum Writer {
    StdoutOnly,
    Dual { file: Mutex<File> },
}

impl Writer {
    fn stdout_only() -> Self {
        Self::StdoutOnly
    }

    fn dual(log_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(log_dir)?;
        let date = Local::now().format("%Y-%m-%d");
        let log_path = log_dir.join(format!("proxy-{date}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        Ok(Self::Dual {
            file: Mutex::new(file),
        })
    }

    fn writeln(&self, line: &str) {
        println!("{line}");
        if let Self::Dual { file } = self {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}

/// 请求/响应日志格式化输出
pub struct RequestLogger {
    log_body: bool,
    log_headers: bool,
    writer: Writer,
}

impl RequestLogger {
    pub fn new(log_body: bool, log_headers: bool, log_dir: Option<&Path>) -> anyhow::Result<Self> {
        let writer = match log_dir {
            Some(dir) => Writer::dual(dir)?,
            None => Writer::stdout_only(),
        };
        Ok(Self {
            log_body,
            log_headers,
            writer,
        })
    }

    /// 打印请求头信息 (每个请求都会调用)
    pub fn log_request_header(
        &self,
        method: &str,
        uri: &str,
        headers: &[(String, String)],
    ) {
        let now = Local::now().format("%H:%M:%S%.3f");
        self.writer.writeln(&format!("\n{}", "═".repeat(80)));
        self.writer
            .writeln(&format!("  REQUEST  [{now}]  {method} {uri}"));
        self.writer.writeln(&format!("{}", "═".repeat(80)));

        if self.log_headers && !headers.is_empty() {
            self.writer.writeln("  Headers:");
            for (name, value) in headers {
                let display_value = if is_sensitive_header(name) {
                    mask_sensitive(value)
                } else {
                    value.clone()
                };
                self.writer
                    .writeln(&format!("    {name}: {display_value}"));
            }
        }
    }

    /// 打印请求 body (仅有 body 的请求才会调用)
    pub fn log_request_body(&self, body: &[u8]) {
        if !self.log_body {
            return;
        }
        if let Ok(text) = std::str::from_utf8(body) {
            if !text.is_empty() {
                self.writer.writeln("  Body:");
                // 尝试美化 JSON
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                        for line in pretty.lines() {
                            self.writer.writeln(&format!("    {line}"));
                        }
                    } else {
                        self.writer.writeln(&format!("    {text}"));
                    }
                } else {
                    self.writer.writeln(&format!("    {text}"));
                }
            }
        } else {
            self.writer
                .writeln(&format!("  Body: <binary {} bytes>", body.len()));
        }
    }

    /// 打印响应头信息
    pub fn log_response_start(&self, status: u16, headers: &[(String, String)]) {
        let now = Local::now().format("%H:%M:%S%.3f");
        self.writer.writeln(&format!("\n{}", "─".repeat(80)));
        self.writer
            .writeln(&format!("  RESPONSE  [{now}]  Status: {status}"));
        self.writer.writeln(&format!("{}", "─".repeat(80)));

        if self.log_headers && !headers.is_empty() {
            self.writer.writeln("  Headers:");
            for (name, value) in headers {
                self.writer.writeln(&format!("    {name}: {value}"));
            }
        }

        if self.log_body {
            self.writer.writeln("  Body (streaming):");
        }
    }

    /// 打印 SSE/streaming 响应 chunk
    pub fn log_response_chunk(&self, chunk: &[u8]) {
        if !self.log_body {
            return;
        }
        if let Ok(text) = std::str::from_utf8(chunk) {
            if !text.is_empty() {
                for line in text.lines() {
                    if !line.is_empty() {
                        self.writer.writeln(&format!("    {line}"));
                    }
                }
            }
        }
    }

    /// 打印请求结束标记
    pub fn log_request_end(&self, duration_ms: u64) {
        self.writer.writeln(&format!("{}", "─".repeat(80)));
        self.writer
            .writeln(&format!("  DONE  耗时: {duration_ms}ms"));
        self.writer.writeln(&format!("{}\n", "═".repeat(80)));
    }

    /// 打印实际发往上游的请求 (URI 重写 + Header 注入后)
    pub fn log_upstream_request(
        &self,
        method: &str,
        uri: &str,
        headers: &[(String, String)],
    ) {
        self.writer.writeln(&format!("  UPSTREAM  {method} {uri}"));
        if self.log_headers && !headers.is_empty() {
            self.writer.writeln("  Upstream Headers:");
            for (name, value) in headers {
                let display_value = if is_sensitive_header(name) {
                    mask_sensitive(value)
                } else {
                    value.clone()
                };
                self.writer
                    .writeln(&format!("    {name}: {display_value}"));
            }
        }
    }

    /// 打印错误信息
    pub fn log_error(&self, message: &str) {
        let now = Local::now().format("%H:%M:%S%.3f");
        self.writer.writeln(&format!("\n{}", "!".repeat(80)));
        self.writer
            .writeln(&format!("  ERROR  [{now}]  {message}"));
        self.writer.writeln(&format!("{}\n", "!".repeat(80)));
    }

    /// 打印上游连接信息
    pub fn log_connection(
        &self,
        sni: &str,
        address: &str,
        use_tls: bool,
        reused: bool,
        tls_version: &str,
    ) {
        self.writer.writeln(&format!(
            "  CONNECT  {sni} -> {address} (TLS={use_tls}, reused={reused}, tls_version={tls_version})"
        ));
    }
}

/// 判断是否为敏感 header
fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "x-api-key"
        || lower == "authorization"
        || lower == "api-key"
        || lower == "x-api-token"
}

/// API Key 脱敏显示
pub fn mask_sensitive(value: &str) -> String {
    if value.len() <= 10 {
        return "***".to_string();
    }
    format!("{}***{}", &value[..6], &value[value.len() - 4..])
}
