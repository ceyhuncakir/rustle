import { useEffect, useId, useRef, useState } from "react";

interface ComboboxProps {
  value: string;
  options: string[];
  onChange: (value: string) => void;
  placeholder?: string;
  loading?: boolean;
  label?: string;
  className?: string;
}

/** A searchable list: type to filter, arrows to move, Enter to pick. Free text is accepted. */
export function Combobox({ value, options, onChange, placeholder, loading, label, className }: ComboboxProps) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState(value);
  const [active, setActive] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  // A new saved value replaces whatever was being typed.
  const [seen, setSeen] = useState(value);
  if (seen !== value) {
    setSeen(value);
    setQuery(value);
  }

  const q = query.trim().toLowerCase();
  const filtered = !q || q === value.toLowerCase() ? options : options.filter((o) => o.toLowerCase().includes(q));

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) {
        setOpen(false);
        setQuery(value);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open, value]);

  useEffect(() => {
    if (!open) return;
    listRef.current?.children[active]?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  const pick = (v: string) => {
    onChange(v);
    setQuery(v);
    setOpen(false);
  };

  return (
    <div ref={root} className={"relative " + (className ?? "")}>
      <div className="control flex h-8 items-center gap-1 pr-1">
        <input
          role="combobox"
          aria-expanded={open}
          aria-controls={id}
          aria-autocomplete="list"
          aria-label={label}
          value={query}
          placeholder={placeholder}
          onFocus={() => setOpen(true)}
          onChange={(e) => {
            setQuery(e.target.value);
            setOpen(true);
            setActive(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setOpen(true);
              setActive((a) => Math.min(filtered.length - 1, a + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((a) => Math.max(0, a - 1));
            } else if (e.key === "Enter") {
              e.preventDefault();
              const chosen = open ? filtered[active] : undefined;
              pick(chosen ?? query.trim());
            } else if (e.key === "Escape") {
              setOpen(false);
              setQuery(value);
            }
          }}
          className="h-full min-w-0 flex-1 bg-transparent font-mono text-[12.5px] outline-none placeholder:font-sans placeholder:text-fg-3"
          autoComplete="off"
          spellCheck={false}
        />
        <button
          type="button"
          tabIndex={-1}
          aria-label="Show list"
          onClick={() => setOpen((o) => !o)}
          className="flex h-6 w-6 items-center justify-center rounded text-fg-2 hover:bg-surface-3"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M3 4.5l3 3 3-3" />
          </svg>
        </button>
      </div>
      {open && (
        <ul
          id={id}
          ref={listRef}
          role="listbox"
          className="absolute right-0 z-20 mt-1 max-h-[240px] w-full min-w-[260px] overflow-auto rounded-control bg-surface py-1 [box-shadow:0_8px_24px_rgba(0,0,0,0.25),0_0_0_1px_var(--line-strong)]"
        >
          {loading && filtered.length === 0 && <li className="px-3 py-1.5 text-[12.5px] text-fg-2">Loading…</li>}
          {!loading && filtered.length === 0 && (
            <li className="px-3 py-1.5 text-[12.5px] text-fg-2">
              No match{query.trim() ? " - press Enter to use what you typed" : ""}
            </li>
          )}
          {filtered.map((o, i) => (
            <li
              key={o}
              role="option"
              aria-selected={o === value}
              onMouseDown={(e) => e.preventDefault()}
              onMouseEnter={() => setActive(i)}
              onClick={() => pick(o)}
              className={
                "cursor-default truncate px-3 py-1.5 font-mono text-[12.5px] " +
                (i === active ? "bg-accent-soft" : "") +
                (o === value ? " text-accent" : "")
              }
            >
              {o}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
