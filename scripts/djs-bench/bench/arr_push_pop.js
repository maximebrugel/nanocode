function main(){ let s = 0; const a = []; for (let r = 0; r < 100; r++){ for (let i = 0; i < 20000; i++) a.push(i); while (a.length) s = (s + a.pop()) | 0; } return s; }
