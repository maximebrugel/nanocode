function main(){ let s = 0; for (let r = 0; r < 200; r++){ let t = ''; for (let i = 0; i < 2000; i++) t += 'ab' + i; s += t.length; } return s; }
