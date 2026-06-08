/**
 * Safe DOM helpers to avoid innerHTML XSS.
 */

/**
 * Renders an error state into a container element.
 * Uses textContent (not innerHTML) to prevent XSS from error messages.
 */
export function renderError(
  container: HTMLElement,
  title: string,
  message: string,
): void {
  container.textContent = "";
  const h2 = document.createElement("h2");
  h2.textContent = title;
  const p = document.createElement("p");
  p.className = "error-text";
  p.textContent = message;
  container.append(h2, p);
}

/**
 * Escapes a string for safe insertion into HTML.
 * Use this when building innerHTML with dynamic data.
 */
export function escapeHtml(str: string): string {
  const div = document.createElement("div");
  div.textContent = str;
  return div.innerHTML;
}
