export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Space kept between a placed node and its neighbours. */
const GAP = 24;
/** Rings of slots searched around the source before giving up. */
const MAX_RINGS = 12;

function overlaps(a: Rect, b: Rect): boolean {
  return (
    a.x < b.x + b.width + GAP &&
    a.x + a.width + GAP > b.x &&
    a.y < b.y + b.height + GAP &&
    a.y + a.height + GAP > b.y
  );
}

/** Slot offsets (in source-sized steps) of ring `r`, nearest first: right,
 *  below, left, above, then the remaining cells of the ring. */
function ringOffsets(r: number): [number, number][] {
  const cells: [number, number][] = [];
  for (let dy = -r; dy <= r; dy += 1) {
    for (let dx = -r; dx <= r; dx += 1) {
      if (Math.max(Math.abs(dx), Math.abs(dy)) === r) cells.push([dx, dy]);
    }
  }
  const sideRank = ([dx, dy]: [number, number]): number => {
    if (dx === r && dy === 0) return 0;
    if (dx === 0 && dy === r) return 1;
    if (dx === -r && dy === 0) return 2;
    if (dx === 0 && dy === -r) return 3;
    return 4;
  };
  return cells.sort(
    (a, b) =>
      sideRank(a) - sideRank(b) ||
      Math.hypot(a[0], a[1]) - Math.hypot(b[0], b[1]),
  );
}

/** Top-left position for a node the size of `source`, in the nearest slot
 *  around it that doesn't overlap any of `occupied`. */
export function findFreeSpot(
  source: Rect,
  occupied: Rect[],
): { x: number; y: number } {
  const stepX = source.width + GAP;
  const stepY = source.height + GAP;
  for (let r = 1; r <= MAX_RINGS; r += 1) {
    for (const [dx, dy] of ringOffsets(r)) {
      const candidate: Rect = {
        ...source,
        x: source.x + dx * stepX,
        y: source.y + dy * stepY,
      };
      if (!occupied.some((rect) => overlaps(candidate, rect))) {
        return { x: candidate.x, y: candidate.y };
      }
    }
  }
  return { x: source.x + stepX, y: source.y };
}
