import { api, on } from "../shared/api";
import { STATES, type IslandState } from "../shared/theme";
import { Island } from "./island";

const root = document.querySelector<HTMLElement>(".island");
if (!root) throw new Error("overlay: .island not found");

const island = new Island(root);

island.onResize = (width, height) => {
  api.overlayResize(width, height).catch((err) => console.warn("overlay_resize failed", err));
};

async function main(): Promise<void> {
  // Subscribe before announcing readiness so nothing emitted in between is lost.
  await on("flow:state", ({ state }) => {
    if ((STATES as readonly string[]).includes(state)) {
      island.setState(state as IslandState);
    } else {
      console.warn("overlay: ignoring unknown state", state);
    }
  });
  await on("flow:text", ({ text }) => island.setText(text));
  await on("flow:level", ({ level }) => island.pushLevel(level));

  await api.overlayReady().catch((err) => console.warn("overlay_ready failed", err));
}

void main();

// Handy from the devtools console while tuning.
declare global {
  interface Window {
    island: Island;
  }
}
window.island = island;
