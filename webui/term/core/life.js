// A faithful port of eidolon's TUI pulse, crates/tui/src/life.rs: Conway's
// Life on a world eight rows tall, of which the middle four are drawn in
// braille (each character a 2x4 block of dots). A turn opens on the agent
// standing at the strip's left edge; a tool call launches a lightweight
// spaceship from the left, its result one from the right; a strip that has
// settled (still, a two-beat, or thinned to nothing) stands the agent up
// again with a ship from the other edge each time. Stepped on the TUI's own
// tick, 250 ms (crates/tui/src/app.rs:2087-2090).

export const TICK_MS = 250;           // app.rs:2087
export const CELLS = 12;              // state.rs:1401, World::new(12)
const ROWS = 4;                       // life.rs:16
const WORLD = 8;                      // life.rs:17
const TOP = (WORLD - ROWS) / 2;       // life.rs:18
const DOT = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]]; // life.rs:22
const SHIP = ['.#..#', '#....', '#...#', '####.'];                 // life.rs:25
const AGENT = ['.#.', '###', '.#.', '#.#'];                        // life.rs:30
const BLANK = '⠀';

export class World {
  constructor(cells = CELLS) {
    this.cols = Math.max(1, cells) * 2;
    this.cells = new Uint8Array(WORLD * this.cols);
    this.prev = ['', ''];
    this.launches = 0;
    this.agent();
  }

  // A new turn: clear the strip and stand the agent in it again (life.rs:55).
  restart() {
    this.cells = new Uint8Array(WORLD * this.cols);
    this.prev = ['', ''];
    this.agent();
  }

  agent() { // life.rs:64-73
    AGENT.forEach((row, y) => {
      for (let x = 0; x < row.length; x++) {
        if (row[x] === '#') this.cells[(TOP + y) * this.cols + (x % this.cols)] = 1;
      }
    });
  }

  call() { this.launch(true); }     // a call going out: a ship from the left
  result() { this.launch(false); }  // a result coming back: one from the right

  launch(fromLeft) { // life.rs:87-98
    const w = SHIP[0].length;
    const x0 = fromLeft ? 0 : this.cols - w;
    SHIP.forEach((row, y) => {
      const cells = fromLeft ? [...row].reverse() : [...row];
      cells.forEach((ch, x) => {
        if (ch === '#' && x0 + x >= 0 && x0 + x < this.cols) this.cells[(TOP + y) * this.cols + x0 + x] = 1;
      });
    });
    this.launches++;
  }

  step() { // life.rs:102-117
    this.cells = step(this.cells, this.cols);
    const now = this.render();
    const stuck = now === this.prev[0] || now === this.prev[1];
    let lit = 0;
    for (const c of now) if (c !== BLANK) lit++;
    this.prev = [this.prev[1], now];
    if (stuck || lit < 3) {
      this.restart();
      this.launch(this.launches % 2 === 0);
    }
  }

  render() { // life.rs:119-133
    let out = '';
    for (let c = 0; c < this.cols / 2; c++) {
      let bits = 0;
      for (let dx = 0; dx < 2; dx++) {
        for (let r = 0; r < ROWS; r++) {
          if (this.cells[(TOP + r) * this.cols + c * 2 + dx]) bits |= DOT[dx][r];
        }
      }
      out += String.fromCharCode(0x2800 + bits);
    }
    return out;
  }
}

// The agent as a pair of braille cells standing still (life.rs:140-149).
export function icon() {
  const w = new World(2);
  return w.render();
}

function step(world, cols) { // life.rs:151-171, a torus
  const next = new Uint8Array(WORLD * cols);
  for (let r = 0; r < WORLD; r++) {
    for (let c = 0; c < cols; c++) {
      let n = 0;
      for (const dr of [WORLD - 1, 0, 1]) {
        for (const dc of [cols - 1, 0, 1]) {
          if (dr === 0 && dc === 0) continue;
          n += world[((r + dr) % WORLD) * cols + ((c + dc) % cols)];
        }
      }
      const alive = world[r * cols + c];
      next[r * cols + c] = (alive && (n === 2 || n === 3)) || (!alive && n === 3) ? 1 : 0;
    }
  }
  return next;
}

// The pulse's word after `calls` tool calls: turns over every eight (life.rs:177-182).
const WORDS = ['COGITAT', 'MEDITAT', 'RUMINAT', 'DELIBERAT', 'PONDERAT', 'SPECULAT', 'EXCOGITAT', 'PERPENDIT'];
export const word = (calls) => WORDS[Math.floor(Math.max(0, calls) / 8) % WORDS.length];

// The strip is shaded along its length, #ff922b at the left to #ffd43b at the
// right (crates/tui/ui/default.rn:512-518).
export function shade(x, width) {
  const hex = (n) => Math.floor(n).toString(16).padStart(2, '0');
  return `#ff${hex(0x92 + (0x42 * x) / width)}${hex(0x2b + (0x10 * x) / width)}`;
}
