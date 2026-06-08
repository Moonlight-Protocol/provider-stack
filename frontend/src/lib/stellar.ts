/**
 * Stellar helpers for the provider console.
 * Account balance checks, fund transactions, and Horizon submission.
 */
import { HORIZON_URL, STELLAR_NETWORK } from "./config.ts";

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

export async function getAccountBalance(
  publicKey: string,
): Promise<{ xlm: string; funded: boolean }> {
  try {
    const res = await fetch(`${HORIZON_URL}/accounts/${publicKey}`);
    if (res.status === 404) return { xlm: "0", funded: false };
    if (!res.ok) return { xlm: "0", funded: false };
    const data = await res.json();
    const native = data.balances?.find(
      (b: { asset_type: string; balance: string }) => b.asset_type === "native",
    );
    return { xlm: native?.balance ?? "0", funded: true };
  } catch {
    return { xlm: "0", funded: false };
  }
}

export async function buildFundTx(
  sourcePublicKey: string,
  destinationPublicKey: string,
  amountXlm: string,
): Promise<string> {
  const sdk = await import("stellar-sdk");
  const { TransactionBuilder, Operation, Asset, Horizon } = sdk;
  const allowHttp = HORIZON_URL.startsWith("http://");
  const server = new Horizon.Server(HORIZON_URL, { allowHttp });
  const account = await server.loadAccount(sourcePublicKey);

  const { funded } = await getAccountBalance(destinationPublicKey);

  const op = funded
    ? Operation.payment({
      destination: destinationPublicKey,
      asset: Asset.native(),
      amount: amountXlm,
    })
    : Operation.createAccount({
      destination: destinationPublicKey,
      startingBalance: amountXlm,
    });

  const tx = new TransactionBuilder(account, {
    fee: "100000",
    networkPassphrase: getNetworkPassphrase(),
  })
    .addOperation(op)
    .setTimeout(30)
    .build();

  return tx.toXDR();
}

export async function submitHorizonTx(signedXdr: string): Promise<void> {
  const res = await fetch(`${HORIZON_URL}/transactions`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: `tx=${encodeURIComponent(signedXdr)}`,
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({}));
    throw new Error(
      err.extras?.result_codes?.operations?.[0] || err.title ||
        `Transaction failed: ${res.status}`,
    );
  }
}
