.PHONY: build frontend backend fmt clippy test docker clean

build: frontend backend

frontend:
	cd frontend && npm ci && npm run build

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
	rm -rf frontend/public frontend/node_modules
