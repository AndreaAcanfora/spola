// A workflow that waits for a human, for as long as it takes.
//
//   spola run examples/approval.sp --state deploy.state --detach
//   ...hours later, from anywhere...
//   spola resume deploy.state --with yes
fn step(name) {
    print("✓ {name}")
}

let version = "v1.4.2"
step("build {version}")
step("run 412 tests")
let changes = ["new checkout flow", "faster search", "fix login on Safari"]

let answer = await ask("Deploy {version} to production? ({len(changes)} changes)")
if lower(trim(answer)) == "yes" {
    step("deploy {version}")
    let started = await now()
    step("notify #releases")
    return {deployed: version, at: started}
}
print("✗ deploy cancelled: {answer}")
