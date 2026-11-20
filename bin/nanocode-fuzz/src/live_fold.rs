use nanocode_live_fold::{
    Access, Ack, ClientFrame, Cursor, Encoded, ErrCode, Failure, Intent, Op, Open, Row,
    ServerFrame, Snapshot, ViewKey, decode_client, decode_server, encode, snapshot_frame,
};
use serde_json::{Map, Number, Value};

use crate::{Checked, rng::Rng};

const WORDS: [&str; 6] = ["tasks", "rows", "", "a/b", "k\u{e9}y", "\"q\""];

fn value(rng: &mut Rng, depth: u64) -> Value {
    match rng.below(if depth == 0 { 6 } else { 8 }) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(1, 2)),
        2 => Value::Number(Number::from(rng.next())),
        3 => Value::Number(Number::from(i64::from_le_bytes(rng.next().to_le_bytes()))),
        4 => Number::from_f64(f64::from_bits(rng.next())).map_or(Value::Null, Value::Number),
        5 => Value::String(rng.text(12)),
        6 => Value::Array((0..rng.below(4)).map(|_| value(rng, depth - 1)).collect()),
        _ => Value::Object(
            (0..rng.below(4))
                .map(|_| (rng.text(6), value(rng, depth - 1)))
                .collect::<Map<String, Value>>(),
        ),
    }
}

fn text(rng: &mut Rng) -> String {
    if rng.chance(1, 2) {
        rng.word(&WORDS)
    } else {
        rng.text(10)
    }
}

fn small(rng: &mut Rng) -> u32 {
    u32::from_le_bytes(
        rng.next().to_le_bytes()[..4]
            .try_into()
            .expect("four bytes"),
    )
}

fn cursor(rng: &mut Rng) -> Cursor {
    Cursor::new(small(rng), rng.next(), rng.next())
}

fn view(rng: &mut Rng) -> ViewKey {
    (0..rng.below(3)).fold(ViewKey::new(text(rng)), |view, _| {
        view.arg(text(rng), text(rng))
    })
}

fn row(rng: &mut Rng) -> Row {
    Row {
        table: text(rng),
        key: text(rng),
        value: value(rng, 3),
    }
}

fn op(rng: &mut Rng) -> Op {
    if rng.chance(2, 3) {
        Op::Upsert {
            table: text(rng),
            key: text(rng),
            value: value(rng, 3),
        }
    } else {
        Op::Delete {
            table: text(rng),
            key: text(rng),
        }
    }
}

fn client(rng: &mut Rng) -> ClientFrame {
    match rng.below(3) {
        0 => ClientFrame::Sub {
            id: small(rng),
            view: view(rng),
            cursor: rng.chance(1, 2).then(|| cursor(rng)),
        },
        1 => ClientFrame::Unsub { id: small(rng) },
        _ => ClientFrame::Mut(Intent {
            client: text(rng),
            mid: rng.next(),
            name: text(rng),
            args: value(rng, 3),
        }),
    }
}

fn server(rng: &mut Rng) -> ServerFrame {
    match rng.below(5) {
        0 => ServerFrame::Snap {
            id: small(rng),
            cursor: cursor(rng),
            rows: (0..rng.below(4)).map(|_| row(rng)).collect(),
        },
        1 => ServerFrame::Diff {
            id: small(rng),
            from: cursor(rng),
            to: cursor(rng),
            ops: (0..rng.below(4)).map(|_| op(rng)).collect(),
        },
        2 => ServerFrame::Ack(Ack {
            client: text(rng),
            mid: rng.next(),
            result: if rng.chance(1, 2) {
                Ok(cursor(rng))
            } else {
                Err(Failure {
                    code: text(rng),
                    message: text(rng),
                })
            },
        }),
        3 => ServerFrame::Eph {
            id: small(rng),
            body: value(rng, 3),
        },
        _ => ServerFrame::Err {
            id: rng.chance(1, 2).then(|| small(rng)),
            code: *rng.pick(&[
                ErrCode::Denied,
                ErrCode::UnknownView,
                ErrCode::BadFrame,
                ErrCode::Overloaded,
            ]),
            message: text(rng),
        },
    }
}

