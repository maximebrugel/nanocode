function get(o){ return o.x; }
function main(){ const os = [{x:1}, {y:2, x:2}, {z:3, y:1, x:3}, {w:0, x:4}]; let s = 0; for (let i = 0; i < 3000000; i++) s += get(os[i & 3]); return s; }
