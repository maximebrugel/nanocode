function make(d){ return d === 0 ? { l: null, r: null } : { l: make(d - 1), r: make(d - 1) }; }
function check(t){ return t.l === null ? 1 : 1 + check(t.l) + check(t.r); }
function main(){ let s = 0; const keep = make(14); for (let i = 0; i < 40; i++) s += check(make(12)); return s + check(keep); }
