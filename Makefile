.PHONY: build dev test test-fast test-watch lint fmt fmt-check style audit cov clean install

build:          ## Build release binary
	cargo build --release

dev:            ## Run in development mode
	cargo run

test:           ## Run all tests (parallel runner if installed)
	@if command -v cargo-nextest >/dev/null 2>&1; then \
		cargo nextest run; \
	else \
		cargo test --all-features; \
	fi

test-fast:      ## Run tests with nextest (fast, parallel)
	cargo nextest run

test-watch:     ## Watch and run tests on change
	cargo watch -x test

lint:           ## Run clippy with warnings as errors
	cargo clippy --all-targets --all-features -- -D warnings

fmt:            ## Format all code
	cargo fmt --all

fmt-check:      ## Check formatting (CI)
	cargo fmt --all -- --check

audit:          ## Security audit
	cargo audit

cov:            ## Generate coverage report
	cargo tarpaulin --out Html --out Lcov

clean:          ## Clean build artifacts
	cargo clean

install:        ## Install to cargo bin
	cargo install --path .

style:          ## Check style limits (file lines / indentation)
	./scripts/check-style.sh
