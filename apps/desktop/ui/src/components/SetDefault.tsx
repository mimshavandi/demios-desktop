// "Set as default" on a model row (#219): sets the task's slot to this model. A model shows it
// for each task it suits (Chat for chat models; Project search for embedding models on this
// device, the only ones search can use). Once set, the row says "Default for <task>" instead.

import { createContext, useContext } from "react";
import type { ModelCatalog, SlotInfo } from "../ipc/types";
import { Badge, Button } from "./primitives";

export interface Defaults {
  catalog: ModelCatalog;
  /** Saves `modelRef` as `slot`'s model and confirms it ("Now used for Chat"). */
  setDefault: (slot: SlotInfo, modelRef: string) => void;
}

export const DefaultsContext = createContext<Defaults | null>(null);

/** The tasks a listed, usable model suits. */
export function tasksFor(catalog: ModelCatalog, modelRef: string): SlotInfo[] {
  const model = catalog.models.find((m) => m.model_ref === modelRef);
  if (model === undefined || !model.available) return [];
  return catalog.slots.filter(
    (s) => model.capabilities.includes(s.capability) && (s.slot_id !== "embed" || model.source === "local"),
  );
}

export function SetDefault({ modelRef, name }: { modelRef: string; name: string }) {
  const d = useContext(DefaultsContext);
  if (d === null) return null;
  return (
    <>
      {tasksFor(d.catalog, modelRef).map((slot) =>
        d.catalog.settings.slots[slot.slot_id] === modelRef ? (
          <Badge key={slot.slot_id} tone="accent">
            Default for {slot.label}
          </Badge>
        ) : (
          <Button
            key={slot.slot_id}
            size="sm"
            aria-label={`Set ${name} as default for ${slot.label}`}
            onClick={() => d.setDefault(slot, modelRef)}
          >
            Set as default
          </Button>
        ),
      )}
    </>
  );
}
