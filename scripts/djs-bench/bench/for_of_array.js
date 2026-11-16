function main(){ const a = []; for (let i = 0; i < 1000; i++) a.push(i); let s = 0; for (let r = 0; r < 3000; r++){ for (const v of a) s += v; } return s; }
