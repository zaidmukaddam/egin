---
description: A set of search pages built from one template and a table of facts, such as "<product> vs <competitor>" or "<product> for <use case>"
---
Programmatic SEO is one page pattern filled in from a table, one page per row. It works when each row carries facts a searcher wants and can't get from the other pages. When a page would differ from its neighbors only by the name swapped in, search engines count it as thin content and it hurts the whole site. Make fewer pages before you make thin ones.

1. Pick the pattern: a head term plus a modifier people search for, such as `<product> vs <competitor>`, `<product> for <use case>`, `<product> <integration> integration` or `best <category> for <audience>`. Take the modifiers from the brief and `brand.md`'s "Instead of", then check with `web_search` what already ranks for two or three of them and what those pages answer.
2. Build the table in `drafts/seo/<pattern>/data.md`: one Markdown table row per page, with the slug, the modifier, and the facts only that page has (prices, limits, setup steps, what it replaces). Put a source link in each row. Drop any row you can't fill with real facts, and say which ones you dropped.
3. Write the template in `drafts/seo/<pattern>/template.md`, with `{column}` slots. It answers the search in its first two sentences. Every section has to change from row to row, and boilerplate that never changes goes in at most one short section.
4. Write three sample pages, `drafts/seo/<pattern>/<slug>.md`, from the rows with the most facts, and stop there. Hand back the table, the template and the samples so the person can check them before the rest are made. Write the rest only when the brief says the samples were approved.
5. With the whole set written, add `drafts/seo/<pattern>/index.md`: a hub page that links every page, so each one is a click from somewhere a crawler can find.

Each page starts with front matter:

```
---
title: Under 60 characters, the search phrase first
description: Under 155 characters, the answer and who it's for
slug: <slug>
---
```

One `#` heading that matches the search phrase. Link each page to the two or three pages nearest it, and to the product's own page. A comparison page says where the competitor is the better choice too: a page that only praises the product reads as an ad and gets no trust.
