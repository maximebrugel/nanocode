function f3(a, b, c){ return a + b * c; }
function fr(...xs){ return xs.length; }
function main(){ let s = 0; for (let i = 0; i < 1000000; i++){ s = (s + f3(i, 2, 3) + fr(i, i, i)) | 0; } return s; }
