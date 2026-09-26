//! Conway's Life on a strip four cells tall, drawn in braille: each
//! character is a 2×4 block of dots. It is the pulse above the prompt
//! while a turn runs. A turn opens on the agent standing in the middle
//! of the strip — the figure from the start screen, which is the thing
//! that is thinking — and the rules take it from there. The world steps
//! on the UI's tick; what else moves in
//! it is spaceships, and the turn's own traffic launches them: a call
//! goes out from the left, its result comes back from the right, so the
//! strip is the shape of the work — ships crossing, meeting, and the
//! debris of where they met. A world that has settled, into stillness or
//! a two-beat, gets a ship from whichever side went last.
//! The world is eight rows and the strip shows the middle four: on four
//! rows that wrap, a ship's top row is its own bottom neighbour.

/// Rows drawn: a braille cell is four dots tall.
const ROWS: usize = 4;
const WORLD: usize = 8;
const TOP: usize = (WORLD - ROWS) / 2;

/// Braille dot bits by column and row: dots 1–3 down the left, 4–6 down
/// the right, 7 and 8 the bottom pair.
const DOT: [[u32; ROWS]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// A lightweight spaceship heading left; its mirror heads right.
const SHIP: [&str; 4] = [".#..#", "#....", "#...#", "####."];

/// The agent: the figure from the start screen, which is what a turn
/// opens on — it is the thing that is thinking, so it is what stands in
/// the middle before the rules take it anywhere.
const AGENT: [&str; 4] = [".#.", "###", ".#.", "#.#"];

#[derive(Clone, Debug)]
pub struct World {
    cells: Vec<bool>,
    cols: usize,
    /// The last two strips drawn, to notice a window that has stopped.
    /// The world is compared as it is *seen*: a glider drifting through
    /// the rows the strip does not show keeps the world changing while
    /// the strip sits still, and a still strip is the thing to fix.
    prev: [String; 2],
    /// Launches so far; which edge the next lone one leaves from.
    launches: usize,
}

impl World {
    /// A strip `cells` characters wide with the agent at its left edge.
    pub fn new(cells: usize) -> Self {
        let cols = cells.max(1) * 2;
        let mut w = World { cells: vec![false; WORLD * cols], cols, prev: [String::new(), String::new()], launches: 0 };
        w.agent();
        w
    }

    /// A new turn: clear the strip and stand the agent in it again.
    pub fn restart(&mut self) {
        self.cells = vec![false; WORLD * self.cols];
        self.prev = [String::new(), String::new()];
        self.agent();
    }

    /// The agent stands at the strip's left edge, where the icon beside
    /// the mode's name is: a turn starts moving from where the figure
    /// already was, rather than jumping to the middle of the strip.
    fn agent(&mut self) {
        let x0 = 0;
        for (y, row) in AGENT.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                if ch == b'#' {
                    self.cells[(TOP + y) * self.cols + (x0 + x) % self.cols] = true;
                }
            }
        }
    }

    /// A call going out: a ship from the left.
    pub fn call(&mut self) {
        self.launch(true);
    }

    /// A result coming back: a ship from the right, into whatever the
    /// call that asked for it left behind.
    pub fn result(&mut self) {
        self.launch(false);
    }

    /// One ship, from the left heading right or from the right heading left.
    fn launch(&mut self, from_left: bool) {
        let x0 = if from_left { 0 } else { self.cols - SHIP[0].len() };
        for (y, row) in SHIP.iter().enumerate() {
            let cells: Vec<u8> = if from_left { row.bytes().rev().collect() } else { row.bytes().collect() };
            for (x, ch) in cells.iter().enumerate() {
                if *ch == b'#' {
                    self.cells[(TOP + y) * self.cols + x0 + x] = true;
                }
            }
        }
        self.launches += 1;
    }

    /// One generation. A strip that has stopped — repeating itself, or
    /// thinned to nothing — stands the agent up again with a ship.
    pub fn step(&mut self) {
        self.cells = step(&self.cells, self.cols);
        let now = self.render();
        // A strip that repeats itself has stopped, whether it is a still
        // life or a two-beat; one that has thinned to nothing has died.
        let stuck = now == self.prev[0] || now == self.prev[1];
        let thin = now.chars().filter(|c| *c != '\u{2800}').count() < 3;
        self.prev = [std::mem::take(&mut self.prev[1]), now];
        if stuck || thin {
            self.restart();
            // A ship is sent in with it, from the other edge each time:
            // the agent alone settles in two generations, and a strip
            // that repeats the same two frames reads as frozen.
            self.launch(self.launches.is_multiple_of(2));
        }
    }

    pub fn render(&self) -> String {
        (0..self.cols / 2)
            .map(|c| {
                let mut bits = 0u32;
                for (dx, dots) in DOT.iter().enumerate() {
                    for (r, dot) in dots.iter().enumerate() {
                        if self.cells[(TOP + r) * self.cols + c * 2 + dx] {
                            bits |= dot;
                        }
                    }
                }
                char::from_u32(0x2800 + bits).unwrap_or(' ')
            })
            .collect()
    }
}

