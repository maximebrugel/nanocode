function main(){ const m = new Map(); for (let i = 0; i < 1000; i++) m.set('k' + i, i); let s = 0; for (let r = 0; r < 1000; r++) for (let i = 0; i < 1000; i += 3) s += m.get('k' + i); return s; }
