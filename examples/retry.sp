// Calls a flaky service, backing off between attempts. The waits are durable:
// a crash during a backoff resumes the same attempt, with the same delay left.
fn call_service(attempt) {
    let roll = await random()
    print("attempt {attempt}: rolled {floor(roll * 100)}")
    return roll > 0.7
}

let delay = 0.25
let attempt = 1
while not call_service(attempt) {
    if attempt == 8 {
        error("the service is down, giving up")
    }
    print("  failed, retrying in {delay}s")
    await sleep(delay)
    delay = min(delay * 2, 2.0)
    attempt += 1
}
print("succeeded on attempt {attempt}")
