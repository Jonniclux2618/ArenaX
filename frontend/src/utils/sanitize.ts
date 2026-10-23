/**
 * Input sanitization utilities (#706 – XSS Protections)
 *
 * Pure-TypeScript helpers with no runtime dependencies.
 * Use these on any user-supplied data before rendering or persisting it.
 */
import { safeHref } from "@/lib/safeNavigation";

// ─── HTML entity map ──────────────────────────────────────────────────────────
const HTML_ENTITIES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#x27;",
  "/": "&#x2F;",
  "`": "&#x60;",
  "=": "&#x3D;",
};

/**
 * Encodes input as plain text safe to interpolate into an HTML context.
 * Rich user-provided HTML is intentionally not supported.
 */
export function sanitizeHtml(input: string): string {
  if (typeof input !== "string") return "";
  return escapeHtml(input.trim());
}

/**
 * Returns '#' for dangerous URLs (javascript:, data:text/html, vbscript:).
 * Allows http://, https://, relative paths, and anchor-only URLs.
 */
export function sanitizeUrl(url: string): string {
  const safe = safeHref(url);
  return safe ?? "#";
}

/**
 * HTML entity-encodes a string to prevent injection into HTML attributes
 * and text nodes.
 */
export function escapeHtml(input: string): string {
  if (typeof input !== "string") return "";
  return input.replace(/[&<>"'`=/]/g, (char) => HTML_ENTITIES[char] ?? char);
}

/**
 * General-purpose input sanitizer for user-supplied strings:
 * trims whitespace, enforces a maximum length, then HTML-escapes the result.
 *
 * @param input     - raw user input
 * @param maxLength - maximum allowed length (default: 1000)
 */
export function sanitizeInput(input: string, maxLength = 1000): string {
  if (typeof input !== "string") return "";
  const trimmed = input.trim();
  const clamped = trimmed.length > maxLength ? trimmed.slice(0, maxLength) : trimmed;
  return escapeHtml(clamped);
}
