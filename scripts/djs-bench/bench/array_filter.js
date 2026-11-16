function main(){ const a = []; for (let i = 0; i < 100; i++) a.push(i); let s = 0; for (let k = 0; k < 40000; k++){ s += a.filter(x => x > 49).length; } return s; }
