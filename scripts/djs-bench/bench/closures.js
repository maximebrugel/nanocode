function make(k){ let c = 0; return function(x){ c += x + k; return c; }; }
function main(){ let s = 0; for (let j = 0; j < 200; j++){ const f = make(j); for (let i = 0; i < 10000; i++) s = (s + f(i)) | 0; } return s; }
