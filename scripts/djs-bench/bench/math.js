function main(){ let s = 0; for (let i = 1; i < 1000000; i++){ s += Math.floor(Math.sqrt(i)) + Math.abs(Math.sin(i)) + Math.max(i & 7, 3) + Math.min(i, 5); } return Math.round(s); }
