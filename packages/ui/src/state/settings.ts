// The person's last choice of model, reasoning level and access, remembered.
import type { Model, TurnSettings } from "../engine/types";

const key = "chirality.turnSettings";

export function remembered(): Partial<TurnSettings> {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "{}") as Partial<TurnSettings>;
  } catch {
    return {};
  }
}

export function remember(settings: TurnSettings) {
  try {
    localStorage.setItem(key, JSON.stringify(settings));
  } catch {
    // Not kept; the next start uses the defaults.
  }
}

/** Settings that the current models accept, preferring what was chosen before. */
export function resolveSettings(models: Model[], wanted: Partial<TurnSettings> | null | undefined): TurnSettings {
  const model = models.find((m) => m.id === wanted?.model) ?? models.find((m) => m.isDefault) ?? models[0];
  const effort = wanted?.effort && model?.efforts.includes(wanted.effort) ? wanted.effort : (model?.defaultEffort ?? model?.efforts[0] ?? null);
  return { model: model?.id ?? "", effort, access: wanted?.access ?? "ask" };
}
