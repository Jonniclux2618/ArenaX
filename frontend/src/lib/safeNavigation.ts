const APP_ORIGIN = "https://arenax.invalid";
const CONTROL_OR_BACKSLASH = /[\u0000-\u001f\u007f\\]/;

/** Accept an absolute HTTP(S) URL or a same-origin relative URL. */
export function safeHref(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const href = value.trim();
  if (!href || CONTROL_OR_BACKSLASH.test(href) || href.startsWith("//")) {
    return null;
  }

  try {
    const parsed = new URL(href, APP_ORIGIN);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
    if (parsed.origin !== APP_ORIGIN && !/^https?:\/\//i.test(href)) return null;
    return href;
  } catch {
    return null;
  }
}

/** Restrict post-auth redirects to app-local absolute paths. */
export function safeRedirectPath(value: unknown, fallback = "/"): string {
  if (
    typeof value !== "string" ||
    !value.startsWith("/") ||
    value.startsWith("//") ||
    value.startsWith("/\\") ||
    CONTROL_OR_BACKSLASH.test(value)
  ) {
    return fallback;
  }

  try {
    const parsed = new URL(value, APP_ORIGIN);
    if (parsed.origin !== APP_ORIGIN || parsed.username || parsed.password) return fallback;
    return `${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    return fallback;
  }
}
