interface SwitchProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  id?: string;
}

export function Switch({ checked, onChange, disabled, id }: SwitchProps) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={
        "relative h-[24px] w-[42px] shrink-0 rounded-full transition-colors duration-150 disabled:opacity-50 " +
        (checked ? "bg-accent" : "bg-surface-3 [box-shadow:inset_0_0_0_1px_var(--line-strong)]")
      }
    >
      <span
        className={
          "absolute top-[3px] left-[3px] h-[18px] w-[18px] rounded-full bg-white shadow-[0_1px_2px_rgba(0,0,0,0.35)] transition-transform duration-150 " +
          (checked ? "translate-x-[18px]" : "")
        }
      />
    </button>
  );
}
