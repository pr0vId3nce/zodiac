// Parses a Claude Code question dialog out of a pane's rendered screen —
// the numbered-option picker Claude draws for permission prompts and
// AskUserQuestion ("❯ 1. Yes / 2. Yes, and don't ask again / 3. No").
//
// Deterministic and conservative on purpose: it only trusts a block of
// consecutively-numbered options starting at 1 that sits near the bottom
// of the screen (where Claude's dialogs live), so a numbered list quoted
// up in the transcript can't masquerade as a question. Called by main.ts
// when a pane flips to needs_input; the result rides in push payloads and
// the state broadcast so both the iOS shell and the web UI can render
// real answer buttons.

export interface PaneQuestion {
  question: string;
  options: string[];
}

const MAX_OPTIONS = 9;
// The options block must end within this many non-blank rows of the screen
// bottom. Claude's dialogs sit directly above the input box + hint line.
const BOTTOM_WINDOW = 12;

const OPTION_RE = /^\s*(?:❯\s*)?(\d)[.)]\s+(\S.*)$/;
const RULE_RE = /^[\s─╌╍━═╭╮╯╰┌┐└┘├┤]*$/;

/** Drops box-drawing side borders and trailing whitespace. */
function stripChrome(line: string): string {
  return line.replace(/[│┃║]/g, " ").replace(/\s+$/, "");
}

export function parseQuestion(screen: string): PaneQuestion | null {
  const lines = screen.split("\n").map(stripChrome);

  // Index of the last non-blank row, for the bottom-window guard.
  let lastContent = lines.length - 1;
  while (lastContent >= 0 && !lines[lastContent].trim()) lastContent--;
  if (lastContent < 0) return null;

  // Scan bottom-up for the last block of consecutively numbered options.
  for (let end = lastContent; end >= 0; end--) {
    const m = lines[end].match(OPTION_RE);
    if (!m) continue;

    // Walk this numbered block upward to its first option.
    const numbered = new Map<number, number>(); // option number -> line idx
    numbered.set(Number(m[1]), end);
    let i = end - 1;
    let expected = Number(m[1]) - 1;
    while (i >= 0 && expected >= 1) {
      const line = lines[i];
      const om = line.match(OPTION_RE);
      if (om && Number(om[1]) === expected) {
        numbered.set(expected, i);
        expected--;
      } else if (line.trim() && !om) {
        // a wrapped continuation of the option above it — skip
        if (!line.match(/^\s{2,}/)) break;
      } else if (om) {
        break; // numbering out of order — not a dialog
      }
      i--;
    }

    const count = numbered.size;
    if (count < 2 || count > MAX_OPTIONS || !numbered.has(1)) continue;
    // Numbers must be exactly 1..count.
    if ([...numbered.keys()].some((n) => n < 1 || n > count)) continue;

    // Bottom-window guard: count non-blank rows below the block's end.
    let below = 0;
    for (let j = end + 1; j <= lastContent; j++) if (lines[j].trim()) below++;
    if (below > BOTTOM_WINDOW) continue;

    // Option text: the numbered line plus indented continuation rows. The
    // LAST option's wrapped lines sit below `end` (the bottom-up scan
    // stopped at the last *numbered* line) — walk indented rows down until
    // a blank, rule, or unindented line, so "3. No, and tell Claude what
    // to do differently…" keeps its tail at narrow widths.
    let lastEnd = end + 1;
    while (
      lastEnd <= lastContent &&
      /^\s{2,}\S/.test(lines[lastEnd]) &&
      !RULE_RE.test(lines[lastEnd])
    ) {
      lastEnd++;
    }
    const startIdx = numbered.get(1)!;
    const options: string[] = [];
    for (let n = 1; n <= count; n++) {
      const idx = numbered.get(n)!;
      let text = lines[idx].match(OPTION_RE)![2].trim();
      const next = n < count ? numbered.get(n + 1)! : lastEnd;
      for (let j = idx + 1; j < next; j++) {
        const cont = lines[j].trim();
        if (cont) text += ` ${cont}`;
      }
      options.push(text);
    }

    // Question: the non-blank lines directly above the block (max 3),
    // stopping at a blank row or a border/rule row.
    const qLines: string[] = [];
    for (let j = startIdx - 1; j >= 0 && qLines.length < 3; j--) {
      const line = lines[j].trim();
      if (!line || RULE_RE.test(line)) {
        if (qLines.length) break;
        continue; // skip the dialog's top padding/border first
      }
      qLines.unshift(line);
    }
    const question = qLines.join(" ").trim();
    if (!question) continue;

    return { question, options };
  }
  return null;
}
