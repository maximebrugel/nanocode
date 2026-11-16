class Point { constructor(x, y){ this.x = x; this.y = y; } add(p){ return new Point(this.x + p.x, this.y + p.y); } len2(){ return this.x * this.x + this.y * this.y; } }
class P3 extends Point { constructor(x, y, z){ super(x, y); this.z = z; } }
function main(){ let p = new Point(0, 0); let s = 0; for (let i = 0; i < 500000; i++){ p = p.add(new Point(1, 2)); const q = new P3(i, 1, 2); s += q.z + (p.len2() & 7); } return s + p.x; }
