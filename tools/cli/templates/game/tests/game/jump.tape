# Space jumps once per press.
seed 1
1: press jump
2: expect Player.jumps == 1
3: release jump
4: press jump
5: expect Player.jumps == 2
