/**
 * Wallet integration for the public KYC/KYB submission route.
 *
 * Isolation contract — this module:
 *   - Holds the connected address in MODULE-LOCAL state. No localStorage,
 *     sessionStorage, IndexedDB, or cookies for any artifact (address,
 *     signed challenge, derived data).
 *   - Never reads or writes the operator-auth keys (provider_admin_address,
 *     console_token). Existing operator sessions on other routes are
 *     unaffected by KYC-route activity, and vice versa.
 *   - Refresh / navigation away purges all state. The user must reconnect.
 *
 * The Stellar Wallets Kit is a static SDK shared with operator-auth code, but
 * the kit's own internal state (selected wallet, network) is ephemeral and
 * does not persist KYC-relevant data. We call its `authModal()` and
 * `signMessage()` directly; we do NOT call `setWallet()` (no caching).
 */
import { StellarWalletsKit } from "@creit-tech/stellar-wallets-kit/sdk";
import { Networks } from "@creit-tech/stellar-wallets-kit/types";
import { FreighterModule } from "@creit-tech/stellar-wallets-kit/modules/freighter";
import { STELLAR_NETWORK } from "./config.ts";

let kitInitialized = false;
let kycAddress: string | null = null;

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

function getNetworkPassphrase(): string {
  switch (STELLAR_NETWORK) {
    case "mainnet":
      return "Public Global Stellar Network ; September 2015";
    case "standalone":
      return "Standalone Network ; February 2017";
    default:
      return "Test SDF Network ; September 2015";
  }
}

function ensureKitInit(): void {
  if (kitInitialized) return;
  StellarWalletsKit.init({
    modules: [new FreighterModule()],
    network: getWalletNetwork(),
  });
  kitInitialized = true;
}

export function getKycAddress(): string | null {
  return kycAddress;
}

export function clearKycWallet(): void {
  kycAddress = null;
}

/**
 * Opens the wallet modal and stores the chosen address in module-local state.
 */
export async function connectKycWallet(): Promise<string> {
  ensureKitInit();
  const { address } = await StellarWalletsKit.authModal();
  if (!address) throw new Error("Wallet connect cancelled");
  kycAddress = address;
  return address;
}

/**
 * Signs the given challenge message (SEP-53) with the connected wallet.
 * Returns the wallet's signedMessage string verbatim — the server side
 * accepts hex or base64 and tries SEP-43, SEP-53, and raw fallbacks.
 */
export async function signKycMessage(message: string): Promise<string> {
  ensureKitInit();
  if (!kycAddress) throw new Error("Wallet not connected");
  const result = await StellarWalletsKit.signMessage(message, {
    address: kycAddress,
    networkPassphrase: getNetworkPassphrase(),
  });
  if (
    typeof result?.signedMessage !== "string" ||
    result.signedMessage.length === 0
  ) {
    throw new Error("Wallet returned an empty signature");
  }
  return result.signedMessage;
}
