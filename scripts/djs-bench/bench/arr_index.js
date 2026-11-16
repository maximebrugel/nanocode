function main(){ const a = new Array(10000).fill(0); for (let r = 0; r < 300; r++) for (let i = 0; i < 10000; i++) a[i] = a[i] + i; let s = 0; for (let i = 0; i < 10000; i++) s += a[i]; return s; }
