/**
 * Sentences of Settings › General › Privacy copied exactly from `docs/user/privacy.md`, the
 * owner-approved privacy text (#402). `App.test.tsx` checks they're still in that file word
 * for word, and that every "Where your data goes" section there has a row in Settings, so the
 * two can't drift apart. Change the wording in privacy.md first, with the owner's approval.
 */

/** "Model providers you add a key for" (privacy.md, approved with #158). */
export const PROJECT_INSTRUCTIONS_SENT = "In a project's chats, the project's instructions are sent with each message too.";

/** The "Your Demios Node, if you pair one" row's title and text (privacy.md, from #307). */
export const NODE_TITLE = "Your Demios Node, if you pair one";
export const NODE_TEXT =
  "Your chats and projects sync to a Node you run on your own machine, and through it to your other paired devices, encrypted end to end. Nothing goes to Demios.";
