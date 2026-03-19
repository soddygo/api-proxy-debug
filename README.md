# API Proxy Debug

本地 HTTP 反向代理工具，用于拦截和记录 AI 模型 API 的所有请求与响应，方便抓包分析和问题排查。

## 工作原理

```
Claude Code ──HTTP──▶ localhost:9180 (本代理) ──HTTPS──▶ api.anthropic.com
                           │
                     日志输出 (终端 + 文件)
                     ├─ 请求 method/URI/headers/body
                     ├─ 实际上游请求 (URI 重写 + API Key 注入后)
                     ├─ 上游连接信息 (TLS 版本/连接复用)
                     ├─ 响应 status/headers/body (支持 SSE streaming)
                     └─ 错误信息 (连接失败/超时/TLS 握手失败)
```

代理本身监听 HTTP 明文，上游连接走 HTTPS（自动根据 `backend_url` 的 scheme 判断），实现「明文抓包 + 安全传输」。

## 快速开始

### 1. 创建配置文件

```bash
cp config.example.json config.json
```

编辑 `config.json`，填入真实的后端地址和 API Key：

```json
{
  "listen": "0.0.0.0:9180",
  "backend_url": "https://api.anthropic.com",
  "api_key": "sk-ant-your-api-key",
  "api_protocol": "anthropic"
}
```

> `config.json` 包含敏感信息，已在 `.gitignore` 中排除，不会被提交。

### 2. 启动代理

```bash
make run
```

### 3. 配置 Claude Code 指向代理

```bash
ANTHROPIC_BASE_URL=http://127.0.0.1:9180 claude
```

## 配置说明

所有配置项均可通过 JSON 配置文件或 CLI 参数指定，CLI 参数优先级更高。

| 配置项 | CLI 参数 | 默认值 | 说明 |
|--------|---------|--------|------|
| `listen` | `--listen` / `-l` | `0.0.0.0:8080` | 代理监听地址 |
| `backend_url` | `--backend-url` / `-b` | **必填** | 后端 API 地址 |
| `api_key` | `--api-key` / `-k` | **必填** | 注入到请求中的 API Key |
| `api_protocol` | `--api-protocol` / `-p` | `anthropic` | 认证协议：`anthropic` 或 `openai` |
| `no_log_body` | `--no-log-body` | `false` | 关闭 body 日志 |
| `no_log_headers` | `--no-log-headers` | `false` | 关闭 headers 日志 |
| `log_dir` | `--log-dir` | - | 日志输出目录（`make run` 默认为 `logs/`） |

### 认证协议

- **anthropic**：通过 `x-api-key` header 注入 API Key（Anthropic 官方 API）
- **openai**：通过 `Authorization: Bearer <key>` header 注入（OpenAI 兼容 API）

## 日志输出

日志同时输出到终端和文件（`logs/proxy-YYYY-MM-DD.log`），按天滚动。

### 日志示例

```
════════════════════════════════════════════════════════════════════════════════
  REQUEST  [10:30:01.123]  POST /v1/messages
════════════════════════════════════════════════════════════════════════════════
  Headers:
    content-type: application/json
    x-api-key: sk-ant***xxxx
  Body:
    {
      "model": "claude-sonnet-4-20250514",
      "max_tokens": 1024,
      "messages": [...]
    }
  UPSTREAM  POST /api/proxy/model/v1/messages
  Upstream Headers:
    host: api.anthropic.com
    x-api-key: sk-ant***xxxx
  CONNECT  api.anthropic.com -> 1.2.3.4:443 (TLS=true, reused=false, tls_version=TLSv1.3)
────────────────────────────────────────────────────────────────────────────────
  RESPONSE  [10:30:02.456]  Status: 200
────────────────────────────────────────────────────────────────────────────────
  Headers:
    content-type: text/event-stream
  Body (streaming):
    event: message_start
    data: {"type":"message_start",...}
    ...
────────────────────────────────────────────────────────────────────────────────
  DONE  耗时: 1333ms
════════════════════════════════════════════════════════════════════════════════
```

错误场景会以 `ERROR` 标记输出：

```
!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
  ERROR  [10:30:01.123]  请求失败 [POST /v1/messages] -> 502: ConnectTimedout
!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
```

## Make 命令

```bash
make run                        # 启动代理 (日志默认输出到 logs/)
make run CONFIG_FILE=xx.json    # 使用指定配置文件启动
make build                      # 编译 release
make clean                      # 清理构建产物
```

## 技术栈

- [Pingora](https://github.com/cloudflare/pingora) 0.8 - Cloudflare 开源的 Rust 反向代理框架
- rustls - TLS 实现
- clap - CLI 参数解析
- tracing - 结构化日志

## License

MIT
