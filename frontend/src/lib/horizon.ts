/**
 * Horizon account lookup.
 * Returns true if the account has ever been funded (exists on-chain).
 * Returns false if the account has never existed (404).
 */
import { HORIZON_URL } from "./config.ts";

export async function accountExists(publicKey: string): Promise<boolean> {
  try {
    const res = await fetch(`${HORIZON_URL}/accounts/${publicKey}`);
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * The account's native (XLM) balance in stroops; 0 when the account does
 * not exist or Horizon is unreachable.
 */
export async function getNativeBalance(publicKey: string): Promise<bigint> {
  try {
    const res = await fetch(`${HORIZON_URL}/accounts/${publicKey}`);
    if (!res.ok) return 0n;
    const account = await res.json();
    const native = (account.balances as Array<Record<string, string>> ?? [])
      .find((b) => b.asset_type === "native");
    if (!native?.balance) return 0n;
    const [whole, frac = ""] = native.balance.split(".");
    return BigInt(whole) * 10_000_000n +
      BigInt((frac + "0000000").slice(0, 7));
  } catch {
    return 0n;
  }
}
