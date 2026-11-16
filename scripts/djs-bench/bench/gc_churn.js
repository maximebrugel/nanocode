function main(){ let s = 0; for (let i = 0; i < 1000000; i++){ const o = { a: [i, i + 1], b: 'x' }; s = (s + o.a[1]) | 0; } return s; }
