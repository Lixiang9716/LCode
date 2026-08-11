.PHONY: build dev test test-watch lint fmt fmt-check audit cov clean install

build:          ## Build release binary
	cargo build --release

dev:            ## Run in development mode
	cargo run

test:           ## Run all tests
	cargo test --all-features

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
