.PHONY: build frontend backend fmt clippy test docker clean

build: frontend backend

frontend:
	cd frontend && deno task build

backend:
	cargo build --release

fmt:
	cargo fmt --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

docker:
	docker buildx build --platform linux/amd64,linux/arm64 -t provider-stack:dev .

clean:
	cargo clean
	rm -rf frontend/public/app.js frontend/public/styles.css frontend/public/health.json frontend/node_modules
