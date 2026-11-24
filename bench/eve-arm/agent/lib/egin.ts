// Calls one of egin's tools for this run's session through the metering proxy (EGIN_TOOL_URL, set by egin).
export async function egin(name: string, args: Record<string, unknown>): Promise<string> {
  const url = process.env.EGIN_TOOL_URL;
  if (!url) throw new Error(`${name} is not available in this run`);
  const res = await fetch(`${url}/tool/${name}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(args),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(text || `${name} failed (${res.status})`);
  return text;
}
