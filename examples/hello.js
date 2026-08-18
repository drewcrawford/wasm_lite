// The simplest possible wasm_lite program: print "hello, world".
//
// Output goes to the devtools console, and the runner forwards it to the
// terminal that ran `cargo run` — the program does not write to the page. It
// used to append to a `<pre>` the runner put in the shell; that `<pre>` was
// removed because a shell that takes layout space displaces the DOM of any
// program that builds its own.
console.log("hello, world");
