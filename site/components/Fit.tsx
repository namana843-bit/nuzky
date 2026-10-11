"use client";

import { useEffect, useRef } from "react";
import { VIEW_ZOOM } from "@/lib/fit";

/** A part of a composition: its centre and how many design pixels wide it is. */
export type FitView = { x: number; y: number; w: number };

// Lays a fixed-size composition out at its design size and scales it down to the available width,
// so the reels and the editor keep their exact proportions on every screen.
//
// The scale is the `--fit` variable. An inline script sets it while the HTML is still parsing, so a phone never
// paints the composition at full size while the JavaScript loads (or when it fails to run), and a resize only
// updates the variable instead of rendering the whole composition again.
//
// With a `view`, a phone shows that part of the composition in a window instead of the whole thing shrunk
// past reading. The framing is CSS (`.fit[data-view]` in globals.css), so it is right from the first paint.
export function Fit({
  width,
  height,
  className = "",
  waves,
  view,
  children,
}: {
  width: number;
  height: number;
  className?: string;
  /** Marks the composition as an anchor for the light behind it (see Waves). */
  waves?: "swell" | "glow";
  view?: FitView;
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => el.style.setProperty("--fit", String(Math.min(1, entry.contentRect.width / width))));
    ro.observe(el);
    return () => ro.disconnect();
  }, [width]);

  return (
    // The inline script adds `--fit` to the style before React hydrates, so the attribute differs on purpose.
    <div
      ref={ref}
      data-waves={waves}
      data-view={view ? "" : undefined}
      suppressHydrationWarning
      className={`fit relative w-full ${className}`}
      style={
        {
          maxWidth: width,
          "--fit-w": width,
          "--fit-h": height,
          ...(view && { "--view-x": view.x, "--view-y": view.y, "--view-w": view.w, "--view-zoom": VIEW_ZOOM }),
        } as React.CSSProperties
      }
    >
      <script
        dangerouslySetInnerHTML={{
          __html: `(function(e){e.style.setProperty("--fit",Math.min(1,e.clientWidth/${width}))})(document.currentScript.parentElement)`,
        }}
      />
      <div className="fit-stage absolute left-0 top-0 origin-top-left" style={{ width, height }}>
        {children}
      </div>
    </div>
  );
}
