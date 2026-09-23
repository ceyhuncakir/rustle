export interface Option<V extends string = string> {
  value: V;
  label: string;
  disabled?: boolean;
}

interface SelectProps<V extends string> {
  value: V;
  options: readonly Option<V>[];
  onChange: (value: V) => void;
  id?: string;
  label?: string;
}

/** A native select in the app's clothing: keyboard, screen reader and OS popup for free. */
export function Select<V extends string>({ value, options, onChange, id, label }: SelectProps<V>) {
  return (
    <select
      id={id}
      aria-label={label}
      value={value}
      onChange={(e) => onChange(e.target.value as V)}
      className="control select h-8 max-w-[280px] cursor-default truncate text-[13.5px]"
    >
      {options.map((o) => (
        <option key={o.value} value={o.value} disabled={o.disabled}>
          {o.label}
        </option>
      ))}
    </select>
  );
}
