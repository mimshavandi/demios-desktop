import * as RDialog from "@radix-ui/react-dialog";
import { X } from "lucide-react";
import type { ReactNode, RefObject } from "react";
import { ICON, IconButton } from "./primitives";

/**
 * Design-system dialog (#174) for work done in a layer above the page, such as adding a
 * model. Confirms use ConfirmDialog (alert dialog), never this.
 *
 * - `role="dialog"`, labelled by its title; focus is trapped inside while it is open.
 * - Esc and the close button close it; focus returns to `returnFocusTo` (or the trigger).
 * - `size="large"` is wide and tall; on a narrow window every dialog fills the window.
 */
export function Dialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  size = "large",
  returnFocusTo,
  testId,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  /** One line under the title, read with it. */
  description?: string;
  children: ReactNode;
  size?: "large" | "medium";
  /** Where focus goes on close when it isn't the element that opened it. */
  returnFocusTo?: RefObject<HTMLElement | null>;
  testId?: string;
}) {
  return (
    <RDialog.Root open={open} onOpenChange={onOpenChange}>
      <RDialog.Portal>
        <RDialog.Overlay className="dn-dialog-overlay" />
        <RDialog.Content
          className={`dn-dialog dn-dialog-${size}`}
          data-testid={testId}
          {...(description === undefined ? { "aria-describedby": undefined } : {})}
          onCloseAutoFocus={(e) => {
            const el = returnFocusTo?.current;
            if (el) {
              e.preventDefault();
              el.focus();
            }
          }}
        >
          <div className="dn-dialog-head">
            <RDialog.Title className="dn-dialog-title">{title}</RDialog.Title>
            <RDialog.Close asChild>
              <IconButton label="Close">
                <X {...ICON} aria-hidden="true" />
              </IconButton>
            </RDialog.Close>
          </div>
          {description !== undefined && (
            <RDialog.Description className="dn-help">{description}</RDialog.Description>
          )}
          <div className="dn-dialog-body">{children}</div>
        </RDialog.Content>
      </RDialog.Portal>
    </RDialog.Root>
  );
}
