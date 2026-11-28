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

/** The provider's name for a model that doesn't run on this device, or `null` for one that does. */
export function providerName(ref: string | null, catalog: ModelCatalog | null): string | null {
  const info = describeModel(ref, catalog);
  if (info === null || info.local) return null;
  return info.text.split(" · ")[0] ?? null;
}

/** The model chat will use: the Chat slot's model, and only that (#219: an empty slot is
 * never filled by an automatic pick; the first suitable model fills it once). */
export function chatModel(catalog: ModelCatalog | null, _devFake: boolean): string | null {
  if (catalog !== null && catalog.chat_model !== null) return catalog.chat_model;
  const chosen = catalog?.settings.slots["chat"];
  return chosen !== undefined && chosen !== "" ? chosen : null;
}

/** Chat models that could answer, for "Choose a model for Chat" (several and none chosen). */
export function chatModelsAvailable(catalog: ModelCatalog | null): boolean {
  return catalog?.models.some((m) => m.available && m.capabilities.includes("chat")) ?? false;
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
