CONFIG_FILE ?= config.json
CARGO_FLAGS ?=
LOG_DIR ?= logs

.PHONY: build run clean help

build:
	cargo build --release $(CARGO_FLAGS)

run:
	cargo run --release $(CARGO_FLAGS) -- --config $(CONFIG_FILE) --log-dir $(LOG_DIR)

clean:
	cargo clean

help:
	@echo "用法:"
	@echo "  make run                          启动代理 (日志默认输出到 logs/)"
	@echo "  make run CONFIG_FILE=xx.json      使用指定配置文件启动"
	@echo "  make build                        编译 release"
	@echo "  make clean                        清理构建产物"
