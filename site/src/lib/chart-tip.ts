// The benchmark charts' tooltip: one element for every row on the page, shown on mouse hover, keyboard focus and
// tap, and built with textContent. Each row's data-tip lines are "colour key, series, value", highest first.
export function wireChartTips() {
  const rows = [...document.querySelectorAll<HTMLElement>(".chart-row")];
  if (!rows.length) return;
  const tip = document.createElement("div");
  tip.className = "chart-tip";
  tip.hidden = true;
  document.body.append(tip);
  const hide = () => {
    tip.hidden = true;
    document.querySelector(".chart-row.on")?.classList.remove("on");
  };
  const show = (row: HTMLElement) => {
    tip.replaceChildren();
    const head = document.createElement("div");
    head.className = "chart-tip-name";
    head.textContent = row.dataset.name ?? "";
    tip.append(head);
    for (const [k, series, value] of (row.dataset.tip ?? "").split("\n").map((l) => l.split("\t"))) {
      const line = document.createElement("div");
      line.className = "chart-tip-row";
      const key = document.createElement("span");
      key.className = `chart-tip-key chart-s${k}`;
      const v = document.createElement("strong");
      v.textContent = value;
      const s = document.createElement("span");
      s.textContent = series;
      line.append(key, v, s);
      tip.append(line);
    }
    document.querySelector(".chart-row.on")?.classList.remove("on");
    row.classList.add("on");
    tip.hidden = false;
    const r = row.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    tip.style.left = `${Math.min(innerWidth - t.width - 12, Math.max(12, r.right - t.width))}px`;
    tip.style.top = `${r.top - t.height - 8 < 8 ? r.bottom + 8 : r.top - t.height - 8}px`;
  };
  for (const row of rows) {
    row.addEventListener("pointerenter", (e) => e.pointerType === "mouse" && show(row));
    row.addEventListener("pointerleave", (e) => e.pointerType === "mouse" && hide());
    row.addEventListener("focus", () => show(row));
    row.addEventListener("blur", hide);
    // Touch: a tap shows the row's tooltip, a second tap hides it.
    row.addEventListener("click", () => (row.classList.contains("on") && !tip.hidden ? hide() : show(row)));
  }
  addEventListener("scroll", hide, { passive: true });
}
