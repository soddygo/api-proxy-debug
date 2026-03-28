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
CMAKE_ENV := CMAKE_GENERATOR=Visual Studio 17 2022
endif

.PHONY: build run clean help

build:
	$(CMAKE_ENV) cargo build --release $(CARGO_FLAGS)

run:
	$(CMAKE_ENV) cargo run --release $(CARGO_FLAGS) -- --config $(CONFIG_FILE) --log-dir $(LOG_DIR)

clean:
	cargo clean

help:
	@echo "用法:"
	@echo "  make run                          启动代理 (日志默认输出到 logs/)"
	@echo "  make run CONFIG_FILE=xx.json      使用指定配置文件启动"
	@echo "  make build                        编译 release"
	@echo "  make clean                        清理构建产物"
