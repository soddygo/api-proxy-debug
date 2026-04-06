CONFIG_FILE ?= config.json
CARGO_FLAGS ?=
LOG_DIR ?= logs

# 检测 Windows 环境
IS_WINDOWS := $(findstring MINGW,$(shell uname -s))$(findstring MSYS,$(shell uname -s))
ifeq ($(IS_WINDOWS),)
# 非 Windows (macOS/Linux)
CMAKE_ENV :=
else
# Windows 环境
CMAKE_ENV := CMAKE_GENERATOR="Visual Studio 17 2022"
endif

.PHONY: build run clean help stats telemetry

build:
	$(CMAKE_ENV) cargo build --release $(CARGO_FLAGS)

run:
	@mkdir -p $(LOG_DIR)
	$(CMAKE_ENV) cargo run --release $(CARGO_FLAGS) -- --config $(CONFIG_FILE)

# 启用 OpenTelemetry
run-telemetry:
	@mkdir -p $(LOG_DIR)
	$(CMAKE_ENV) cargo run --release $(CARGO_FLAGS) -- --config $(CONFIG_FILE) --enable-telemetry

# 查看统计
stats:
	@cat $(LOG_DIR)/stats.json 2>/dev/null || echo "暂无统计数据，请先运行代理"

clean:
	cargo clean
	rm -rf $(LOG_DIR)/*.log

help:
	@echo "用法:"
	@echo "  make run                          启动代理 (使用 config.json)"
	@echo "  make run CONFIG_FILE=xx.json      使用指定配置文件启动"
	@echo "  make run-telemetry                启动代理并启用 OpenTelemetry"
	@echo "  make build                        编译 release"
	@echo "  make stats                        查看统计数据"
	@echo "  make clean                        清理构建产物和日志"
	@echo ""
	@echo "配置示例 (config.json):"
	@echo "  {"
	@echo "    \"listen\": \"0.0.0.0:8989\","
	@echo "    \"backends\": ["
	@echo "      {\"name\": \"anthropic\", \"url\": \"https://api.anthropic.com\", \"api_key\": \"xxx\", \"protocol\": \"anthropic\"}"
	@echo "    ]"
	@echo "  }"
