// A small assistant that keeps its memory in the program itself: no database.
// Kill the process at any prompt and restore it: the list is still there.
let todos = []
let done = 0

fn show() {
    if len(todos) == 0 {
        print("  (nothing to do)")
        return
    }
    for i in range(len(todos)) {
        print("  {i + 1}. {todos[i]}")
    }
}

print("Commands: add <task>, done <number>, list, quit")
while true {
    let line = trim(await ask("todo>"))
    let words = split(line)
    if len(words) == 0 { continue }
    let cmd = lower(words[0])
    let rest = join(slice(words, 1), " ")
    if cmd == "add" and rest != "" {
        push(todos, rest)
        print("added \"{rest}\"")
    } else if cmd == "done" {
        let n = int(rest) - 1
        if n >= 0 and n < len(todos) {
            print("finished \"{remove(todos, n)}\"")
            done += 1
        } else {
            print("no task {rest}")
        }
    } else if cmd == "list" {
        show()
    } else if cmd == "quit" {
        break
    } else {
        print("try: add buy milk")
    }
}
return {finished: done, left: todos}
