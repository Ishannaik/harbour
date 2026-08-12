# FR-65: local parity with .github/workflows/ci.yml — same commands, same
# order, so a failure never appears first at PR time.
check: fmt lint test
fmt:
	cargo fmt --check
lint:
	cargo clippy --all-targets -- -D warnings
test:
	cargo test
lines:
	awk 'length($0) > 100 {print FILENAME":"FNR": "length($0)}' $(git ls-files '*.rs')
audit:
	cargo audit
