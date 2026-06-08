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
