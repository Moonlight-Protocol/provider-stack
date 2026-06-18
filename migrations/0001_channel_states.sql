-- UC6 — asset-lifecycle: per-(privacy)channel disabled flag.
--
-- The contract holds no channel state; ChannelStateChanged events on
-- channel-auth are the only on-chain artifact. This row tracks the local
-- view: did the council disable this privacy-channel? The bundle-submit
-- gate consults it to enforce withdraw-only.
--
-- Keyed by the PRIVACY-CHANNEL contract id (the asset channel), not by
-- channel-auth: a single channel-auth can host multiple asset channels.

CREATE TABLE channel_states (
    channel_contract_id TEXT PRIMARY KEY,
    is_disabled         BOOLEAN NOT NULL DEFAULT FALSE,
    last_event_ledger   BIGINT,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
