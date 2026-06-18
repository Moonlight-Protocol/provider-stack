/**
 * Wallet integration and auth state.
 * Uses stellar-wallets-kit v2 (static API).
 */
import { StellarWalletsKit } from "@creit-tech/stellar-wallets-kit/sdk";
import { Networks } from "@creit-tech/stellar-wallets-kit/types";
import { FreighterModule } from "@creit-tech/stellar-wallets-kit/modules/freighter";
import { STELLAR_NETWORK } from "./config.ts";

const STORAGE_KEY = "provider_admin_address";

let initialized = false;
let connectedAddress: string | null = null;

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
  if (getConnectedAddress()) {
    StellarWalletsKit.setWallet("freighter");
  }
}

export function getConnectedAddress(): string | null {
  if (!connectedAddress) {
    connectedAddress = localStorage.getItem(STORAGE_KEY);
  }
  return connectedAddress;
}

export function isWalletConnected(): boolean {
  return !!getConnectedAddress();
}

export function clearSession(): void {
  connectedAddress = null;
  localStorage.removeItem(STORAGE_KEY);
}

export function getNetworkPassphrase(): string {
  switch (STELLAR_NETWORK) {
    case "mainnet":
      return "Public Global Stellar Network ; September 2015";
    case "standalone":
      return "Standalone Network ; February 2017";
    default:
      return "Test SDF Network ; September 2015";
  }
}

/**
 * Open wallet modal, connect, and store the address.
 * Returns the public key.
 */
export async function connectWallet(): Promise<string> {
  ensureInit();
  const { address } = await StellarWalletsKit.authModal();
  connectedAddress = address;
  localStorage.setItem(STORAGE_KEY, address);
  return address;
}

/**
 * Sign an arbitrary message with the connected wallet (SEP-53).
 * Used for challenge-response authentication with the provider platform.
 */
export async function signMessage(message: string): Promise<string> {
  ensureInit();
  const address = getConnectedAddress();
  if (!address) throw new Error("Wallet not connected");

  const result = await StellarWalletsKit.signMessage(message, {
    address,
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

/**
 * Sign a transaction XDR with the connected wallet.
 */
export async function signTransaction(xdr: string): Promise<string> {
  ensureInit();
  const address = getConnectedAddress();
  if (!address) throw new Error("No wallet connected");

  const { signedTxXdr } = await StellarWalletsKit.signTransaction(xdr, {
    address,
    networkPassphrase: getNetworkPassphrase(),
  });

  return signedTxXdr;
}
