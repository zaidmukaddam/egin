// Code highlighting with @pierre/highlights, at build time. Each token gets `color: light-dark(light, dark)`,
// so code follows the page's color-scheme with no extra CSS.
import { codeToHtml, isSupportedLanguage } from "@pierre/highlights";
import { pierreDark, pierreLight } from "@pierre/highlights/themes";

const decoder = new TextDecoder();

/** Highlighted `<pre>` HTML for `code`. A language it doesn't know is shown as plain text. */
export function highlight(code, lang = "text") {
  const html = codeToHtml(code.replace(/\n$/, ""), {
    lang: isSupportedLanguage(lang) ? lang : "text",
    themes: { light: pierreLight, dark: pierreDark },
    defaultColor: "light-dark()",
  });
  // The bytes are a view into WebAssembly memory, valid only until the next call.
  return decoder.decode(html);
}

/** A Sätteri hast plugin for Markdown: every fenced code block goes through `highlight`. */
export const highlightsPlugin = {
  name: "highlights",
  element: {
    filter: ["pre"],
    visit(node, ctx) {
      const code = node.children?.find((c) => c.type === "element" && c.tagName === "code");
      if (!code) return;
      const className = Array.isArray(code.properties?.className) ? code.properties.className : [];
      const lang = code.data?.lang ?? className.find((c) => String(c).startsWith("language-"))?.slice(9);
      // A raw node is written out as is.
      return { type: "raw", value: highlight(ctx.textContent(code), lang) };
    },
  },
};
