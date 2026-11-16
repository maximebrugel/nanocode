function rng(seed){ let s = seed >>> 0; return function(){ s = (Math.imul(s, 1103515245) + 12345) >>> 0; return s >>> 8; }; }
function main(){ const r = rng(1); const base = []; for (let i = 0; i < 1000; i++) base.push(r() % 100000); let s = 0; for (let k = 0; k < 200; k++){ const a = base.slice(); a.sort((p, q) => p - q); s += a[k % 1000]; } return s; }
