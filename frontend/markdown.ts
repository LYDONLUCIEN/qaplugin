import DOMPurify from "dompurify";
import { marked } from "marked";

export function renderMarkdown(target: HTMLElement, source: string) {
  const html = marked.parse(source || "", {
    async: false,
    breaks: true,
    gfm: true,
  }) as string;
  target.innerHTML = DOMPurify.sanitize(html, { USE_PROFILES: { html: true } });
  for (const link of target.querySelectorAll<HTMLAnchorElement>("a[href]")) {
    link.target = "_blank";
    link.rel = "noopener noreferrer";
  }
}
