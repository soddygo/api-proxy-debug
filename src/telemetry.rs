//! OpenTelemetry 集成模块
//!
//! 提供分布式追踪和指标导出能力：
//! - OTLP 协议导出（Jaeger、Grafana Tempo、Zipkin 等）
//! - 自动从 tracing span 生成 OpenTelemetry traces
//! - Metrics 导出（Prometheus、Grafana 等）
//!
//! ## 使用方式
//!
//! 1. 启动 OTLP Collector（例如使用 Jaeger）：
//!    ```bash
//!    docker run -d --name jaeger \
//!      -p 4317:4317 \
//!      -p 16686:16686 \
//!      jaegertracing/all-in-one:latest
//!    ```
//!
//! 2. 配置 config.json：
//!    ```json
//!    {
//!      "telemetry": {
//!        "enabled": true,
//!        "otlp_endpoint": "http://localhost:4317"
//!      }
//!    }
//!    ```
//!
//! 3. 访问 Jaeger UI: http://localhost:16686

use std::time::Duration;

use opentelemetry::{global, trace::TracerProvider as TracerProviderTrait, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    runtime,
    trace::{Sampler, TracerProvider},
    Resource,
};
use serde::Deserialize;
use tracing::info;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Registry;

/// OpenTelemetry 配置
#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryConfig {
    /// 是否启用 OpenTelemetry
    #[serde(default)]
    pub enabled: bool,

    /// OTLP Collector 端点 (例如: http://localhost:4317)
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,

    /// 服务名称
    #[serde(default = "default_service_name")]
    pub service_name: String,

    /// 采样率 (0.0 - 1.0)
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,

    /// 是否启用 metrics
    #[serde(default = "default_enable_metrics")]
    pub enable_metrics: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
            sample_rate: default_sample_rate(),
            enable_metrics: default_enable_metrics(),
        }
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_service_name() -> String {
    "api-proxy-debug".to_string()
}

fn default_sample_rate() -> f64 {
    1.0
}

fn default_enable_metrics() -> bool {
    true
}

/// OpenTelemetry 初始化器
///
/// 管理完整的 OpenTelemetry 生命周期：
/// - 初始化 TracerProvider 和 MeterProvider
/// - 创建 tracing layer 用于自动导出
/// - 优雅关闭时清理资源
pub struct TelemetryInitializer {
    config: TelemetryConfig,
    tracer_provider: Option<TracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryInitializer {
    /// 创建新的初始化器
    pub fn new(config: TelemetryConfig) -> Self {
        Self {
            config,
            tracer_provider: None,
            meter_provider: None,
        }
    }

    /// 初始化 OpenTelemetry
    ///
    /// 此方法会：
    /// 1. 创建 TracerProvider（用于导出 traces）
    /// 2. 创建 MeterProvider（用于导出 metrics）
    /// 3. 设置全局 provider
    pub fn init(&mut self) -> anyhow::Result<()> {
        if !self.config.enabled {
            info!("OpenTelemetry 未启用");
            return Ok(());
        }

        info!(
            "初始化 OpenTelemetry: endpoint={}, service={}",
            self.config.otlp_endpoint, self.config.service_name
        );

        // 创建资源（服务标识）
        let resource = Resource::new(vec![KeyValue::new(
            "service.name",
            self.config.service_name.clone(),
        )]);

        // 初始化 Tracing
        let tracer_provider = self.init_tracing(&resource)?;
        self.tracer_provider = Some(tracer_provider);

        // 初始化 Metrics
        if self.config.enable_metrics {
            let meter_provider = self.init_metrics(&resource)?;
            self.meter_provider = Some(meter_provider);
        }

        info!("OpenTelemetry 初始化成功");
        Ok(())
    }

    /// 初始化分布式追踪
    fn init_tracing(&self, resource: &Resource) -> anyhow::Result<TracerProvider> {
        // 创建 OTLP span exporter (gRPC)
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(format!("{}/v1/traces", self.config.otlp_endpoint))
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(10))
            .build()?;

        // 配置采样器
        let sampler = Sampler::TraceIdRatioBased(self.config.sample_rate);

        // 创建 TracerProvider with batch exporter
        let tracer_provider = TracerProvider::builder()
            .with_resource(resource.clone())
            .with_sampler(sampler)
            .with_batch_exporter(exporter, runtime::Tokio)
            .build();

        // 设置为全局 provider
        global::set_tracer_provider(tracer_provider.clone());

        Ok(tracer_provider)
    }

