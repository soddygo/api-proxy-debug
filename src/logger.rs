use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use chrono::Local;

/// 单个日志文件写入器
struct LogWriter {
    file: Mutex<File>,
    /// 是否同时输出到终端
    print_to_stdout: bool,
}

impl LogWriter {
    fn new(log_dir: &Path, prefix: &str, print_to_stdout: bool) -> anyhow::Result<Self> {
        fs::create_dir_all(log_dir)?;
        let date = Local::now().format("%Y-%m-%d");
        let log_path = log_dir.join(format!("{prefix}-{date}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        Ok(Self {
            file: Mutex::new(file),
            print_to_stdout,
        })
    }

    fn writeln(&self, line: &str) {
        if self.print_to_stdout {
            println!("{line}");
        }
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// 双日志输出器
pub struct DualLogger {
    /// 详细日志 (包含 headers/body，仅写文件)
    detail: LogWriter,
    /// 时间线日志 (仅关键事件，输出到终端+文件)
    timeline: LogWriter,
    /// 是否记录 headers
    log_headers: bool,
    /// 是否记录 body
    log_body: bool,
}

impl DualLogger {
    /// 创建双日志输出器
    pub fn new(log_body: bool, log_headers: bool, log_dir: &Path) -> anyhow::Result<Self> {
        // 详细日志：仅写文件，不输出到终端
        let detail = LogWriter::new(log_dir, "proxy-detail", false)?;
        // 时间线日志：输出到终端 + 文件
        let timeline = LogWriter::new(log_dir, "proxy-timeline", true)?;
        Ok(Self {
            detail,
            timeline,
            log_headers,
            log_body,
        })
    }

    /// 输出到详细日志 (仅写文件)
    fn log_detail(&self, line: &str) {
        self.detail.writeln(line);
    }

    /// 输出到时间线日志 (终端 + 文件)
    fn log_timeline(&self, line: &str) {
        self.timeline.writeln(line);
    }

    /// 打印请求头信息 (每个请求都会调用)
    pub fn log_request_header(
        &self,
        method: &str,
        uri: &str,
        headers: &[(String, String)],
    ) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let header_line = format!("  REQUEST  [{now}]  {method} {uri}");

        // 时间线日志：仅关键事件
        self.log_timeline(&format!("\n{}", "═".repeat(60)));
        self.log_timeline(&header_line);
        self.log_timeline(&format!("{}", "═".repeat(60)));

        // 详细日志：完整内容
        self.log_detail(&format!("\n{}", "═".repeat(80)));
        self.log_detail(&header_line);
        self.log_detail(&format!("{}", "═".repeat(80)));

        if self.log_headers && !headers.is_empty() {
            self.log_detail("  Headers:");
            for (name, value) in headers {
                let display_value = if is_sensitive_header(name) {
                    mask_sensitive(value)
                } else {
                    value.clone()
                };
                self.log_detail(&format!("    {name}: {display_value}"));
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
                self.log_detail("  Body:");
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
                    if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                        for line in pretty.lines() {
                            self.log_detail(&format!("    {line}"));
                        }
                    } else {
                        self.log_detail(&format!("    {text}"));
                    }
                } else {
                    self.log_detail(&format!("    {text}"));
                }
            }
        } else {
            self.log_detail(&format!("  Body: <binary {} bytes>", body.len()));
        }
    }

    /// 打印响应头信息
    pub fn log_response_start(&self, status: u16, headers: &[(String, String)]) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let header_line = format!("  RESPONSE  [{now}]  Status: {status}");

        // 时间线日志
        self.log_timeline(&format!("{}", "─".repeat(60)));
        self.log_timeline(&header_line);

        // 详细日志
        self.log_detail(&format!("\n{}", "─".repeat(80)));
        self.log_detail(&header_line);
        self.log_detail(&format!("{}", "─".repeat(80)));

        if self.log_headers && !headers.is_empty() {
            self.log_detail("  Headers:");
            for (name, value) in headers {
                self.log_detail(&format!("    {name}: {value}"));
            }
        }

        if self.log_body {
            self.log_detail("  Body (streaming):");
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
                        self.log_detail(&format!("    {line}"));
                    }
                }
            }
        }
    }

    /// 打印请求结束标记
    pub fn log_request_end(&self, duration_ms: u64) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let done_line = format!("  DONE  [{now}]  耗时: {duration_ms}ms");

        // 时间线日志
        self.log_timeline(&done_line);
        self.log_timeline(&format!("{}\n", "═".repeat(60)));

        // 详细日志
        self.log_detail(&format!("{}", "─".repeat(80)));
        self.log_detail(&done_line);
        self.log_detail(&format!("{}\n", "═".repeat(80)));
    }

    /// 打印实际发往上游的请求 (URI 重写 + Header 注入后)
    pub fn log_upstream_request(
        &self,
        method: &str,
        uri: &str,
        headers: &[(String, String)],
    ) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let header_line = format!("  UPSTREAM  [{now}]  {method} {uri}");

        // 时间线日志
        self.log_timeline(&header_line);

        // 详细日志
        self.log_detail(&header_line);
        if self.log_headers && !headers.is_empty() {
            self.log_detail("  Upstream Headers:");
            for (name, value) in headers {
                let display_value = if is_sensitive_header(name) {
                    mask_sensitive(value)
                } else {
                    value.clone()
                };
                self.log_detail(&format!("    {name}: {display_value}"));
            }
        }
    }

    /// 打印错误信息
    pub fn log_error(&self, message: &str) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let error_line = format!("  ERROR  [{now}]  {message}");

        // 时间线日志
        self.log_timeline(&format!("\n{}", "!".repeat(60)));
        self.log_timeline(&error_line);
        self.log_timeline(&format!("{}\n", "!".repeat(60)));

        // 详细日志
        self.log_detail(&format!("\n{}", "!".repeat(80)));
        self.log_detail(&error_line);
        self.log_detail(&format!("{}\n", "!".repeat(80)));
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
        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let conn_line = format!(
            "  CONNECT  [{now}]  {sni} -> {address} (TLS={use_tls}, reused={reused}, tls_version={tls_version})"
        );

        // 时间线日志
        self.log_timeline(&conn_line);

        // 详细日志
        self.log_detail(&conn_line);
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
