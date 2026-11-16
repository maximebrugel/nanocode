function main(){ let s = 0; for (let i = 0; i < 4000000; i++) { if (i % 7 === 3) s += i % 13; } return s; }
