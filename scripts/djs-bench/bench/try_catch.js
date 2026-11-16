function thrower(i){ if (i % 4 === 0) throw i; return i; }
function main(){ let s = 0; for (let i = 0; i < 400000; i++){ try { s += thrower(i); } catch (e) { s -= 1; } finally { s += 1; } } return s; }
