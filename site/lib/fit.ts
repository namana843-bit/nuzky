// `sizes` for an image inside a Fit composition: `w` CSS pixels at the design width, scaled down with the
// composition once the page (minus its side padding) is narrower than the design.
export const fitSizes = (w: number, design: number, padding = 40) =>
  `(min-width: ${design + padding}px) ${w}px, ${Math.ceil((w / design) * 100)}vw`;

// The most a phone enlarges the part of a composition that a Fit `view` frames. A wider window shows more
// of the composition around the same centre instead of enlarging it further.
export const VIEW_ZOOM = 1.15;

// `sizes` for an image in a composition a phone frames with a `view`: below the `md` breakpoint it can be
// shown at up to VIEW_ZOOM times its design size.
export const viewSizes = (w: number, design: number) => `(max-width: 767px) ${Math.ceil(w * VIEW_ZOOM)}px, ${fitSizes(w, design)}`;
