function main(){ let s = 0; for (let i = 0; i < 100000; i++){ const e = new Error('x' + (i & 7)); s += e.message.length; } return s; }
