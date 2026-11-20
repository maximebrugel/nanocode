use std::collections::BTreeSet;
use std::num::NonZeroU32;

use nanocode_partitions::{Partition, Refused, Row, Token, assignee, assignment, partition_of};

use crate::{Checked, rng::Rng};

const NODES: [&str; 5] = ["n0", "n1", "n2", "n3", "n4"];

fn rows(rng: &mut Rng) -> Checked {
    let partition = Partition(u32::try_from(rng.below(64)).expect("a small partition fits"));
    let mut row: Row<String> = Row::Free {
        epoch: rng.below(8),
    };
    let mut issued: Vec<(String, u64)> = Vec::new();
    let mut now = rng.below(1_000);
    for _ in 0..rng.range(1, 48) {
        now = now.saturating_add(rng.below(50));
        let node = rng.word(&NODES);
        let ttl = rng.range(1, 100);
        let epoch = if !issued.is_empty() && rng.chance(3, 4) {
            rng.pick(&issued).1
        } else {
            rng.below(row.epoch() + 3)
        };
        let before = row.clone();
        match rng.below(4) {
            0 => {
                let live = rng.chance(1, 2);
                let claimable = row.claimable(&node, now, live);
                match row.claim(&node, now, ttl, live) {
                    Ok((next, raised)) => {
                        ensure!(
                            claimable,
                            "claim of {before:?} by {node} succeeded though not claimable"
                        );
                        ensure!(
                            raised == before.epoch() + 1,
                            "claim raised epoch {} to {raised}",
                            before.epoch()
                        );
                        ensure!(
                            issued.iter().all(|(_, old)| *old < raised),
                            "claim reissued epoch {raised}"
                        );
                        ensure!(
                            next.holder() == Some(&node) && next.epoch() == raised,
                            "claim gave {next:?}"
                        );
                        ensure!(
                            next.fence(Token {
                                partition,
                                epoch: raised
                            })
                            .is_ok(),
                            "fresh token refused by {next:?}"
                        );
                        issued.push((node, raised));
                        row = next;
                    }
                    Err(Refused::Held {
                        holder,
                        epoch,
                        expires_at,
                    }) => {
                        ensure!(
                            !claimable,
                            "claim refused though claimable: {before:?} by {node}"
                        );
                        ensure!(
                            before
                                == Row::Held {
                                    holder,
                                    epoch,
                                    expires_at
                                },
                            "refusal misreports {before:?}"
                        );
                    }
                }
            }
            1 => match row.renew(partition, &node, epoch, now, ttl) {
                Ok(next) => {
                    ensure!(
                        before.holder() == Some(&node) && before.epoch() == epoch,
                        "renew by {node}@{epoch} admitted on {before:?}"
                    );
                    ensure!(
                        next == Row::Held {
                            holder: node,
                            epoch,
                            expires_at: now.saturating_add(ttl)
                        },
                        "renew gave {next:?}"
                    );
                    row = next;
                }
                Err(fenced) => {
                    ensure!(
                        before.holder() != Some(&node) || before.epoch() != epoch,
                        "renew by holder {node}@{epoch} refused on {before:?}"
                    );
                    ensure!(
                        fenced.current == before.epoch()
                            && fenced.held == epoch
                            && fenced.partition == partition,
                        "renew fenced {fenced:?}"
                    );
                }
            },
            2 => match row.release(partition, &node, epoch) {
                Ok(next) => {
                    ensure!(
                        before.holder() == Some(&node) && before.epoch() == epoch,
                        "release by non-holder {node}@{epoch} admitted on {before:?}"
                    );
                    ensure!(next == Row::Free { epoch }, "release gave {next:?}");
                    row = next;
                }
                Err(fenced) => {
                    ensure!(
                        before.holder() != Some(&node) || before.epoch() != epoch,
                        "release by holder {node}@{epoch} refused on {before:?}"
                    );
                    ensure!(
                        fenced.current == before.epoch(),
                        "release fenced {fenced:?}"
                    );
                }
            },
            _ => {
                let token = Token { partition, epoch };
                let admitted = matches!(&row, Row::Held { epoch: held, .. } if *held == epoch);
                ensure!(
                    row.fence(token).is_ok() == admitted,
                    "fence of {token:?} on {row:?} gave the wrong verdict"
                );
            }
        }
        ensure!(
            row.epoch() >= before.epoch(),
            "epoch went back from {before:?} to {row:?}"
        );
        for (_, old) in &issued {
            if *old < row.epoch() {
                ensure!(
                    row.fence(Token {
                        partition,
                        epoch: *old
                    })
                    .is_err(),
                    "stale epoch {old} admitted by {row:?}"
                );
            }
        }
        if let Row::Free { .. } = row {
            ensure!(
                issued.iter().all(|(_, old)| row
                    .fence(Token {
                        partition,
                        epoch: *old
                    })
                    .is_err()),
                "a free row admitted a token"
            );
        }
    }
    Ok(())
}

fn placement(rng: &mut Rng) -> Checked {
    let count = NonZeroU32::new(u32::try_from(rng.range(1, 300)).expect("a small count fits"))
        .expect("a count of at least one");
    let key = rng.bytes(24);
    let partition = partition_of(&key, count);
    ensure!(
        partition.0 < count.get(),
        "partition_of gave {partition:?} for count {count}"
    );
    ensure!(
        partition == partition_of(&key, count),
        "partition_of is not deterministic"
    );
    let live: BTreeSet<String> = NODES
        .iter()
        .filter(|_| rng.chance(2, 3))
        .map(|node| (*node).to_owned())
        .collect();
    let assigned = assignment(count, &live);
    ensure!(
        assigned == assignment(count, &live),
        "assignment is not deterministic"
    );
    ensure!(
        assigned.len()
            == if live.is_empty() {
                0
            } else {
                count.get() as usize
            },
        "assignment covers {} of {count} partitions over {} nodes",
        assigned.len(),
        live.len()
    );
    for (partition, node) in &assigned {
        ensure!(live.contains(node), "{partition:?} assigned to dead {node}");
        ensure!(partition.0 < count.get(), "{partition:?} outside {count}");
        let mut fewer = live.clone();
        let gone = rng.word(&NODES);
        if gone != *node {
            fewer.remove(&gone);
            ensure!(
                assignee(*partition, &fewer) == Some(node),
                "removing {gone} moved {partition:?} off {node}"
            );
        }
    }
    Ok(())
}

pub fn check(rng: &mut Rng) -> Checked {
    rows(rng)?;
    placement(rng)
}
