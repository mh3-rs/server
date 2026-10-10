//! A quest bot's body in a hunt (`roadmap/experiment-bots.md`, "The long hunt, decoded"): where it is, which way it
//! faces, its area and what it's doing, as the player messages it sends say it. Pure: a target and a clock in, game
//! messages out (`link.rs` frames them). A hunter walks with its full state (kind 1, only when its action changes:
//! walk or run, stop, idle) and its position (kind 2, every 0.5 s); it enters an area with kind 3.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut};

/// A player message's kind (the byte after the group): a member's spawn.
const SPAWN: u8 = 5;

/// A hunter's full state, recorded (dev2, idle at the camp, the long hunt): the bot's own patched in.
const STATE_TEMPLATE: &str = "0001020100000000c4d947e9c2a86ee3c41c439c000000000000b556000000000200000000003801006400640258000002000000427000000100000000640000000000000000000000000000";
/// A hunter's position, recorded (the joiner's, s05 and s11).
const POSITION_TEMPLATE: &str = "00010202c48da615438380004428451a000000000000ac17000000000100006400640258006400000000000000000000";
/// An area entry, recorded (dev2 walking into area 1, the long hunt).
const ENTRY_TEMPLATE: &str =
    "00010203452aa0000000000044610000000000000000ac1700000000020100010000000000010000";

/// Where the fields are, from a message's start (its 4-byte header included).
const STATE_PARAM: usize = 0x04;
const STATE_POSITION: usize = 0x08;
const STATE_FACING: usize = 0x1a;
const STATE_MAP: usize = 0x20;
const STATE_AREA: usize = 0x21;
const STATE_ACTION: usize = 0x23;
const STATE_SEED: usize = 0x26;
const POSITION_AT: usize = 0x04;
const FACING_AT: usize = 0x16;
const MAP_AT: usize = 0x1c;
pub const AREA_AT: usize = 0x1d;
const ENTRY_HOW: usize = 0x1f;

/// Actions (group, number).
const IDLE: (u8, u16) = (0, 0);
const WALK: (u8, u16) = (1, 1);
const RUN: (u8, u16) = (1, 2);
const STOP: (u8, u16) = (1, 3);
/// A full state's parameter on an area entry.
const ENTERED: u16 = 0x0c;

/// Speeds (the game's units a second, measured); how close counts as there.
const WALK_SPEED: f32 = 440.0;
const NEAR: f32 = 60.0;
/// Position messages' period; a stop's idle comes this long after it.
const POSITION_EVERY: Duration = Duration::from_millis(500);
const STOP_TO_IDLE: Duration = Duration::from_millis(500);
/// How long an arrived bot waits for its next target before stopping: just over the brain's step (0.5 s).
const STOP_GRACE: Duration = Duration::from_millis(600);
/// A target further than this is jumped to (placed there, as on entering an area), not walked to.
const JUMP_FROM: f32 = 3000.0;
/// The bot covers each stretch in the time since its previous target (the brain's rhythm: its pace is the hunter's),
/// within these bounds (0.5 s from still); the gait only picks the animation. Never faster than a sprint.
const TARGET_SOONEST: Duration = Duration::from_millis(50);
const TARGET_LATEST: Duration = Duration::from_millis(500);
const SPRINT_SPEED: f32 = 1100.0;

pub struct Body {
    index: u16,
    position: [f32; 3],
    facing: u16,
    area: u8,
    map: u8,
    /// Walking or running there.
    target: Option<[f32; 3]>,
    running: bool,
    /// Its pace to the current target (units a second), and when that target came.
    speed: f32,
    last_target: Instant,
    /// The area the next move enters (it starts there).
    entering: Option<(u8, u8)>,
    /// Arrived, walking on the spot until the next target or this instant (then the stop).
    stop_at: Option<Instant>,
    idle_at: Option<Instant>,
    last_step: Instant,
    last_position: Instant,
}

