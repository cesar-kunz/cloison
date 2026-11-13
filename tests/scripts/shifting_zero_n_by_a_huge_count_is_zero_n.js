// `0n` shifted by any count is `0n`, without a size to exceed.
// expect: 0,0,0,20,-3
[0n << 999999999999n, 0n >> -999999999999n, 0n << -5n, 5n << 2n, -5n >> 1n].join()
