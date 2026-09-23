import { useEffect, useRef } from "react";
import { useEvent } from "../hooks";
import { Visualizer } from "../visualizer";

/** The pill's own waveform, fed by `flow:level`. */
export function LevelMeter({ className }: { className?: string }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const vis = useRef<Visualizer | null>(null);

  useEffect(() => {
    if (!canvas.current) return;
    const v = new Visualizer(canvas.current);
    v.setMode("listening");
    v.start();
    vis.current = v;
    return () => {
      v.destroy();
      vis.current = null;
    };
  }, []);

  useEvent("flow:level", ({ level }) => vis.current?.pushLevel(level));

  return (
    <canvas
      ref={canvas}
      aria-label="Microphone level"
      role="img"
      className={"block h-[44px] w-full rounded-[22px] bg-[rgba(17,19,27,0.88)] [box-shadow:inset_0_0_0_1px_rgba(255,255,255,0.10),0_10px_30px_rgba(0,0,0,0.35)] " + (className ?? "")}
    />
  );
}
