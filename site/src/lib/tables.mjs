/** A Sätteri hast plugin for Markdown: every table goes in a `<div class="table">` that scrolls sideways,
 * so the table itself keeps table layout and fills the card. The marker stops the wrapped table from
 * being wrapped again if the visitor walks into what it returned. */
export const tablesPlugin = {
  name: "tables",
  element: {
    filter: ["table"],
    visit(node) {
      if (node.properties?.dataWrapped !== undefined) return;
      return {
        type: "element",
        tagName: "div",
        properties: { className: ["table"] },
        children: [{ ...node, properties: { ...node.properties, dataWrapped: "" } }],
      };
    },
  },
};
