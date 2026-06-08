/**
 * Provider setup step definitions and draft helpers.
 */

export const SETUP_STEPS = [
  { id: "metadata", label: "Provider Info" },
  { id: "fund", label: "Fund Account" },
  { id: "join", label: "Join Council" },
] as const;

export type SetupStepId = typeof SETUP_STEPS[number]["id"];

export function saveFormDraft(
  step: string,
  data: Record<string, unknown>,
): void {
  sessionStorage.setItem(`setup_draft_${step}`, JSON.stringify(data));
}

export function getFormDraft(step: string): Record<string, unknown> | null {
  try {
    const raw = sessionStorage.getItem(`setup_draft_${step}`);
    return raw ? JSON.parse(raw) : null;
  } catch {
    return null;
  }
}

export function clearFormDraft(step: string): void {
  sessionStorage.removeItem(`setup_draft_${step}`);
}

export function clearAllDrafts(): void {
  for (const step of SETUP_STEPS) {
    sessionStorage.removeItem(`setup_draft_${step.id}`);
  }
  sessionStorage.removeItem("setup_pp_index");
  sessionStorage.removeItem("setup_pp_publickey");
}