/// The agent as a pair of braille cells, standing still: the same
/// figure the strip is seeded with, drawn by the same code, so the icon
/// beside the mode's name and the thing that starts moving when a turn
/// does cannot drift apart.
pub fn icon() -> String {
    let mut w = World {
        cells: vec![false; WORLD * 4],
        cols: 4,
        prev: [String::new(), String::new()],
        launches: 0,
    };
    w.agent();
    w.render()
}

fn step(world: &[bool], cols: usize) -> Vec<bool> {
    let mut next = vec![false; WORLD * cols];
    for r in 0..WORLD {
        for c in 0..cols {
            let mut n = 0;
            for dr in [WORLD - 1, 0, 1] {
                for dc in [cols - 1, 0, 1] {
                    if (dr, dc) == (0, 0) {
                        continue;
                    }
                    if world[((r + dr) % WORLD) * cols + (c + dc) % cols] {
                        n += 1;
                    }
                }
            }
            let alive = world[r * cols + c];
            next[r * cols + c] = matches!((alive, n), (true, 2) | (true, 3) | (false, 3));
        }
    }
    next
}

/// The words the pulse wears, all third-person Latin, all "it is
/// thinking"; the one for a turn that has made `calls` tool calls
/// turns over every eight. The transcript's folded thinking line reads
/// the same table, so the two never disagree.
pub const WORDS: [&str; 8] = ["COGITAT", "MEDITAT", "RUMINAT", "DELIBERAT", "PONDERAT", "SPECULAT", "EXCOGITAT", "PERPENDIT"];

/// The pulse's word after `calls` tool calls.
pub fn word(calls: i64) -> &'static str {
    WORDS[(calls.max(0) as usize / 8) % WORDS.len()]
}

