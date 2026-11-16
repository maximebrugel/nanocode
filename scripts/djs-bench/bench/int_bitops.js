function main(){ let h = 2166136261 | 0; for (let i = 0; i < 3000000; i++){ h ^= i & 255; h = Math.imul(h, 16777619); h = (h << 3) ^ (h >>> 5); } return h; }
