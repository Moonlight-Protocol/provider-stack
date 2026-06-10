# Test-suite route-surface audit

Cross-check of every PROVIDER_URL endpoint touched by:

- `local-dev/testnet/main.ts`
- `local-dev/lifecycle/testnet-verify.ts`
- `local-dev/testnet/events-capture/{harness,subscribe}.ts`
- `local-dev/lib/client/{auth,bundle,deposit,send,withdraw,receive,register-entity}.ts`

| # | Path | Status in Rust stack | Compat work needed |
|---|---|---|---|
| 1 | `GET /api/v1/health` | ✅ exists | none |
| 2 | `POST /api/v1/dashboard/auth/challenge` | ✅ exists | rename request fields → camelCase (`publicKey`); response shape: `{ data: { nonce } }` |
| 3 | `POST /api/v1/dashboard/auth/verify` | ✅ exists | request: `{ publicKey, nonce, signature }`; response: `{ data: { token } }` |
| 4 | `POST /api/v1/dashboard/pp/register` | ❌ missing | **compat shim**: accept `{ secretKey, derivationIndex?, label? }`; validate that the derived public key matches `OPERATOR_PUBLIC_KEY` env or `PP_PUBLIC_KEY`; return `200 { data: { id, publicKey } }` |
| 5 | `GET /api/v1/dashboard/pp/list` | ❌ missing | **compat shim**: return `{ data: [{ id, publicKey, label, isActive }] }` for the single configured PP |
| 6 | `GET /api/v1/stellar/auth?account=G...` | ✅ exists | response shape: `{ data: { challenge: <base64 XDR>, networkPassphrase } }` (test reads `data.data.challenge`) |
| 7 | `POST /api/v1/stellar/auth` | ✅ exists | request: `{ transaction }`; response: `{ data: { token } }` |
| 8 | `POST /api/v1/providers/:pk/entities/challenge` | ✅ exists | response: `{ data: { nonce } }`; request key check — confirm `{ pubkey }` |
| 9 | `POST /api/v1/providers/:pk/entities` | ✅ exists | request: `{ pubkey, name, jurisdictions, signedChallenge }`; response: `{ data: { entityId, status } }` |
| 10 | `POST /api/v1/providers/:pk/council/join` | ✅ exists | request body uses camelCase: `{ councilUrl, councilId, councilName, label, contactEmail, signedEnvelope }`; response: wrapped in `{ data }` |
| 11 | `GET /api/v1/providers/:pk/council/membership` | ✅ exists | response: `{ data: { status, ... } }` (the test polls until `data.status === "ACTIVE"`) |
| 12 | `POST /api/v1/providers/:pk/entity/bundles` | ✅ exists (real fee calc) | request: `{ operationsMLXDR, channelContractId }`; response: `{ data: { operationsBundleId } }` |
| 13 | `GET /api/v1/providers/:pk/entity/bundles/:bundleId` | ✅ exists | response: `{ data: { status, ... } }` |
| 14 | `GET /api/v1/providers/:pk/events/ws` | ✅ exists (heartbeat + subprotocol) | none |

## Cross-cutting

- **All request/response JSON keys** are camelCase in the Deno reference; current Rust handlers use snake_case. Going to apply `#[serde(rename_all = "camelCase")]` to every wire struct.
- **All success responses** are wrapped in `{ "data": ... }`. Current Rust returns the payload directly.
- **Errors** in Deno are `{ "error": <kind>, "message": <text> }` — matches current Rust.

## Order

1. Wire-format compat: serde camelCase + `{ data: ... }` wrapping (touches every handler).
2. PP register / list compat shims (single-PP semantics; internally constant).
3. Update integration tests to match the new wire shape.
4. Live local-dev stand-up.
