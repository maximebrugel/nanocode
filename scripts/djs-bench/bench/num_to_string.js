function main(){ let s = 0; for (let i = 0; i < 300000; i++){ const t = String(i) + (i * 0.5).toString(); s += t.length + parseInt(t, 10) % 7 + (parseFloat('3.25') | 0); } return s; }
