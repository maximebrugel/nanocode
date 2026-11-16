function main(){ let s = 0; for (let i = 0; i < 1000000; i++){ const o = { a: i, b: i + 1, c: i + 2 }; s = (s + o.a + o.b * 2 + o.c) | 0; } return s; }