impl Body {
    /// At the quest's start, on the host's spot (its spawn state, 0105): the bot's spawn answer (0205) and its body.
    pub fn spawn(index: u16, host_spawn: &[u8], now: Instant) -> Option<(Vec<u8>, Body)> {
        if host_spawn.len() < MAP_AT + 2 {
            return None;
        }
        let mut spawn = host_spawn.to_vec();
        header(&mut spawn, index);
        spawn[3] = SPAWN;
        let body = Body {
            index,
            position: floats(&host_spawn[POSITION_AT..]),
            facing: (&host_spawn[FACING_AT..]).get_u16(),
            area: host_spawn[AREA_AT],
            map: host_spawn[MAP_AT],
            target: None,
            running: false,
            speed: WALK_SPEED,
            last_target: now,
            entering: None,
            stop_at: None,
            idle_at: None,
            last_step: now,
            last_position: now,
        };
        Some((spawn, body))
    }

    /// The next move starts in `area` (`map` its map byte, as hunters there send it).
    pub fn enter(&mut self, area: u8, map: u8) {
        if area != self.area {
            self.entering = Some((area, map));
        }
    }

    /// To `target`, walking or running (the brain decides), facing it; into a new area, or far, it starts there.
    pub fn go(&mut self, target: [f32; 3], running: bool, now: Instant) -> Vec<Vec<u8>> {
        if self.entering.is_none() && distance(self.position, target) > JUMP_FROM {
            self.entering = Some((self.area, self.map));
        }
        if let Some((area, map)) = self.entering.take() {
            (self.position, self.area, self.map, self.target) = (target, area, map, None);
            let mut entry = hex(ENTRY_TEMPLATE);
            self.stamp(&mut entry, POSITION_AT, FACING_AT, MAP_AT);
            entry[ENTRY_HOW] = 1;
            return vec![entry, self.state(IDLE, ENTERED)];
        }
        let distance = distance(self.position, target);
        let still = self.target.is_none() && self.stop_at.is_none();
        // Already there (still, or arrived and waiting): no walk. A stopped hunter's positions keep coming every 0.5 s,
        // and resuming on each would keep the bot walking on the spot (lab).
        if self.target.is_none() && distance < NEAR {
            return Vec::new();
        }
        // A new walk (or run) when it was still, or switching between the two; otherwise the walk goes on, retargeted
        // (also just after arriving: no stop and start again between two of a hunter's positions).
        let starting = still || running != self.running;
        if self.target.is_none() {
            self.last_step = now;
        }
        self.facing = facing(self.position, target);
        let since = match still {
            true => TARGET_LATEST,
            false => now
                .duration_since(self.last_target)
                .clamp(TARGET_SOONEST, TARGET_LATEST),
        };
        self.last_target = now;
        self.speed = (distance / since.as_secs_f32()).clamp(WALK_SPEED, SPRINT_SPEED);
        self.target = Some(target);
        (self.stop_at, self.idle_at) = (None, None);
        self.running = running;
        match starting {
            true => vec![self.state(if running { RUN } else { WALK }, 0)],
            false => Vec::new(),
        }
    }

    /// The clock: the body moves on, stops on arrival and idles after; its position every 0.5 s.
    pub fn step(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if let Some(target) = self.target {
            let reach = self.speed * now.duration_since(self.last_step).as_secs_f32();
            self.last_step = now;
            let left = distance(self.position, target);
            if left <= reach {
                self.position = target;
                self.target = None;
                self.stop_at = Some(now + STOP_GRACE);
            } else {
                for (p, t) in self.position.iter_mut().zip(target) {
                    *p += (t - *p) * reach / left;
                }
            }
        }
        if self.stop_at.is_some_and(|at| now >= at) {
            self.stop_at = None;
            self.idle_at = Some(now + STOP_TO_IDLE);
            out.push(self.state(STOP, 0));
        }
        if self.idle_at.is_some_and(|at| now >= at) {
            self.idle_at = None;
            out.push(self.state(IDLE, 0));
        }
        if now.duration_since(self.last_position) >= POSITION_EVERY {
            self.last_position = now;
            let mut position = hex(POSITION_TEMPLATE);
            self.stamp(&mut position, POSITION_AT, FACING_AT, MAP_AT);
            out.push(position);
        }
        out
    }

