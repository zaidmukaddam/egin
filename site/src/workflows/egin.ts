// The docs agent's sessions run as Workflow runs. The workflow itself ships with egin; re-exporting
// it here puts it where the Workflow compiler looks, so it is built into this app.
export * from "egin/workflow/session";
