import { FileSpreadsheet, Paperclip, TriangleAlert, X } from "lucide-react";
import { forwardRef } from "react";
import type { ChatFile } from "../ipc/types";
import { ICON, IconButton, InlineMessage, Skeleton } from "./primitives";

/**
 * Data files attached to a chat (#370; design: ChatAttachments, approved 2026-11-28).
 * The files belong to the chat, not to one message: sending doesn't clear them.
 */

/** "676 B", "2.4 KB", "2.4 MB". */
export function fileSize(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  if (bytes < 1e6) return `${(bytes / 1e3).toFixed(1)} KB`;
  if (bytes < 1e9) return `${(bytes / 1e6).toFixed(1)} MB`;
  return `${(bytes / 1e9).toFixed(1)} GB`;
}

/** Cuts a long name in the middle, so the extension stays visible. */
export function middleCut(name: string, max = 28): string {
  if (name.length <= max) return name;
  const keep = max - 1;
  const tail = Math.ceil(keep / 2);
  return `${name.slice(0, keep - tail)}…${name.slice(name.length - tail)}`;
}

export const MISSING_HELP = "This file can't be read any more. Remove it and attach it again.";

export const COPY_HELP = "Demios keeps a copy in this chat. Changes to the original file don't show here.";

/** What a provider gets from the chat's files (approved wording, DS ChatAttachments). */
export function providerHelp(provider: string): string {
  return `${provider} gets what the data tools compute from these files to answer: column names, counts, totals, averages, smallest and largest values and the most common values, per group when a question asks, such as each customer's total. A narrow question can show single entries.`;
}

/** The paperclip in the composer bar. */
export const AttachButton = forwardRef<
  HTMLButtonElement,
  { disabled: boolean; busy: boolean; onAttach: () => void }
>(function AttachButton({ disabled, busy, onAttach }, ref) {
  return (
    <IconButton
      ref={ref}
      label="Attach a data file"
      data-testid="attach-button"
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      onClick={onAttach}
    >
      <Paperclip {...ICON} aria-hidden="true" />
    </IconButton>
  );
});

/** "Files in this chat": the chips, with the copy (and provider) help under the label. */
export function FileChips({
  files,
  copying,
  provider,
  onRemove,
  removeRef,
}: {
  files: ChatFile[];
  /** Names still being copied: a chip each, with a Skeleton in place of the size. */
  copying: string[];
  /** The chat model's provider, or `null` when it runs on this device. */
  provider: string | null;
  onRemove: (f: ChatFile, index: number) => void;
  /** Lets the composer move focus to a chip's remove button after a removal. */
  removeRef: (index: number, el: HTMLButtonElement | null) => void;
}) {
  if (files.length === 0 && copying.length === 0) return null;
  return (
    <div className="chat-files" data-testid="chat-files">
      <p className="chat-files-label" id="chat-files-h">
        Files in this chat
      </p>
      <p className="chat-files-help" id="chat-files-help">
        {COPY_HELP}
        {provider !== null && ` ${providerHelp(provider)}`}
      </p>
      <ul className="chat-files-list" aria-labelledby="chat-files-h" aria-describedby="chat-files-help">
        {files.map((f, i) => (
          <li key={f.id} className="chat-file">
            {f.missing ? (
              <TriangleAlert {...ICON} className="chat-file-missing" role="img" aria-label={MISSING_HELP} />
            ) : (
              <FileSpreadsheet {...ICON} aria-hidden="true" />
            )}
            <span className="chat-file-name" title={f.missing ? MISSING_HELP : f.name}>
              {middleCut(f.name)}
            </span>
            <span className="chat-file-size">{fileSize(f.size)}</span>
            <IconButton
              ref={(el) => removeRef(i, el)}
              size="sm"
              label={`Remove ${f.name}`}
              onClick={() => onRemove(f, i)}
            >
              <X {...ICON} aria-hidden="true" />
            </IconButton>
          </li>
        ))}
        {copying.map((name) => (
          <li key={`copying:${name}`} className="chat-file" aria-busy="true">
            <FileSpreadsheet {...ICON} aria-hidden="true" />
            <span className="chat-file-name" title={name}>
              {middleCut(name)}
            </span>
            <Skeleton width="var(--dn-space-8)" />
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Why files weren't attached, in the engine's words: one sentence, or a list for several. */
export function NotAttached({
  skipped,
  onDismiss,
}: {
  skipped: { name: string; reason: string }[];
  onDismiss: () => void;
}) {
  if (skipped.length === 0) return null;
  const dismiss = (
    <IconButton size="sm" label="Dismiss" onClick={onDismiss}>
      <X {...ICON} aria-hidden="true" />
    </IconButton>
  );
  const [only] = skipped;
  return (
    <div className="chat-files-notice" data-testid="not-attached">
      <InlineMessage tone="warning" action={dismiss}>
        {skipped.length === 1 && only !== undefined ? (
          `${only.name} wasn't attached: ${only.reason}`
        ) : (
          <>
            These files weren't attached:
            <ul className="chat-files-skipped">
              {skipped.map((s) => (
                <li key={s.name}>
                  {s.name}: {s.reason}
                </li>
              ))}
            </ul>
          </>
        )}
      </InlineMessage>
    </div>
  );
}
