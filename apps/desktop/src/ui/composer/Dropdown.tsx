import { useEffect, useRef, useState, type ReactNode } from "react";

export interface Option<T> {
  value: T;
  label: string;
  detail?: string;
}

/** A compact control that opens a list above it. */
export function Dropdown<T>(props: {
  label: ReactNode;
  title: string;
  options: Option<T>[];
  value: T | null;
  onPick(value: T): void;
  header?: ReactNode;
  wide?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !root.current?.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  return (
    <div className="dropdown" ref={root}>
      <button className="quiet dropdown-button" title={props.title} aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen(!open)}>
        {props.label}
      </button>
      {open && (
        <div className={`menu dropdown-menu ${props.wide ? "dropdown-menu--wide" : ""}`} role="listbox" aria-label={props.title}>
          {props.header}
          {props.options.map((o) => (
            <button
              key={o.label}
              role="option"
              aria-selected={o.value === props.value}
              className="menu-item"
              onClick={() => {
                props.onPick(o.value);
                setOpen(false);
              }}
            >
              <span className="dropdown-label">{o.label}</span>
              {o.detail && <span className="dropdown-detail">{o.detail}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
