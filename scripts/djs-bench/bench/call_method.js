const o = { n: 0, add(x){ this.n = (this.n + x) | 0; return this; } };
function main(){ for (let i = 0; i < 3000000; i++) o.add(i); return o.n; }
