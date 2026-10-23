const RETURN_PATH_BASE = "https://arenax.invalid";

export function getSafeReturnPath(
  candidate: string | null | undefined,
  fallback = "/",
): string {
  if (
    !candidate ||
    !candidate.startsWith("/") ||
    candidate.startsWith("//") ||
    candidate.includes("\\") ||
    /[\u0000-\u001f]/.test(candidate)
  ) {
    return fallback;
  }

  try {
    const url = new URL(candidate, RETURN_PATH_BASE);
    if (url.origin !== RETURN_PATH_BASE) return fallback;
    return `${url.pathname}${url.search}${url.hash}`;
  } catch {
    return fallback;
  }
}