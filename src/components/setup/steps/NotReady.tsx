import { Clock } from "lucide-react";

/** Stands in for a step until its update of Nuzky builds it. */
export function NotReady() {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-1.5 text-center">
      <Clock size={26} className="mb-1 text-muted" />
      <p className="text-[15px] font-semibold text-fg">Not ready yet</p>
      <p className="text-[13px] text-muted">This step comes in a later update of Nuzky.</p>
    </div>
  );
}
