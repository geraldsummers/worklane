.PHONY: test coverage

test:
	cargo test --workspace

coverage:
	cargo llvm-cov --workspace --fail-under-lines 90
