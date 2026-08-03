// A self-contained classic worker: echoes back what it is sent, doubled.
// Linked into the wasm by `link_to!` and served from a blob URL.
onmessage = function (ev) {
    postMessage(ev.data * 2);
};
