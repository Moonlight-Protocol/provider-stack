/**
 * Wallet integration for the entity payment surface (#/pay-utxo).
 *
 * Isolation contract — same stance as wallet-kyc.ts: the connected address
 * lives in MODULE-LOCAL state only. No localStorage, sessionStorage,
 * IndexedDB, or cookies. This module never reads or writes the operator-auth
 * keys (provider_admin_address, console_token). Refresh or navigation away
 * purges all state — the entity reconnects and re-signs SEP-10 every visit.
 */
import { StellarWalletsKit } from "@creit-tech/stellar-wallets-kit/sdk";
import { Networks } from "@creit-tech/stellar-wallets-kit/types";
import { FreighterModule } from "@creit-tech/stellar-wallets-kit/modules/freighter";
import { STELLAR_NETWORK } from "./config.ts";
import { getNetworkPassphrase } from "./wallet.ts";

let initialized = false;
let entityAddress: string | null = null;

function getWalletNetwork(): Networks {
  switch (STELLAR_NETWORK) {
    case "mainnet":
      return Networks.PUBLIC;
    case "standalone":
      return Networks.STANDALONE;
    default:
      return Networks.TESTNET;
  }
}

function ensureInit(): void {
  if (!initialized) {
    StellarWalletsKit.init({
      modules: [new FreighterModule()],
      network: getWalletNetwork(),
    });
    initialized = true;
  }
  if (entityAddress) {
    StellarWalletsKit.setWallet("freighter");
  }
}

export function getEntityAddress(): string | null {
  return entityAddress;
}

export function isEntityWalletConnected(): boolean {
  return !!entityAddress;
}

export function clearEntityWallet(): void {
  entityAddress = null;
}

/**
 * Open the wallet modal, connect, and hold the address in module-local
 * state. Returns the public key.
 */
export async function connectEntityWallet(): Promise<string> {
  ensureInit();
  const { address } = await StellarWalletsKit.authModal();
  if (!address) throw new Error("Wallet connect cancelled");
  entityAddress = address;
  return address;
}

/**
 * Sign a transaction XDR with the entity's connected wallet.
 * Used to co-sign the SEP-10 challenge.
 */
export async function signEntityTransaction(xdr: string): Promise<string> {
  ensureInit();
  const address = entityAddress;
  if (!address) throw new Error("Wallet not connected");

  const { signedTxXdr } = await StellarWalletsKit.signTransaction(xdr, {
    address,
    networkPassphrase: getNetworkPassphrase(),
  });

  if (typeof signedTxXdr !== "string" || signedTxXdr.length === 0) {
    throw new Error(
      "The wallet returned no signature. Check that Freighter is on the " +
        "right network and the request was approved.",
    );
  }
  return signedTxXdr;
}

/**
 * Sign an arbitrary message with the entity's wallet (SEP-53).
 * Used to derive the UTXO master seed — same message and mechanism as
 * moonlight-pay's initMasterSeed.
 */
export async function signEntityMessage(message: string): Promise<string> {
  ensureInit();
  const address = entityAddress;
  if (!address) throw new Error("Wallet not connected");

  const result = await StellarWalletsKit.signMessage(message, {
    address,
    networkPassphrase: getNetworkPassphrase(),
  });
  if (
    typeof result?.signedMessage !== "string" ||
    result.signedMessage.length === 0
  ) {
    throw new Error("Wallet returned an empty message signature");
  }
  // Derived keys depend on WHO signed: a signature from any other account
  // yields a different key set and silently orphans the user's funds. Fail
  // loudly instead (Freighter reports the signer; some versions sign with
  // the active account rather than the requested address).
  if (result.signerAddress && result.signerAddress !== address) {
    throw new Error(
      `The wallet signed with ${result.signerAddress} instead of ${address}. ` +
        "Switch the wallet's active account and retry.",
    );
  }
  return result.signedMessage;
}

/**
 * Sign a Soroban authorization entry (base64 XDR) with the entity's wallet.
 * Used to authorize channel deposits.
 */
export async function signEntityAuthEntry(
  authEntryB64: string,
  networkPassphrase: string,
): Promise<string> {
  ensureInit();
  const address = entityAddress;
  if (!address) throw new Error("Wallet not connected");

  const { signedAuthEntry } = await StellarWalletsKit.signAuthEntry(
    authEntryB64,
    { address, networkPassphrase },
  );
  if (!signedAuthEntry) {
    throw new Error("Wallet returned an empty auth entry signature");
  }
  return signedAuthEntry;
}
