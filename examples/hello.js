// The simplest possible wasm_lite program: print "hello, world".
// Output goes both to the page (so the browser window shows it) and to the
// devtools console.
const line = "hello, world";

console.log(line);

const output = document.getElementById("output");
if (output) {
    output.textContent += line + "\n";
}