    /// A full state with `action`.
    fn state(&self, (group, number): (u8, u16), param: u16) -> Vec<u8> {
        let mut state = hex(STATE_TEMPLATE);
        header(&mut state, self.index);
        state[STATE_PARAM..STATE_PARAM + 2].copy_from_slice(&param.to_be_bytes());
        put_floats(&mut state[STATE_POSITION..], self.position);
        state[STATE_FACING..STATE_FACING + 2].copy_from_slice(&self.facing.to_be_bytes());
        (state[STATE_MAP], state[STATE_AREA]) = (self.map, self.area);
        state[STATE_ACTION] = group;
        state[STATE_ACTION + 1..STATE_ACTION + 3].copy_from_slice(&number.to_be_bytes());
        // Random in every recorded one.
        let mut seed = [0u8; 1];
        openssl::rand::rand_bytes(&mut seed).expect("OpenSSL random source");
        state[STATE_SEED] = seed[0];
        state
    }

    /// Our index, position, facing, map and area into a position-shaped message (kinds 2 and 3).
    fn stamp(&self, message: &mut [u8], position: usize, facing: usize, map: usize) {
        header(message, self.index);
        put_floats(&mut message[position..], self.position);
        message[facing..facing + 2].copy_from_slice(&self.facing.to_be_bytes());
        (message[map], message[map + 1]) = (self.map, self.area);
    }
}

/// A player message's header: the sender's index, then its group (its slot, from 1).
fn header(message: &mut [u8], index: u16) {
    message[..2].copy_from_slice(&index.to_be_bytes());
    message[2] = index as u8 + 1;
}

/// Which way to face to look from `from` to `to`: 0x10000 a full turn, `atan2(dx, dz)` (measured).
fn facing(from: [f32; 3], to: [f32; 3]) -> u16 {
    let turn = (to[0] - from[0]).atan2(to[2] - from[2]) / TAU;
    (turn.rem_euclid(1.0) * 65536.0) as u32 as u16
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f32>()
        .sqrt()
}

fn floats(mut bytes: &[u8]) -> [f32; 3] {
    [bytes.get_f32(), bytes.get_f32(), bytes.get_f32()]
}

fn put_floats(mut bytes: &mut [u8], floats: [f32; 3]) {
    floats.iter().for_each(|f| bytes.put_f32(*f));
}

