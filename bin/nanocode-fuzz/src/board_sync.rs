use std::collections::BTreeMap;
use std::sync::Arc;

use nanocode_board::{
    Board, BoardError, NewTask, Op, TaskId,
    access::{Identity, Verifier},
    tasks::Terms,
};
use nanocode_board_sync::{
    Accounts, Author, Channels, Engine, Event, Inbound, Mode, Plain, Settle, Target,
};
use nanocode_sync::block_on;
use nanocode_turnlog::{LogHost, MemoryLog};

use crate::{Checked, rng::Rng};

const SCOPE: &str = "fuzz";
const SYNCS: [&str; 2] = ["slack", "github"];
const THREADS: [&str; 4] = ["c1", "c1:1", "c1:2", "c2:5"];
const USERS: [&str; 3] = ["u1", "u2", "u3"];
const TASKS: [&str; 4] = ["t0", "t1", "slack/c1:1", "github/c2:5"];
const KEYS: [&str; 6] = ["k0", "k1", "k2", "k3", "k4", "k5"];

struct Tokens;

impl Verifier for Tokens {
    fn verify(&self, credential: &str) -> Option<String> {
        credential.strip_prefix("token:").map(str::to_owned)
    }
}

fn target(rng: &mut Rng) -> Target {
    Target::new(rng.pick(&SYNCS), rng.word(&THREADS))
}

fn author(rng: &mut Rng) -> Author {
    Author {
        user: rng.word(&USERS),
        name: rng.text(4),
        bot: rng.chance(1, 4),
    }
}

const fn mode(rng: &mut Rng) -> Mode {
    if rng.chance(1, 2) {
        Mode::Always
    } else {
        Mode::Status
    }
}

fn labels(rng: &mut Rng) -> BTreeMap<String, String> {
    (0..rng.below(3))
        .map(|_| (rng.word(&["k", "j", "source"]), rng.text(3)))
        .collect()
}

fn event(rng: &mut Rng) -> Event {
    match rng.below(10) {
        0 => Event::Opened {
            task: NewTask {
                id: TaskId(rng.word(&TASKS)),
                parent: None,
                spec: rng.text(6),
                labels: labels(rng),
                needs: Vec::new(),
                terms: Terms::default(),
            },
            thread: target(rng),
            message: rng.word(&["m1", "m2", "m3"]),
            mode: mode(rng),
        },
        1 | 2 => Event::Message {
            thread: target(rng),
            message: rng.word(&["m1", "m2", "m3", "m4"]),
            author: author(rng),
            body: if rng.chance(1, 3) {
                format!("@bot {}", rng.text(4))
            } else {
                rng.text(8)
            },
            context: rng.chance(1, 4).then(|| rng.text(4)),
            sub: rng.chance(1, 4).then(|| rng.text(3)),
            marker: None,
        },
        3 => Event::Edited {
            thread: target(rng),
            message: rng.word(&["m1", "m2"]),
            body: rng.text(6),
            version: rng.below(5),
        },
        4 => Event::Deleted {
            thread: target(rng),
            message: rng.word(&["m1", "m2"]),
        },
        5 => Event::Reworded {
            thread: target(rng),
            author: author(rng),
            title: rng.text(5),
            body: rng.chance(1, 2).then(|| rng.text(5)),
            version: rng.below(5),
        },
        6 => Event::Reaction {
            thread: target(rng),
            message: rng.word(&["m1", "m2"]),
            author: author(rng),
            emoji: rng.word(&["+1", "eyes"]),
        },
        7 => Event::Clicked {
            thread: target(rng),
            message: rng.word(&["m1", "m2"]),
            author: author(rng),
            control: rng.word(&["ok", "no"]),
            values: labels(rng),
        },
        8 => Event::Labels {
            task: TaskId(rng.word(&TASKS)),
            set: labels(rng),
            unset: (0..rng.below(2)).map(|_| rng.word(&["k", "j"])).collect(),
            own: (0..rng.below(2)).map(|_| rng.word(&["k", "j"])).collect(),
            version: rng.below(5),
        },
        _ => {
            if rng.chance(1, 2) {
                Event::Closed {
                    task: TaskId(rng.word(&TASKS)),
                    outcome: if rng.chance(1, 2) {
                        Settle::Done(rng.text(3))
                    } else {
                        Settle::Cancelled(rng.text(3))
                    },
                }
            } else {
                Event::Unlinked {
                    task: TaskId(rng.word(&TASKS)),
                    thread: target(rng),
                }
            }
        }
    }
}