/// What a settled turn says, before its clock: perfect-tense Latin, one
/// word per station of the major arcana — the Fool's journey, walked once
/// per twenty-two tool calls. The cards live here in the comment; the
/// terminal line shows only the Latin, deadpan. Each word is true to its
/// count — call sixteen throws the Tower down, call eighteen wanders the
/// Moon — and every multiple of twenty-two is the Fool again: the journey
/// complete where it began.
///
/// INSILIIT (the Fool: it leapt in, no tools) · INSTRUXIT (the Magician:
/// it set out its instruments) · INTUITA EST (the High Priestess: it
/// gazed inward) · COLUIT (the Empress: it cultivated) · ORDINAVIT (the
/// Emperor: it ordered) · INITIAVIT (the Hierophant: it initiated) ·
/// IUNXIT (the Lovers: it joined) · DECUCURRIT (the Chariot: it coursed
/// on) · DOMUIT (Strength: it tamed) · SECESSIT (the Hermit: it
/// withdrew) · VOLVIT (the Wheel of Fortune: it rolled) · LIBRAVIT
/// (Justice: it weighed out) · INVERTIT (the Hanged Man: it inverted) ·
/// MESSUIT (Death: it reaped) · MISCUIT (Temperance: it mingled) · VINXIT
/// (the Devil: it bound) · DEIECIT (the Tower: it threw down) · REFECIT
/// (the Star: it restored) · ERRAVIT (the Moon: it wandered) · ILLUXIT
/// (the Sun: the light came) · EVOCAVIT (Judgement: it summoned) ·
/// PERFECIT (the World: it completed)
pub const DONE: [&str; 22] = [
    "INSILIIT", "INSTRUXIT", "INTUITA EST", "COLUIT", "ORDINAVIT", "INITIAVIT",
    "IUNXIT", "DECUCURRIT", "DOMUIT", "SECESSIT", "VOLVIT", "LIBRAVIT", "INVERTIT",
    "MESSUIT", "MISCUIT", "VINXIT", "DEIECIT", "REFECIT", "ERRAVIT", "ILLUXIT",
    "EVOCAVIT", "PERFECIT",
];

/// The settle phrase after `calls` tool calls: `calls % 22` walks the
/// arcana, and zero is the Fool without a special case — the unnumbered
/// card is where a turn that never reached for a tool belongs.
pub fn done(calls: i64) -> &'static str {
    DONE[(calls.max(0) as usize) % DONE.len()]
}

/// The mark a **settled** turn wears: the star in place of the bullet
/// every other line of harness talk leads with, turned over once per
/// turn beside the word, so two finished turns are told apart by the
/// mark as well as by the Latin.
///
/// The heavy end of the dingbat stars, because at a terminal's size the
/// light six-pointed ones read as a speck — `✹ ✺ ✦ ✸ ❋` fill the cell
/// where `✶` left two thirds of it empty. All five are one cell wide in
/// every font, and deliberately not the family's emoji members (`✳ ✴`),
/// which a terminal is free to paint as a picture and shove the line
/// along by a column.
pub const STARS: [&str; 5] = ["✹", "✺", "✦", "✸", "❋"];

