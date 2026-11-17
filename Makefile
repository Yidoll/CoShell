PNPM ?= corepack pnpm

.PHONY: install dev dev-web dev-backend dev-tauri build check test architecture cargo-check cargo-test

install:
	$(PNPM) install

dev:
	$(PNPM) dev

dev-web:
	$(PNPM) dev:web

dev-backend:
	cargo run -p coshell-web

dev-tauri:
	$(PNPM) dev:tauri

build:
	$(PNPM) build

check: architecture
	$(PNPM) typecheck
	cargo check --workspace

test:
	$(PNPM) test
	cargo test --workspace

architecture:
	$(PNPM) architecture

cargo-check:
	cargo check --workspace

cargo-test:
	cargo test --workspace