    /// 初始化 Metrics
    fn init_metrics(&self, resource: &Resource) -> anyhow::Result<SdkMeterProvider> {
        // 创建 OTLP metrics exporter (gRPC)
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(format!("{}/v1/metrics", self.config.otlp_endpoint))
            .with_protocol(Protocol::Grpc)
            .with_timeout(Duration::from_secs(10))
            .build()?;

        // 创建 MeterProvider，配置定期导出
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_reader(
                PeriodicReader::builder(exporter, runtime::Tokio)
                    .with_interval(Duration::from_secs(30))
                    .with_timeout(Duration::from_secs(10))
                    .build(),
            )
            .build();

        // 设置为全局 provider
        global::set_meter_provider(meter_provider.clone());

        Ok(meter_provider)
    }

    /// 创建 OpenTelemetry Layer 用于 tracing-subscriber
    ///
    /// 这个 layer 会自动将 tracing span 转换为 OpenTelemetry span
    ///
    /// 使用示例：
    /// ```rust
    /// let telemetry = TelemetryInitializer::new(config);
    /// telemetry.init()?;
    /// 
    /// let subscriber = Registry::default()
    ///     .with(tracing_subscriber::fmt::layer())
    ///     .with(telemetry.create_tracing_layer());
    /// ```
    pub fn create_tracing_layer(&self) -> Option<OpenTelemetryLayer<Registry, opentelemetry_sdk::trace::Tracer>> {
        if !self.config.enabled {
            return None;
        }

        // 从 TracerProvider 获取具体的 Tracer
        // 使用 TracerProviderTrait trait 的 tracer 方法
        let tracer = TracerProviderTrait::tracer(
            self.tracer_provider.as_ref()?,
            self.config.service_name.clone(),
        );
        
        Some(tracing_opentelemetry::layer().with_tracer(tracer))
    }

    /// 关闭 OpenTelemetry
    ///
    /// 确保所有 pending 的数据都被导出
    pub fn shutdown(&self) {
        if let Some(_provider) = &self.tracer_provider {
            // 强制 flush 并关闭 tracer provider
            let _ = opentelemetry::global::shutdown_tracer_provider();
            info!("OpenTelemetry TracerProvider 已关闭");
        }
        if let Some(provider) = &self.meter_provider {
            // 强制 flush 并关闭 meter provider
            let _ = provider.shutdown();
            info!("OpenTelemetry MeterProvider 已关闭");
        }
    }

    /// 获取服务名称
    pub fn service_name(&self) -> &str {
        &self.config.service_name
    }

    /// 是否已启用
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
}

impl Drop for TelemetryInitializer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ============================================================================
// 辅助函数：创建 span 属性
// ============================================================================

/// 创建 HTTP 请求相关的 span 属性
pub fn http_request_attributes(method: &str, uri: &str, backend: &str) -> Vec<KeyValue> {
    vec![
        KeyValue::new("http.method", method.to_string()),
        KeyValue::new("http.url", uri.to_string()),
        KeyValue::new("backend.name", backend.to_string()),
    ]
}

/// 创建 HTTP 响应相关的 span 属性
pub fn http_response_attributes(status: u16, duration_ms: u64) -> Vec<KeyValue> {
    vec![
        KeyValue::new("http.status_code", status as i64),
        KeyValue::new("http.duration_ms", duration_ms as i64),
    ]
}

/// 创建 Token 使用量相关的 span 属性
pub fn token_usage_attributes(
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
) -> Vec<KeyValue> {
    vec![
        KeyValue::new("tokens.input", input_tokens as i64),
        KeyValue::new("tokens.output", output_tokens as i64),
        KeyValue::new("tokens.cache_read", cache_read_tokens as i64),
        KeyValue::new("tokens.cache_creation", cache_creation_tokens as i64),
    ]
}

/// 创建错误相关的 span 属性
pub fn error_attributes(error_type: &str, error_message: &str) -> Vec<KeyValue> {
    vec![
        KeyValue::new("error.type", error_type.to_string()),
        KeyValue::new("error.message", error_message.to_string()),
    ]
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_config_defaults() {
        let config = TelemetryConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.otlp_endpoint, "http://localhost:4317");
        assert_eq!(config.service_name, "api-proxy-debug");
        assert_eq!(config.sample_rate, 1.0);
        assert!(config.enable_metrics);
    }

    #[test]
    fn test_http_request_attributes() {
        let attrs = http_request_attributes("POST", "/v1/messages", "anthropic");
        assert_eq!(attrs.len(), 3);
    }

    #[test]
    fn test_token_usage_attributes() {
        let attrs = token_usage_attributes(100, 50, 10, 5);
        assert_eq!(attrs.len(), 4);
    }
}