fn replayed(host: &Arc<MemoryLog>, writer: &Identity) -> Result<Board, String> {
    block_on(Board::open(
        Arc::clone(host) as Arc<dyn LogHost>,
        SCOPE,
        writer.clone(),
    ))
    .map_err(|error| format!("reopening the log failed: {error}"))
}

pub fn check(rng: &mut Rng) -> Checked {
    let host = Arc::new(MemoryLog::new());
    let writer =
        Identity::verify(&Tokens, "token:agent:sync").map_err(|error| format!("{error:?}"))?;
    let mut board = replayed(&host, &writer)?;
    if rng.chance(15, 16) {
        block_on(board.submit(0, Op::Charter {}))
            .map_err(|error| format!("charter failed: {error}"))?;
    }
    let mirrored = SYNCS
        .iter()
        .flat_map(|sync| ["c1", "c2"].map(|channel| Target::new(sync, channel)))
        .filter_map(|channel| rng.chance(1, 2).then(|| (channel, mode(rng))))
        .collect();
    let bot = SYNCS
        .iter()
        .filter(|_| rng.chance(1, 2))
        .map(|sync| ((*sync).to_owned(), "bot".to_owned()))
        .collect();
    let accounts = SYNCS
        .iter()
        .flat_map(|sync| USERS.map(|user| ((*sync).to_owned(), user.to_owned())))
        .filter(|_| rng.chance(1, 2))
        .map(|account| {
            let member = format!("user:{}", account.1);
            (account, member)
        })
        .collect();
    let engine = Engine::new(
        Plain,
        Channels {
            mirrored,
            bot,
            start: rng.chance(1, 2),
        },
        Accounts(accounts),
        BTreeMap::new(),
        1,
    );
    let mut sent: Vec<Inbound> = Vec::new();
    let mut at_ms = 1;
    for _ in 0..rng.range(1, 30) {
        at_ms += rng.below(100);
        let inbound = if !sent.is_empty() && rng.chance(1, 4) {
            rng.pick(&sent).clone()
        } else {
            Inbound {
                key: rng.word(&KEYS),
                event: event(rng),
            }
        };
        let first = block_on(engine.ingest(&mut board, at_ms, &inbound));
        if let Err(error) = &first {
            ensure!(
                !matches!(error, BoardError::Corrupt { .. }),
                "{inbound:?} corrupted the board: {error}"
            );
        }
        let reopened = replayed(&host, &writer)?;
        ensure!(
            reopened.fold() == board.fold(),
            "after {inbound:?} ({first:?}) the board differs from its log"
        );
        let before = board.fold().clone();
        let second = block_on(engine.ingest(&mut board, at_ms + 1, &inbound));
        ensure!(
            second.is_ok() == first.is_ok(),
            "ingesting {inbound:?} twice changed its verdict ({first:?} then {second:?})"
        );
        ensure!(
            board.fold().mirror() == before.mirror(),
            "ingesting {inbound:?} twice changed the mirror ({first:?} then {second:?})"
        );
        let settled = board.fold().clone();
        let third = block_on(engine.ingest(&mut board, at_ms + 2, &inbound));
        ensure!(
            third.is_ok() == first.is_ok(),
            "ingesting {inbound:?} thrice changed its verdict ({first:?} then {third:?})"
        );
        ensure!(
            board.fold() == &settled,
            "ingesting {inbound:?} a third time changed the board ({second:?} then {third:?})"
        );
        sent.push(inbound);
    }
    Ok(())
}
