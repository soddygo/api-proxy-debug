//! 后端配置与路由模块
//!
//! 支持配置多个后端，根据请求路径/规则进行路由

use serde::Deserialize;
use tracing::{debug, info, warn};

/// 单个后端配置
#[derive(Debug, Clone, Deserialize)]
pub struct BackendConfig {
    /// 后端名称（用于日志和统计）
    pub name: String,
    /// 后端 URL (例如: https://api.anthropic.com)
    pub url: String,
    /// API Key
    pub api_key: String,
    /// API 协议: anthropic 或 openai
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// 匹配规则
    #[serde(default)]
    pub match_rules: MatchRules,
}

fn default_protocol() -> String {
    "anthropic".to_string()
}

/// 后端匹配规则
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MatchRules {
    /// 路径前缀匹配
    pub path_prefix: Option<String>,
    /// Header 匹配 (key, value)
    pub header: Option<HeaderMatch>,
    /// 是否为默认后端
    #[serde(default)]
    pub default: bool,
}

/// Header 匹配规则
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderMatch {
    pub name: String,
    pub value: String,
}

/// 解析后的后端连接信息
#[derive(Clone, Debug)]
pub struct BackendInfo {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub use_tls: bool,
    pub base_path: String,
    pub api_key: String,
    pub protocol: String,
}

impl BackendInfo {
    /// 从 URL 解析连接信息
    pub fn from_config(config: &BackendConfig) -> anyhow::Result<Self> {
        let parsed = url::Url::parse(&config.url)
            .map_err(|e| anyhow::anyhow!("无效的后端 URL '{}': {}", config.name, e))?;

        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("后端 '{}' 缺少 host", config.name))?
            .to_string();

        let use_tls = parsed.scheme() == "https";
        let port = parsed.port().unwrap_or(if use_tls { 443 } else { 80 });
        let base_path = parsed.path().trim_end_matches('/').to_string();

        Ok(Self {
            name: config.name.clone(),
            host,
            port,
            use_tls,
            base_path,
            api_key: config.api_key.clone(),
            protocol: config.protocol.clone(),
        })
    }

    /// 是否使用 Anthropic 认证方式
    pub fn use_anthropic_auth(&self) -> bool {
        self.protocol.to_lowercase() != "openai"
    }
}

/// 后端路由器
#[derive(Debug, Clone)]
pub struct BackendRouter {
    /// 所有后端配置
    backends: Vec<(BackendConfig, BackendInfo)>,
    /// 默认后端索引
    default_index: Option<usize>,
}

impl BackendRouter {
    /// 创建路由器
    pub fn new(configs: Vec<BackendConfig>) -> anyhow::Result<Self> {
        if configs.is_empty() {
            return Err(anyhow::anyhow!("至少需要配置一个后端"));
        }

        let mut backends = Vec::new();
        let mut default_index = None;

        for (i, config) in configs.into_iter().enumerate() {
            info!(
                "加载后端 [{}]: {} -> {} (协议: {})",
                config.name, config.url, config.protocol,
                if config.match_rules.default { " [默认]" } else { "" }
            );

            if config.match_rules.default {
                if default_index.is_some() {
                    warn!("多个默认后端配置，将使用最后一个");
                }
                default_index = Some(i);
            }

            let info = BackendInfo::from_config(&config)?;
            backends.push((config, info));
        }

        // 如果没有指定默认后端，使用第一个
        let default_index = default_index.or(Some(0));

        Ok(Self {
            backends,
            default_index,
        })
    }

    /// 根据请求路径和 headers 选择后端
    pub fn select(&self, path: &str, headers: &[(String, String)]) -> Option<&BackendInfo> {
        for (config, info) in &self.backends {
            // 路径前缀匹配
            if let Some(ref prefix) = config.match_rules.path_prefix {
                if path.starts_with(prefix) {
                    debug!("路径 '{}' 匹配后端 '{}' (前缀: {})", path, config.name, prefix);
                    return Some(info);
                }
            }

            // Header 匹配
            if let Some(ref header_match) = config.match_rules.header {
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case(&header_match.name)
                        && value == &header_match.value
                    {
                        debug!(
                            "Header '{}: {}' 匹配后端 '{}'",
                            header_match.name, header_match.value, config.name
                        );
                        return Some(info);
                    }
                }
            }
        }

