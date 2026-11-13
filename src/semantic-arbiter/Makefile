.PHONY: build check fmt-check lint test doc

build:
	cargo build --locked

check: fmt-check lint test doc

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
