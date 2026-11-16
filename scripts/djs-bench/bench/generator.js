function* g(n){ for (let i = 0; i < n; i++) yield i; }
function main(){ let s = 0; for (let r = 0; r < 20; r++){ for (const x of g(50000)) s += x; } return s; }
