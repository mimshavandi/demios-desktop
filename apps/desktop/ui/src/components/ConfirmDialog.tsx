import * as AlertDialog from "@radix-ui/react-alert-dialog";
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { Button, InlineMessage } from "./primitives";
import { KEY_GUARD_MS } from "./ToolCards";

/**
 * The shared confirm for every irreversible action (design-system §4, Dialog alert
 * variant). Use it only when undo is impossible. The title names the action, the body lists
 * exactly what goes, and the red button is a verb ("Delete all data"). Focus is trapped
 * and starts on Cancel; `Esc` cancels; focus returns to the trigger. Like the approval
 * card, the confirm button stays disabled for [`KEY_GUARD_MS`] after opening, so a double
 * click or a held Enter meant for the trigger never confirms.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  children,
  confirmLabel,
  busy = false,
  ready = true,
  error = null,
  onConfirm,
  returnFocusTo,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  children: ReactNode;
  confirmLabel: string;
  busy?: boolean;
  /** False while the body is still loading what will be deleted: confirm stays disabled. */
  ready?: boolean;
  error?: string | null;
  onConfirm: () => void;
  returnFocusTo?: RefObject<HTMLElement | null>;
}) {
  const [armed, setArmed] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) {
      setArmed(false);
      return;
    }
    const t = window.setTimeout(() => setArmed(true), KEY_GUARD_MS);
    return () => window.clearTimeout(t);
  }, [open]);

  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Overlay className="dn-dialog-overlay" />
        <AlertDialog.Content
          className="dn-dialog"
          onOpenAutoFocus={(e) => {
            // Start on Cancel, never on the red button (§4).
            e.preventDefault();
            cancelRef.current?.focus();
          }}
          aria-busy={busy || undefined}
          onCloseAutoFocus={(e) => {
            if (returnFocusTo?.current) {
              e.preventDefault();
              returnFocusTo.current.focus();
            }
          }}
          onEscapeKeyDown={(e) => {
            if (busy) e.preventDefault();
          }}
        >
          <AlertDialog.Title className="dn-dialog-title">{title}</AlertDialog.Title>
          <AlertDialog.Description asChild>
            <div className="dn-dialog-body">{children}</div>
          </AlertDialog.Description>
          {error !== null && (
            <InlineMessage tone="error">
              {error}
            </InlineMessage>
          )}
          <div className="dn-dialog-actions">
            <AlertDialog.Cancel asChild>
              <Button variant="secondary" ref={cancelRef} disabled={busy}>
                Cancel
              </Button>
            </AlertDialog.Cancel>
            {/* Not AlertDialog.Action: that closes at once; this stays open until the work ends. */}
            <Button variant="danger" loading={busy} disabled={!armed || !ready || busy} onClick={onConfirm}>
              {confirmLabel}
            </Button>
          </div>
        </AlertDialog.Content>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
