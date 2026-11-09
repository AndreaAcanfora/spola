// Run it with a checkpoint, kill it halfway, run the same command again:
//
//   spola run examples/countdown.sp --state countdown.state
//
// It picks up at the exact iteration it was on, with every variable intact.
let launched = await now()
let log = []
for n in reverse(range(1, 11)) {
    push(log, n)
    print("T-{n}")
    await sleep(1)
}
let elapsed = ((await now()) - launched) / 1000
print("liftoff after {elapsed}s, counted {len(log)} numbers")
