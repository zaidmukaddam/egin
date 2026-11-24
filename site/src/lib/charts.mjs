// Benchmark charts, drawn at build time from a ```chart block in the docs Markdown. The block holds JSON:
//   { "title": "Terminal-Bench 2", "tasks": 87, "series": ["egin", "eve"], "rows": [["glm-5.3", 65, 55], ...] }
// With `tasks` the bars are pass rates (0–100%) and each value reads "74.7% 65/87"; with `"money": true` they are
// dollars from $0. egin is the first series and the only one in lime. Series are named on the chart's first row,
// not in a legend, and the Markdown table under the chart stays as the table view.

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

/** Clean tick values from 0 up to `max`: steps of 1, 2 or 5 times a power of ten, at most five of them. */
function ticks(max) {
  const pow = 10 ** Math.floor(Math.log10(max / 4));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * pow).find((s) => max / s <= 5);
  const out = [];
  for (let v = 0; v <= max + 1e-9; v += step) out.push(Math.round(v * 1000) / 1000);
  return out;
}

/** Dollars at the precision the measurement has: $0.42 under $1, $3.1 under $10, $12 above. */
const dollars = (v) => `$${v < 1 ? v.toFixed(2) : v < 10 ? v.toFixed(1) : Math.round(v).toLocaleString("en-US")}`;

export function renderChart(spec) {
  // `of` gives each row its own task count, for rows that are different benchmarks; it implies pass rates.
  const { title, series, rows, money = false, of } = spec;
  const rates = Boolean(spec.tasks || of);
  const single = series.length === 1;
  const top = rates ? 100 : Math.max(...rows.flatMap((r) => r.slice(1).filter((v) => typeof v === "number")));
  const tickPct = (t) => `${((t / top) * 100).toFixed(2)}%`;

  const body = rows
    .map(([name, ...vals], r) => {
      const tasks = of?.[r] ?? spec.tasks;
      const pct = (v) => `${(((rates ? (v / tasks) * 100 : v) / top) * 100).toFixed(2)}%`;
      const shown = (v) => (rates ? `${((v / tasks) * 100).toFixed(1)}%` : dollars(v));
      const full = (v) => (rates ? `${shown(v)} ${v.toLocaleString("en-US")}/${tasks.toLocaleString("en-US")}` : dollars(v));
      const mine = single ? name === spec.highlight : true;
      const keys = vals.map((_, i) => (single ? (mine ? 0 : 1) : Math.min(i, 2)));
      const bars = vals
        .map((v, i) => {
          if (v == null) return `<div class="chart-bar-row"><span class="chart-none">not run</span></div>`;
          const lead = keys[i] === 0;
          // The series is named once, on the first row, right after its value.
          const who = !single && r === 0 ? `<span class="chart-who">${esc(series[i])}</span>` : "";
          const count = rates ? `<span class="chart-n">${v.toLocaleString("en-US")}/${tasks.toLocaleString("en-US")}</span>` : "";
          return `<div class="chart-bar-row"><span class="chart-bar chart-s${keys[i]}" style="--w:${pct(v)}"></span><span class="chart-val${lead ? " lead" : ""}">${shown(v)}${count}${who}</span></div>`;
        })
        .join("");
      // The tooltip lists every series, highest first (cheapest first for cost).
      const order = vals
        .map((v, i) => ({ v, i }))
        .filter((x) => x.v != null)
        .sort((a, b) => (money ? a.v - b.v : b.v - a.v));
      const tip = order.map(({ v, i }) => `${keys[i]}\t${single ? name : series[i]}\t${full(v)}`).join("\n");
      const said = `${name}: ${vals.map((v, i) => `${single ? "" : `${series[i]} `}${v == null ? "not run" : full(v)}`).join(", ")}`;
      return `<div class="chart-row${single && mine ? " mine" : ""}" tabindex="0" aria-label="${esc(said)}" data-name="${esc(name)}" data-tip="${esc(tip)}"><span class="chart-name" aria-hidden="true">${esc(name)}</span><div class="chart-bars" aria-hidden="true">${bars}</div></div>`;
    })
    .join("");

  const grid = ticks(top);
  const lines = grid.map((t) => `<span class="chart-grid${t === 0 ? " zero" : ""}" style="--x:${tickPct(t)}"></span>`).join("");
  const axis = grid.map((t) => `<span class="chart-tick" style="--x:${tickPct(t)}">${money ? dollars(t) : t}</span>`).join("");
  const axisLabel = rates ? "pass rate, %" : "cost";
  const sub = spec.unit ?? (spec.tasks ? `${spec.tasks.toLocaleString("en-US")} tasks` : "");
  // A chart inside a card leaves its title to the card's header.
  const head = title ? `<figcaption class="chart-head"><span class="chart-title">${esc(title)}</span>${sub ? `<span class="chart-sub">${esc(sub)}</span>` : ""}</figcaption>` : "";
  return `<figure class="chart" data-chart>${head}<div class="chart-plot"><div class="chart-gridlines" aria-hidden="true">${lines}</div>${body}<div class="chart-axis" aria-hidden="true">${axis}<span class="chart-axis-label">${axisLabel}</span></div></div></figure>`;
}

/** A Sätteri hast plugin: a fenced block tagged `chart` becomes a chart; it runs before code highlighting. */
export const chartsPlugin = {
  name: "charts",
  element: {
    filter: ["pre"],
    visit(node, ctx) {
      const code = node.children?.find((c) => c.type === "element" && c.tagName === "code");
      if (!code) return;
      const className = Array.isArray(code.properties?.className) ? code.properties.className : [];
      const lang = code.data?.lang ?? className.find((c) => String(c).startsWith("language-"))?.slice(9);
      if (lang !== "chart") return;
      return { type: "raw", value: renderChart(JSON.parse(ctx.textContent(code))) };
    },
  },
};
