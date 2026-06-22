.PHONY: build frontend backend fmt clippy test docker compose-up clean

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

# Self-host path: app + Postgres via compose. Seeds .env from the example on
# first run so `make compose-up` works from a clean checkout.
compose-up:
	@test -f .env || cp .env.example .env
	docker compose up --build

clean:
	cargo clean
	rm -rf frontend/public/app.js frontend/public/styles.css frontend/public/health.json frontend/node_modules
