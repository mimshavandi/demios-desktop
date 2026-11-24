import type { ModelCatalog } from "./ipc/types";

export interface ModelBadgeInfo {
  /** True when the model runs on this device (mint badge). */
  local: boolean;
  text: string;
}

/** "On this device · Qwen3 8B" or "Anthropic · claude-sonnet-5-5" (design-system §4 Model badge). */
export function describeModel(ref: string | null, catalog: ModelCatalog | null): ModelBadgeInfo | null {
  if (ref === null || ref === "") return null;
  const [scheme, a = "", ...rest] = ref.split(":");
  if (scheme === "local") {
    const known = catalog?.models.find((m) => m.model_ref === ref);
    const name = known?.label.replace(/\s*\(.*\)$/, "") ?? a;
    return { local: true, text: `On this device · ${name}` };
  }
  if (scheme === "byok") {
    const provider = catalog?.providers.find((p) => p.id === a)?.name ?? a;
    return { local: false, text: `${provider} · ${rest.join(":")}` };
  }
  return { local: false, text: `${a} · ${rest.join(":")}` };
}

/** The model chat will use: what the backend resolved, else the chat slot, else the dev
 * fake when this build has it. */
export function chatModel(catalog: ModelCatalog | null, devFake: boolean): string | null {
  if (catalog !== null && catalog.chat_model !== null) return catalog.chat_model;
  const chosen = catalog?.settings.slots["chat"];
  if (chosen !== undefined && chosen !== "") return chosen;
  return devFake ? "local:dev-fake" : null;
}

/** Whether a message can be sent right now (composer "Connect a model" state otherwise). */
export function chatModelReady(catalog: ModelCatalog | null, devFake: boolean): boolean {
  if (catalog !== null && catalog.chat_model !== null) return true;
  const ref = chatModel(catalog, devFake);
  if (ref === null) return false;
  if (ref === "local:dev-fake") return devFake;
  return catalog?.models.some((m) => m.model_ref === ref && m.available) ?? false;
}

/** A model's name for "Made by <model>": "Qwen3 8B", "claude-sonnet-5-5 (Anthropic)". */
export function modelName(ref: string, catalog: ModelCatalog | null): string {
  const [scheme, a = "", ...rest] = ref.split(":");
  if (scheme === "local") {
    return catalog?.models.find((m) => m.model_ref === ref)?.label.replace(/\s*\(.*\)$/, "") ?? a;
  }
  if (scheme === "byok" || scheme === "remote") {
    const provider = catalog?.providers.find((p) => p.id === a)?.name ?? a;
    return `${rest.join(":")} (${provider})`;
  }
  return ref;
}
