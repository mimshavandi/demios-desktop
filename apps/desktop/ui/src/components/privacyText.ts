/**
 * Sentences of Settings › General › Privacy copied exactly from `docs/user/privacy.md`, the
 * owner-approved privacy text (#402). `App.test.tsx` checks they're still in that file word
 * for word, and that every "Where your data goes" section there has a row in Settings, so the
 * two can't drift apart. Change the wording in privacy.md first, with the owner's approval.
 */

/** "Model providers you add a key for" (privacy.md, approved with #158). */
export const PROJECT_INSTRUCTIONS_SENT = "In a project's chats, the project's instructions are sent with each message too.";

/** "Model providers you add a key for": what the data tools send (privacy.md, approved with #400,
 * extended to files attached to a chat with ChatAttachments, #370). */
export const DATA_FILES_SENT =
  "When a chat has data files (a project's, or attached to the chat), what Demios computes from them for an answer is sent too: column names and summaries such as counts, totals, averages, smallest and largest values and the most common values, also per group (for example per customer). A narrow question can reveal single entries.";

/** The "Your Demios Node, if you pair one" row's title and text (privacy.md, from #307). */
export const NODE_TITLE = "Your Demios Node, if you pair one";
export const NODE_TEXT =
  "Your chats and projects sync to a Node you run on your own machine, and through it to your other paired devices, encrypted end to end. Nothing goes to Demios. Files attached to a chat stay on this computer and don't sync.";
