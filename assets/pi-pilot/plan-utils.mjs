export const PLAN_TOOLS = Object.freeze([
  "read",
  "grep",
  "find",
  "ls",
  "web_search",
  "fetch_content",
  "questionnaire",
]);

export function isPlanToolAllowed(name) {
  return PLAN_TOOLS.includes(name);
}

export function extractPlan(text) {
  const match = text.match(/(?:^|\n)(?:#{1,3}\s*)?Plan:\s*\n((?:.|\n)*)/i);
  if (!match) return null;
  const steps = [...match[1].matchAll(/^\s*\d+[.)]\s+(.+)$/gm)]
    .map((item) => item[1].trim())
    .filter(Boolean);
  return steps.length > 0 ? steps : null;
}
