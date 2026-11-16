function main(){ const o = {}; for (let i = 0; i < 30; i++) o['p' + i] = i; let s = 0; for (let r = 0; r < 30000; r++){ for (const k in o) s += o[k]; } return s; }
