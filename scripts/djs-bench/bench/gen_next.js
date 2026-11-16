function* g(){ let i = 0; while (true) yield i++; }
function main(){ const it = g(); let s = 0; for (let i = 0; i < 1000000; i++) s += it.next().value; return s; }