        // 回退到默认后端
        if let Some(index) = self.default_index {
            debug!("使用默认后端 '{}'", self.backends[index].1.name);
            return Some(&self.backends[index].1);
        }

        None
    }

    /// 选择后端并计算重写后的路径
    pub fn select_and_rewrite(
        &self,
        path: &str,
        headers: &[(String, String)],
    ) -> Option<(&BackendInfo, String)> {
        let (config, info) = self.select_with_config(path, headers)?;
        
        // 如果有路径前缀，移除它
        let mut new_path = if let Some(ref prefix) = config.match_rules.path_prefix {
            path.strip_prefix(prefix).unwrap_or(path).to_string()
        } else {
            path.to_string()
        };

        // 加上后端的 base_path
        if !info.base_path.is_empty() {
            new_path = format!("{}{}", info.base_path, new_path);
        }

        Some((info, new_path))
    }

    /// 选择后端并返回配置
    pub fn select_with_config(
        &self,
        path: &str,
        headers: &[(String, String)],
    ) -> Option<(&BackendConfig, &BackendInfo)> {
        for (config, info) in &self.backends {
            // 路径前缀匹配
            if let Some(ref prefix) = config.match_rules.path_prefix {
                if path.starts_with(prefix) {
                    return Some((config, info));
                }
            }

            // Header 匹配
            if let Some(ref header_match) = config.match_rules.header {
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case(&header_match.name)
                        && value == &header_match.value
                    {
                        return Some((config, info));
                    }
                }
            }
        }

        // 回退到默认后端
        self.default_index.map(|i| (&self.backends[i].0, &self.backends[i].1))
    }

    /// 获取所有后端名称
    pub fn backend_names(&self) -> Vec<&str> {
        self.backends.iter().map(|(c, _)| c.name.as_str()).collect()
    }

    /// 获取默认后端
    pub fn default_backend(&self) -> Option<&BackendInfo> {
        self.default_index.map(|i| &self.backends[i].1)
    }
}

impl BackendConfig {
    /// 从配置创建 BackendInfo
    pub fn to_backend_info(&self) -> anyhow::Result<BackendInfo> {
        BackendInfo::from_config(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_router() {
        let configs = vec![
            BackendConfig {
                name: "anthropic".to_string(),
                url: "https://api.anthropic.com".to_string(),
                api_key: "sk-ant-xxx".to_string(),
                protocol: "anthropic".to_string(),
                match_rules: MatchRules {
                    path_prefix: Some("/anthropic".to_string()),
                    ..Default::default()
                },
            },
            BackendConfig {
                name: "openai".to_string(),
                url: "https://api.openai.com/v1".to_string(),
                api_key: "sk-xxx".to_string(),
                protocol: "openai".to_string(),
                match_rules: MatchRules {
                    path_prefix: Some("/openai".to_string()),
                    ..Default::default()
                },
            },
            BackendConfig {
                name: "default".to_string(),
                url: "https://open.bigmodel.cn/api/anthropic".to_string(),
                api_key: "xxx".to_string(),
                protocol: "anthropic".to_string(),
                match_rules: MatchRules {
                    default: true,
                    ..Default::default()
                },
            },
        ];

        let router = BackendRouter::new(configs).unwrap();

        // 测试路径匹配 - anthropic 没有路径前缀
        let (info, path) = router.select_and_rewrite("/anthropic/v1/messages", &[]).unwrap();
        assert_eq!(info.name, "anthropic");
        assert_eq!(path, "/v1/messages"); // 移除 /anthropic 前缀，base_path 为空

        // 测试路径匹配 - openai 有 /v1 路径前缀
        let (info, path) = router.select_and_rewrite("/openai/chat/completions", &[]).unwrap();
        assert_eq!(info.name, "openai");
        assert_eq!(path, "/v1/chat/completions"); // 移除 /openai 前缀，加上 base_path /v1

        // 测试默认后端 - 有 /api/anthropic 路径前缀
        let (info, path) = router.select_and_rewrite("/v1/messages", &[]).unwrap();
        assert_eq!(info.name, "default");
        assert_eq!(path, "/api/anthropic/v1/messages"); // 加上 base_path /api/anthropic
    }
}