/// The star the **`turn`th** settled turn wears, counting from zero: one
/// per turn and never the same two turns running, which is the whole of
/// what a mark that rotates is for. The word above it turns on the
/// tool-call count instead, so the two do not step together and a star
/// comes back before its word does.
pub fn star(turn: usize) -> &'static str {
    STARS[turn % STARS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The star a finished turn wears turns over once a turn, and no two
    /// turns run under the same one.
    #[test]
    fn the_star_turns_over_once_a_turn() {
        let cycle: Vec<&str> = (0..STARS.len()).map(star).collect();
        let unique: std::collections::BTreeSet<&str> = cycle.iter().copied().collect();
        assert_eq!(unique.len(), STARS.len(), "a star came round early: {cycle:?}");
        for turn in 0..STARS.len() * 3 {
            assert_ne!(star(turn), star(turn + 1), "two turns running wore {}", star(turn));
        }
        assert_eq!(star(STARS.len()), star(0), "the table does not wrap");
    }

    /// Narrow, and off the dingbat family's emoji half: a star the
    /// terminal is free to paint as a picture would push the line along
    /// by a column on exactly the machines that render the rest right.
    #[test]
    fn every_star_is_one_narrow_cell() {
        const EMOJI: [char; 6] = ['\u{2728}', '\u{2733}', '\u{2734}', '\u{2744}', '\u{2747}', '\u{27bf}'];
        for s in STARS {
            assert_eq!(s.chars().count(), 1, "{s:?} is not one character");
            let c = s.chars().next().unwrap();
            assert!(
                ('\u{2700}'..='\u{27bf}').contains(&c),
                "{s:?} (U+{:04X}) is outside the dingbats",
                c as u32
            );
            assert!(!EMOJI.contains(&c), "{s:?} is one a terminal may draw as a picture");
        }
    }

    #[test]
    fn a_world_never_settles() {
        let mut w = World::new(12);
        let mut last = vec![w.render()];
        for _ in 0..200 {
            w.step();
            let now = w.render();
            assert!(now.chars().any(|c| c != '\u{2800}'), "went dark");
            assert!(!(last.len() >= 3 && last[last.len() - 3..].iter().all(|r| *r == now)), "still for four frames: {now}");
            last.push(now);
        }
        assert_eq!(w.render().chars().count(), 12);
        assert!(w.render().chars().all(|c| ('\u{2800}'..='\u{28FF}').contains(&c)));
    }

    #[test]
    fn the_icon_is_the_figure_the_strip_starts_from() {
        let icon = icon();
        assert_eq!(icon.chars().count(), 2, "the agent is two cells wide: {icon:?}");
        assert!(icon.chars().all(|c| ('\u{2800}'..='\u{28ff}').contains(&c)), "{icon:?}");
        assert!(World::new(12).render().contains(&icon), "the strip does not open on it");
    }

    #[test]
    fn a_turn_opens_on_the_agent_where_the_icon_stands() {
        let mut w = World::new(12);
        let opening = w.render();
        assert!(opening.starts_with(&icon()), "the strip does not open where the icon is: {opening:?}");
        // It evolves, and when it stops it stands up again — so the
        // check is that some frame differs, not that the last one does.
        let mut moved = false;
        for _ in 0..8 {
            w.step();
            moved |= w.render() != opening;
        }
        assert!(moved, "the agent never moved");
        w.restart();
        assert_eq!(w.render(), opening, "a new turn did not start again from it");
    }

    #[test]
    fn a_call_and_its_result_meet_in_the_middle() {
        let mut w = World { cells: vec![false; WORLD * 24], cols: 24, prev: [String::new(), String::new()], launches: 0 };
        w.call();
        w.result();
        let centre = |w: &World| {
            let live: Vec<usize> = (0..WORLD * 24).filter(|i| w.cells[*i]).map(|i| i % 24).collect();
            (live.iter().min().copied().unwrap(), live.iter().max().copied().unwrap())
        };
        let (l0, r0) = centre(&w);
        for _ in 0..8 {
            w.cells = step(&w.cells, 24);
        }
        let (l1, r1) = centre(&w);
        assert!(l1 > l0 && r1 < r0, "the ships did not close in: {l0}..{r0} then {l1}..{r1}");
    }

    /// A call enters on the left and a result on the right, each from its
    /// own end and neither from the other's.
    #[test]
    fn a_call_enters_left_and_a_result_right() {
        let edges = |f: fn(&mut World)| {
            let mut w = World { cells: vec![false; WORLD * 24], cols: 24, prev: [String::new(), String::new()], launches: 0 };
            f(&mut w);
            let live: Vec<usize> = (0..WORLD * 24).filter(|i| w.cells[*i]).map(|i| i % 24).collect();
            (live.iter().min().copied().unwrap(), live.iter().max().copied().unwrap())
        };
        let (l, r) = edges(World::call);
        assert!(l == 0 && r < 12, "a call is not on the left edge: {l}..{r}");
        let (l, r) = edges(World::result);
        assert!(r == 23 && l > 12, "a result is not on the right edge: {l}..{r}");
    }

    #[test]
    fn a_call_changes_the_world() {
        let mut w = World::new(12);
        for _ in 0..12 {
            w.step();
        }
        let before = w.render();
        w.call();
        assert_ne!(before, w.render());
    }

    #[test]
    fn a_blinker_blinks() {
        let cols = 8;
        let mut w = vec![false; WORLD * cols];
        for c in 2..5 {
            w[cols + c] = true;
        }
        let one = step(&w, cols);
        assert!(one[3] && one[cols + 3] && one[2 * cols + 3] && !one[cols + 2]);
        assert_eq!(step(&one, cols), w);
    }
}
