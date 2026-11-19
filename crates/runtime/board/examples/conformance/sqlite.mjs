// One SQLite connection for the board conformance example: reads one JSON
// request per line ({"statements": [[sql, ...params], ...]}), runs the
// statements in one BEGIN IMMEDIATE transaction and answers {"ok": rows} or
// {"err": message}.
import { DatabaseSync } from 'node:sqlite';
import readline from 'node:readline';

const db = new DatabaseSync(process.argv[2]);
db.exec('PRAGMA busy_timeout = 60000');
db.exec('PRAGMA journal_mode = WAL');
db.exec('PRAGMA synchronous = FULL');

const lines = readline.createInterface({ input: process.stdin });
for await (const line of lines) {
  const request = JSON.parse(line);
  let reply;
  try {
    db.exec('BEGIN IMMEDIATE');
    const results = [];
    for (const [sql, ...params] of request.statements) {
      const statement = db.prepare(sql);
      statement.setReturnArrays(true);
      const rows = statement.all(...params);
      results.push(rows.map((row) => row.map((cell) => (cell === null ? null : String(cell)))));
    }
    db.exec('COMMIT');
    reply = { ok: results };
  } catch (error) {
    if (db.isTransaction) db.exec('ROLLBACK');
    reply = { err: String(error?.message ?? error) };
  }
  process.stdout.write(JSON.stringify(reply) + '\n');
}
