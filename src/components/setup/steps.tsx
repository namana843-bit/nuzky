import type { ComponentType } from "react";
import type { SetupStep } from "../../lib/setup";
import { AiStep } from "./steps/AiStep";
import { CaptionsStep } from "./steps/CaptionsStep";
import { ComputerStep } from "./steps/ComputerStep";
import { FirstCutStep } from "./steps/FirstCutStep";
import { StyleStep } from "./steps/StyleStep";

export const STEPS: Record<SetupStep, { title: string; Body: ComponentType }> = {
  captions: { title: "Captions model", Body: CaptionsStep },
  ai: { title: "Connect your AI", Body: AiStep },
  computer: { title: "Check this computer", Body: ComputerStep },
  style: { title: "Teach your style", Body: StyleStep },
  "first-cut": { title: "Your first cut", Body: FirstCutStep },
};
