// Recursion, closures, lists and maps.
fn fib(n) {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}

fn counter() {
    let count = 0
    return fn() {
        count += 1
        return count
    }
}

let next = counter()
next()
next()
print("counter: {next()}")

let squares = []
for i in range(10) {
    push(squares, i * i)
}
print("squares: {squares}")

let person = {name: "Ada", langs: ["en", "it"]}
person.age = 36
print(person)
print("fib(25) = {fib(25)}")
