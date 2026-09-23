import type { ReactNode } from "react";
import { Row } from "./Row";
import { Select, type Option } from "./Select";

export interface Choice extends Option {
  description: string;
}

interface ChoiceRowProps {
  id: string;
  title: string;
  choices: readonly Choice[];
  value: string;
  onChange: (value: string) => void;
  /** Defaults to the chosen option's description. */
  subtitle?: ReactNode;
  /** Extra content under the row. */
  below?: ReactNode;
}

/** A Row whose control is a Select, subtitled with what the chosen option means. */
export function ChoiceRow({ id, title, choices, value, onChange, subtitle, below }: ChoiceRowProps) {
  return (
    <Row title={title} htmlFor={id} subtitle={subtitle ?? descriptionOf(choices, value)} below={below}>
      <Select id={id} value={value} options={choices} onChange={onChange} />
    </Row>
  );
}

export function descriptionOf(choices: readonly Choice[], value: string): string {
  return choices.find((c) => c.value === value)?.description ?? "";
}
