# Commands (2026-11-14T22:00:12Z, cwd bench/cli, harness commit 4a4c1190b)
+ tools/netguard.sh node harness/smoke.mjs --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=startup --rounds=100 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=coldstart --coldrounds=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=firstframe --rounds=20 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=coldframe --coldrounds=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=idle --idle=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=keys --keyrounds=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=oneshot --rounds=20 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=tools --steps=10,50 --toolrounds=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/suite.mjs --parts=store --sessions=1000 --storerounds=20 --only=nc,nclite,fx
+ node harness/suite.mjs --parts=disk --only=nc,nclite,fx
+ tools/netguard.sh node harness/sweep.mjs --mode=cli --rates=100,1000,10000 --samples=10 --only=nc,nclite,fx
+ tools/netguard.sh node harness/sweep.mjs --mode=tui --rates=100,1000,10000 --samples=10 --only=nc,nclite,fx