fn hex(s: &str) -> Vec<u8> {
    ::hex::decode(s).expect("a recorded message")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_STATE: u8 = 1;
    const POSITION: u8 = 2;
    const AREA_ENTRY: u8 = 3;
    const HOST_SPAWN: &str =
        "00000105c48020004383800044200000000000000000ac1700000000010000000000000000010000";

    fn kinds(messages: &[Vec<u8>]) -> Vec<(u8, u8, u16)> {
        messages
            .iter()
            .map(|m| match m[3] {
                FULL_STATE => (m[3], m[STATE_ACTION], (&m[STATE_ACTION + 1..]).get_u16()),
                kind => (kind, 0, 0),
            })
            .collect()
    }

    #[test]
    fn facing_is_measured_like_the_game() {
        // The recorded straight run: (-1362, -624) to (110, -229), facing 0x3556.
        assert_eq!(
            facing([-1362.0, 0.0, -624.0], [110.0, 0.0, -229.0]) >> 4,
            0x3556 >> 4
        );
        assert_eq!(facing([0.0; 3], [0.0, 0.0, 1.0]), 0);
        assert_eq!(facing([0.0; 3], [-1.0, 0.0, 0.0]), 0xc000);
    }

    /// The actions among messages (full states only).
    fn actions(messages: &[Vec<u8>]) -> Vec<(u8, u16)> {
        kinds(messages)
            .into_iter()
            .filter(|(kind, _, _)| *kind == FULL_STATE)
            .map(|(_, group, number)| (group, number))
            .collect()
    }

    #[test]
    fn the_bot_walks_stops_and_idles() {
        let t0 = Instant::now();
        let (spawn, mut body) = Body::spawn(1, &::hex::decode(HOST_SPAWN).unwrap(), t0).unwrap();
        assert_eq!(&spawn[..4], [0, 1, 2, 5]);
        // A third member: index 2, group 3.
        let (third, _) = Body::spawn(2, &::hex::decode(HOST_SPAWN).unwrap(), t0).unwrap();
        assert_eq!(&third[..4], [0, 2, 3, 5]);
        let ms = |ms: u64| t0 + Duration::from_millis(ms);
        // Near: nothing. Told to walk further: a walk, facing there.
        let start = body.position;
        assert!(
            body.go([start[0] + 10.0, start[1], start[2]], false, t0)
                .is_empty()
        );
        let target = [start[0] + 220.0, start[1], start[2]];
        assert_eq!(actions(&body.go(target, false, t0)), [(1, 1)]);
        assert_eq!(body.facing, 0x4000);
        // From still, a stretch takes 0.5 s: halfway at 0.25 s; there at 0.5 s, its position sent (every 0.5 s, as
        // consoles do).
        assert!(body.step(ms(250)).is_empty());
        assert!((body.position[0] - (start[0] + 110.0)).abs() < 1.0);
        let there = body.step(ms(500));
        assert_eq!(kinds(&there), [(POSITION, 0, 0)]);
        assert_eq!(floats(&there[0][POSITION_AT..]), target);
        // The next target in time: the walk goes on, no stop and start.
        let further = [target[0] + 110.0, target[1], target[2]];
        assert!(body.go(further, false, ms(750)).is_empty());
        assert!(actions(&body.step(ms(1000))).is_empty());
        assert_eq!(body.position, further);
        // The same spot again (a still hunter): still waiting, then stopping all the same, 0.6 s after arriving;
        // idle 0.5 s later.
        assert!(body.go(further, false, ms(1100)).is_empty());
        assert!(actions(&body.step(ms(1500))).is_empty());
        assert_eq!(actions(&body.step(ms(1600))), [(1, 3)]);
        assert!(actions(&body.step(ms(2000))).is_empty());
        assert_eq!(actions(&body.step(ms(2100))), [(0, 0)]);
        // Told to run: a run, at the pace the stretch needs (a sprint's 500 units in 0.5 s from still).
        let ahead = [further[0] + 500.0, further[1], further[2]];
        assert_eq!(actions(&body.go(ahead, true, ms(2200))), [(1, 2)]);
        body.step(ms(2700));
        assert_eq!(body.position, ahead);
    }

    #[test]
    fn the_bot_keeps_the_pace_of_targets_every_tenth_of_a_second() {
        let t0 = Instant::now();
        let (_, mut body) = Body::spawn(1, &::hex::decode(HOST_SPAWN).unwrap(), t0).unwrap();
        let start = body.position;
        // A run's pace (730 units a second): 73 units every 0.1 s, as the brain replays it.
        for i in 1..=10u64 {
            let now = t0 + Duration::from_millis(100 * i);
            body.step(now);
            body.go([start[0] + 73.0 * i as f32, start[1], start[2]], true, now);
        }
        // A tenth of a second after the last: there, not trailing.
        body.step(t0 + Duration::from_millis(1100));
        assert!(
            (body.position[0] - (start[0] + 730.0)).abs() < 1.0,
            "{:?}",
            body.position
        );
    }

    #[test]
    fn the_bot_enters_an_area_where_it_goes() {
        let t0 = Instant::now();
        let (_, mut body) = Body::spawn(1, &::hex::decode(HOST_SPAWN).unwrap(), t0).unwrap();
        body.enter(1, 2);
        let entered = body.go([2622.0, 8.0, 933.0], false, t0);
        assert_eq!(kinds(&entered), [(AREA_ENTRY, 0, 0), (FULL_STATE, 0, 0)]);
        assert_eq!(
            (
                entered[0][MAP_AT],
                entered[0][AREA_AT],
                entered[0][ENTRY_HOW]
            ),
            (2, 1, 1)
        );
        assert_eq!(
            (
                entered[1][STATE_AREA],
                (&entered[1][STATE_PARAM..]).get_u16()
            ),
            (1, ENTERED)
        );
        assert_eq!(body.position, [2622.0, 8.0, 933.0]);
        // Far in the same area: jumped to, not walked.
        let far = body.go([9000.0, 8.0, 933.0], true, t0 + Duration::from_millis(500));
        assert_eq!(kinds(&far), [(AREA_ENTRY, 0, 0), (FULL_STATE, 0, 0)]);
        assert_eq!(body.position, [9000.0, 8.0, 933.0]);
        // Its positions say the new area.
        assert_eq!(body.step(t0 + Duration::from_secs(1))[0][AREA_AT], 1);
    }
}
