function main(){ const a = []; for (let i = 0; i < 100; i++) a.push(i); let s = 0; for (let k = 0; k < 60000; k++){ const t = k % 100; s += a.findIndex(x => x === t); } return s; }
