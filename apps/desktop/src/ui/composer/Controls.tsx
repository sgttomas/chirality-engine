import type { Access, Model, TurnSettings } from "../../engine/types";
import { Dropdown } from "./Dropdown";

const accessNames: Record<Access, string> = { readOnly: "Read only", ask: "Ask", fullAccess: "Full access" };
const accessDetail: Record<Access, string> = {
  readOnly: "Reads; asks before anything else",
  ask: "Works in the project; asks to go further",
  fullAccess: "Never asks",
};
const efforts: Record<string, string> = { minimal: "Minimal", low: "Low", medium: "Medium", high: "High", xhigh: "Extra high" };
const effortName = (e: string) => efforts[e] ?? e;

/** Model, reasoning level and access, on one line. */
export function Controls({ models, settings, onChange }: { models: Model[]; settings: TurnSettings; onChange(s: TurnSettings): void }) {
  const model = models.find((m) => m.id === settings.model);
  return (
    <>
      <Dropdown
        title="Model"
        label={model?.name ?? settings.model}
        value={settings.model}
        options={models.map((m) => ({ value: m.id, label: m.name }))}
        onPick={(id) => {
          const next = models.find((m) => m.id === id)!;
          const effort = settings.effort && next.efforts.includes(settings.effort) ? settings.effort : next.defaultEffort;
          onChange({ ...settings, model: id, effort });
        }}
      />
      {model && model.efforts.length > 0 && (
        <Dropdown
          title="Reasoning"
          label={settings.effort ? effortName(settings.effort) : "Reasoning"}
          value={settings.effort}
          options={model.efforts.map((e) => ({ value: e, label: effortName(e) }))}
          onPick={(effort) => onChange({ ...settings, effort })}
        />
      )}
      <Dropdown
        title="Access"
        label={<span className={`access access--${settings.access}`}>{accessNames[settings.access]}</span>}
        value={settings.access}
        wide
        options={(Object.keys(accessNames) as Access[]).map((a) => ({ value: a, label: accessNames[a], detail: accessDetail[a] }))}
        onPick={(access) => onChange({ ...settings, access })}
      />
    </>
  );
}
