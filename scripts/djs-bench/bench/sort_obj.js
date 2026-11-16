function rng(seed){ let s = seed >>> 0; return function(){ s = (Math.imul(s, 1103515245) + 12345) >>> 0; return s >>> 8; }; }
function main(){ const r = rng(3); const base = []; for (let i = 0; i < 1000; i++) base.push({ k: r() % 10000, i }); let s = 0; for (let k = 0; k < 100; k++){ const a = base.slice(); a.sort((p, q) => p.k < q.k ? -1 : p.k > q.k ? 1 : 0); s += a[k].i; } return s; }