fn stable<T: PartialEq + std::fmt::Debug + serde::Serialize>(
    decoded: Result<T, serde_json::Error>,
    decode: fn(&str) -> Result<T, serde_json::Error>,
    input: &str,
) -> Checked {
    if let Ok(frame) = decoded {
        let again = encode(&frame);
        ensure!(
            decode(&again).as_ref().ok() == Some(&frame),
            "{input:?} decoded to {frame:?} which does not round-trip"
        );
    }
    Ok(())
}

fn frames(rng: &mut Rng) -> Checked {
    let sent = client(rng);
    let wire = encode(&sent);
    ensure!(
        decode_client(&wire).ok().as_ref() == Some(&sent),
        "client frame {wire} does not round-trip"
    );
    ensure!(
        decode_server(&wire).is_err(),
        "client frame {wire} decoded as a server frame"
    );
    let mangled = rng.mutate(&wire);
    stable(decode_client(&mangled), decode_client, &mangled)?;
    let answer = server(rng);
    let wire = encode(&answer);
    ensure!(
        decode_server(&wire).ok().as_ref() == Some(&answer),
        "server frame {wire} does not round-trip"
    );
    ensure!(
        decode_client(&wire).is_err(),
        "server frame {wire} decoded as a client frame"
    );
    let mangled = rng.mutate(&wire);
    stable(decode_server(&mangled), decode_server, &mangled)?;
    let garbage = String::from_utf8_lossy(&rng.bytes(32)).into_owned();
    stable(decode_client(&garbage), decode_client, &garbage)?;
    stable(decode_server(&garbage), decode_server, &garbage)
}

fn acks(rng: &mut Rng) -> Checked {
    let point = encode(&cursor(rng));
    let failure = encode(&Failure {
        code: text(rng),
        message: text(rng),
    });
    let fields = match rng.below(2) {
        0 => format!(",\"cursor\":{point},\"error\":{failure}"),
        _ => String::new(),
    };
    let wire = format!("{{\"t\":\"ack\",\"client\":\"c\",\"mid\":1{fields}}}");
    ensure!(
        decode_server(&wire).is_err(),
        "ambiguous ack {wire} decoded"
    );
    Ok(())
}

fn sealed(rng: &mut Rng) -> Checked {
    let id = small(rng);
    let (from, to) = (cursor(rng), cursor(rng));
    let ops: Vec<Op> = (0..rng.below(4)).map(|_| op(rng)).collect();
    let frame = Encoded::diff(&from, &to, &ops).frame(id);
    let expected = ServerFrame::Diff { id, from, to, ops };
    ensure!(
        decode_server(&frame).ok().as_ref() == Some(&expected),
        "encoded diff {frame} does not decode to {expected:?}"
    );
    let snapshot = Snapshot {
        view: view(rng),
        cursor: cursor(rng),
        rows: (0..rng.below(4)).map(|_| row(rng)).collect(),
    };
    let access = *rng.pick(&[Access::Full, Access::Outline, Access::Denied]);
    let encoded = snapshot_frame(&snapshot, access, &Open);
    ensure!(
        encoded.is_none() == (access == Access::Denied),
        "{access:?} snapshot framed as {encoded:?}"
    );
    if let Some(encoded) = encoded {
        let frame = encoded.frame(id);
        let expected = ServerFrame::Snap {
            id,
            cursor: snapshot.cursor,
            rows: snapshot.rows,
        };
        ensure!(
            decode_server(&frame).ok().as_ref() == Some(&expected),
            "encoded snapshot {frame} does not decode to {expected:?}"
        );
    }
    Ok(())
}

pub fn check(rng: &mut Rng) -> Checked {
    frames(rng)?;
    acks(rng)?;
    sealed(rng)
}
