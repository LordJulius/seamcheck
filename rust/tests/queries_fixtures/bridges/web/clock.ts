export function bind(ch: any) {
  ch.on("tick", () => redraw())
}
function redraw() {}
